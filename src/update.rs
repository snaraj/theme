//! `theme update` — fetch the latest GitHub release and install it over the
//! running binary, the way gh/rustup/bun self-update.
//!
//! Trust model: this is the OWNER'S OWN repo over TLS — a trusted
//! destination class, unlike the page-controlled og:image hop. The host set
//! is GitHub's and nothing else: the API answers directly, and the asset
//! download is one observed redirect, github.com →
//! release-assets.githubusercontent.com (chain observed 2026-09-01;
//! objects.githubusercontent.com kept as the documented previous asset
//! host). Redirects are walked MANUALLY — curl never follows one on its
//! own (`--max-redirs 0` everywhere) — so every hop's host is vetted
//! against that allowlist before it is fetched, and `--proto '=https'`
//! refuses any protocol downgrade. Integrity is SHA256SUMS + TLS; there is
//! deliberately no signature infra beyond that (owner's repo, and
//! required_signatures/cosign gold-plating was ruled out platform-side).
//!
//! The install invariant: THE RUNNING BINARY IS NEVER REPLACED BY
//! UNVERIFIED BYTES. The release asset is a tar.gz holding the single
//! member `theme` (the release workflow's naming: SHA256SUMS digests the
//! TARBALLS). The download lands in the 0700 scratch directory, its digest
//! is stream-verified in-process against SHA256SUMS BEFORE tar ever sees
//! it, and the member is then streamed straight into an O_CREAT|O_EXCL
//! 0755 temp file in the target's OWN directory (same filesystem) — no
//! intermediate extracted file — with the atomic rename(2) only after a
//! clean unpack and fsync, so the target is never partial. Between verify
//! and unpack the tarball sits under the 0700 scratch parent — the same
//! no-other-principal custody every save relies on. A failed run unlinks
//! the temp and leaves the target untouched. An unwritable target
//! directory is a clear error — never sudo.

use crate::config::{Config, MAX_DOWNLOAD_BYTES, UA};
use crate::json::Json;
use crate::net::{curl_config, url_host};
use crate::scratch;
use crate::ui::{die, display_text};
use rustix::fs::{Mode, OFlags};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const RELEASES_API: &str = "https://api.github.com/repos/snaraj/theme/releases";
/// How long a footer-note check result stays fresh. One bounded, silent
/// refresh attempt per window, shared with `theme update` through the same
/// cache file.
const CHECK_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Every host an asset download may touch: the release-download URL itself
/// plus GitHub's asset CDN (current, and the previous one it may still
/// answer from). Any other Location refuses before curl is even spawned.
const ASSET_HOSTS: [&str; 3] = [
    "github.com",
    "release-assets.githubusercontent.com",
    "objects.githubusercontent.com",
];
/// The release JSON and SHA256SUMS are small; cap them far below the
/// binary cap so a wrong asset cannot masquerade as either.
const API_CAP: u64 = 1024 * 1024;
const SUMS_CAP: u64 = 64 * 1024;

/// The published release targets. A platform outside this set gets an
/// honest "no published build" instead of a guessed asset name.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TARGET: &str = "aarch64-apple-darwin";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const TARGET: &str = "x86_64-apple-darwin";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const TARGET: &str = "aarch64-unknown-linux-gnu";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const TARGET: &str = "x86_64-unknown-linux-gnu";
#[cfg(not(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64"),
)))]
const TARGET: &str = "";

pub fn cmd_update(cfg: &Config, want: &str) {
    if TARGET.is_empty() {
        die("no published release build for this platform — build from source");
    }
    let current = env!("CARGO_PKG_VERSION");
    // --version: STRICT shape validation BEFORE the string can touch a URL
    // path, and the downgrade warning before any network — a warning, not a
    // prompt.
    let want_tag = if want.is_empty() {
        None
    } else {
        let (v, canon) = parse_ver_arg(want)
            .unwrap_or_else(|| die("--version takes a release version like v0.1.0"));
        if let Some(cur) = parse_v3(&format!("v{current}"))
            && v < cur
        {
            eprintln!(
                "warning: older versions may be unsupported or break — proceeding to {canon}"
            );
        }
        Some(canon)
    };
    let url = match &want_tag {
        Some(t) => format!("{RELEASES_API}/tags/{t}"),
        None => format!("{RELEASES_API}/latest"),
    };
    let json = fetch_release(&url, "30").unwrap_or_else(|| match &want_tag {
        Some(t) => die(&format!(
            "no release {t} — see https://github.com/snaraj/theme/releases (or the request failed)"
        )),
        None => die("cannot reach the GitHub release API (no network or no release yet)"),
    });

    let tag = json
        .str_field("tag_name")
        .filter(|t| tag_shape_ok(t))
        .map(str::to_string)
        .unwrap_or_else(|| die("release has no usable tag"));
    if want_tag.is_none() {
        // Only a LATEST answer refreshes the footer-note cache — one source
        // of truth shared with the update-available check. A --version fetch
        // must not: stamping an older tag would hide the real latest.
        write_check(cfg, &tag);
        if tag.trim_start_matches('v') == current {
            println!("already up to date (v{current})");
            return;
        }
    }

    // SHA256SUMS is the ground truth for both the asset NAME (the unique
    // entry naming this target triple) and its digest — so the asset-naming
    // scheme lives in the release chain, not here.
    let sums_url = asset_url(&json, "SHA256SUMS")
        .unwrap_or_else(|| die("release publishes no SHA256SUMS — refusing an unverifiable build"));
    let sums_file = scratch::new();
    fetch_asset(&sums_url, &sums_file, SUMS_CAP)
        .unwrap_or_else(|e| die(&format!("SHA256SUMS download failed: {e}")));
    let sums = std::fs::read_to_string(&sums_file)
        .unwrap_or_else(|_| die("SHA256SUMS is not readable text"));
    scratch::done(&sums_file);
    let (expect_hex, asset_name) =
        pick_from_sums(&sums, TARGET).unwrap_or_else(|e| die(&display_text(&e)));

    let bin_url = asset_url(&json, &asset_name).unwrap_or_else(|| {
        die(&display_text(&format!(
            "SHA256SUMS names '{asset_name}' but the release has no such asset"
        )))
    });
    let staged = scratch::new();
    fetch_asset(&bin_url, &staged, MAX_DOWNLOAD_BYTES)
        .unwrap_or_else(|e| die(&format!("release download failed: {e}")));
    let got = std::fs::metadata(&staged).map(|m| m.len()).unwrap_or(0);
    if got == 0 || got > MAX_DOWNLOAD_BYTES {
        die("release download is empty or over the byte cap");
    }

    verify_file(&staged, &expect_hex).unwrap_or_else(|e| die(&e));
    let target = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .unwrap_or_else(|_| die("cannot resolve the running binary's path"));
    install_over(&target, &staged).unwrap_or_else(|e| die(&e));
    scratch::done(&staged);
    println!("theme v{current} → {tag}");
    println!("updated: {}", display_text(&target.display().to_string()));
}

/// One release-API request: hardened flags, bounded size, parsed JSON.
/// `max_time` is the caller's latency budget — 30s for the explicit
/// `theme update`, 2s for the silent footer-note refresh.
fn fetch_release(url: &str, max_time: &str) -> Option<Json> {
    let body = curl_config(
        "header = \"Accept: application/vnd.github+json\"\nheader = \"X-GitHub-Api-Version: 2022-11-28\"\n",
        &[
            "-fsg",
            "--proto",
            "=https",
            "--max-redirs",
            "0",
            "--max-filesize",
            "1048576",
            "--max-time",
            max_time,
            "-A",
            UA,
            "-K",
            "-",
            "--url",
            url,
        ],
    )?;
    if body.len() as u64 > API_CAP {
        return None;
    }
    String::from_utf8(body).ok().and_then(|s| Json::parse(&s))
}

/// The update-available footer on the bare `theme` screen. Silent on every
/// failure mode — offline, rate-limited, bad JSON, malformed cache — and
/// printed ONLY when the cached latest is strictly newer than this build.
/// THEME_NO_UPDATE_CHECK (non-empty) disables the check and the note.
///
/// Speed contract: the note reads one small cache file. At most one
/// bounded refresh (2s hard cap) runs per [`CHECK_TTL`] window, and the
/// attempt is stamped even on failure so an offline machine pays it once
/// per window, not per run. Every cache touch — read and stamp alike —
/// goes through [`check_dir`]'s fail-closed custody.
pub fn maybe_note(cfg: &Config) {
    let off = std::env::var("THEME_NO_UPDATE_CHECK")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    if off {
        return;
    }
    // Custody first: a cache dir that fails the fail-closed audit gets no
    // read, no stamp, no note — and no network attempt either.
    let Some(dirfd) = check_dir(cfg) else { return };
    let (fresh, mut cached) = read_check(&dirfd);
    if !fresh {
        let tag = fetch_release(&format!("{RELEASES_API}/latest"), "2")
            .and_then(|j| {
                j.str_field("tag_name")
                    .filter(|t| tag_shape_ok(t))
                    .map(str::to_string)
            })
            .unwrap_or_default();
        write_check_at(&dirfd, &tag);
        cached = tag;
    }
    // The cached tag is REMOTE data headed for a terminal: it renders only
    // if it parses as a strict numeric semver triple, and both printed
    // values are RECONSTRUCTED from the parsed numbers — a remote-supplied
    // string or URL is never echoed.
    if let Some((a, b, c)) = newer_than(cached.trim(), env!("CARGO_PKG_VERSION")) {
        println!(
            "\nupdate to the latest theme version: v{a}.{b}.{c} -> https://github.com/snaraj/theme/releases/tag/v{a}.{b}.{c}"
        );
        println!("to update run: theme update");
    }
}

/// Fail-closed custody of the cache directory (Codex round 4): the check
/// cache is only touched through a directory fd whose fstat proves the dir
/// is OURS and carries no group/world write bit — audited, never chmodded;
/// created 0700 when absent. A dir that fails the audit gets NOTHING: no
/// stamp, no read, no note — silence, exactly like every other failure
/// mode here. O_NOFOLLOW on the open kills a symlink planted AT the dir
/// name; the uid check kills an attacker-owned real dir swapped in through
/// a writable ancestor (they cannot create one owned as us).
fn check_dir(cfg: &Config) -> Option<rustix::fd::OwnedFd> {
    let _ = rustix::fs::mkdir(&cfg.cache_dir, Mode::from_raw_mode(0o700));
    let fd = rustix::fs::open(
        &cfg.cache_dir,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .ok()?;
    let st = rustix::fs::fstat(&fd).ok()?;
    if st.st_uid != rustix::process::getuid().as_raw() || (st.st_mode as u32) & 0o022 != 0 {
        return None;
    }
    Some(fd)
}

/// Stamp the check cache: content is the latest known tag (empty when the
/// refresh failed — stamping the failed attempt is the deliberate
/// anti-thundering choice, one bounded try per TTL window), mtime is the
/// attempt time. Custody-gated by [`check_dir`].
fn write_check(cfg: &Config, tag: &str) {
    if let Some(dirfd) = check_dir(cfg) {
        write_check_at(&dirfd, tag);
    }
}

/// The stamp itself, descriptor-bound like the installer: the temp is
/// created O_CREAT|O_EXCL|O_NOFOLLOW 0600 THROUGH the audited dirfd (the
/// pid-predictable name is harmless once no other principal can write the
/// directory), then renameat replaces the final name — only ever renaming
/// a file THIS process opened, and replacing whatever entry sat there
/// (symlink, FIFO) rather than following it.
fn write_check_at(dirfd: &rustix::fd::OwnedFd, tag: &str) {
    let tmp = format!(".update-check.{}", std::process::id());
    let _ = rustix::fs::unlinkat(dirfd, tmp.as_str(), rustix::fs::AtFlags::empty());
    let Ok(fd) = rustix::fs::openat(
        dirfd,
        tmp.as_str(),
        OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW,
        Mode::from_raw_mode(0o600),
    ) else {
        return;
    };
    let mut f = File::from(fd);
    if f.write_all(tag.as_bytes()).is_err() {
        drop(f);
        let _ = rustix::fs::unlinkat(dirfd, tmp.as_str(), rustix::fs::AtFlags::empty());
        return;
    }
    drop(f);
    if rustix::fs::renameat(dirfd, tmp.as_str(), dirfd, "update-check").is_err() {
        let _ = rustix::fs::unlinkat(dirfd, tmp.as_str(), rustix::fs::AtFlags::empty());
    }
}

/// The read side of the same custody: the cache file opens through the
/// audited dirfd with O_NOFOLLOW (a planted symlink refuses) and
/// O_NONBLOCK (a planted FIFO returns instead of hanging — the save
/// path's guard), must fstat as a REGULAR file, and is read through a
/// 256-byte cap. Freshness comes from the opened fd's own metadata, never
/// a path re-walk. Returns (fresh, content); anything irregular is
/// (stale, empty) — silent, and healed by the next stamp.
fn read_check(dirfd: &rustix::fd::OwnedFd) -> (bool, String) {
    let Ok(fd) = rustix::fs::openat(
        dirfd,
        "update-check",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    ) else {
        return (false, String::new());
    };
    let f = File::from(fd);
    let Ok(md) = f.metadata() else {
        return (false, String::new());
    };
    if !md.file_type().is_file() {
        return (false, String::new());
    }
    let fresh = md
        .modified()
        .map(|t| match std::time::SystemTime::now().duration_since(t) {
            Ok(age) => age <= CHECK_TTL,
            // A future mtime reads as fresh, not stale — the failure mode
            // of a skewed clock must be silence, never a hot loop.
            Err(_) => true,
        })
        .unwrap_or(false);
    let mut s = String::new();
    if f.take(256).read_to_string(&mut s).is_err() {
        return (fresh, String::new());
    }
    (fresh, s)
}

/// Strict numeric semver: `v<major>.<minor>.<patch>`, ASCII digits only —
/// no prerelease, no build metadata, nothing that could smuggle bytes into
/// a URL or the terminal.
fn parse_v3(s: &str) -> Option<(u64, u64, u64)> {
    let rest = s.strip_prefix('v')?;
    let mut parts = rest.split('.');
    let mut next = || {
        parts
            .next()
            .filter(|p| !p.is_empty() && p.len() <= 10 && p.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|p| p.parse::<u64>().ok())
    };
    let v = (next()?, next()?, next()?);
    parts.next().is_none().then_some(v)
}

/// The remote triple, if it is strictly newer than the running build —
/// equal and older (dev build) both answer None, so no note renders.
fn newer_than(cached: &str, current: &str) -> Option<(u64, u64, u64)> {
    let r = parse_v3(cached)?;
    let c = parse_v3(&format!("v{current}"))?;
    (r > c).then_some(r)
}

/// A user-supplied `--version` value: `vX.Y.Z` or `X.Y.Z`, normalized to
/// the canonical tag. Anything else — including path shapes — refuses
/// before the string can reach a URL.
fn parse_ver_arg(s: &str) -> Option<((u64, u64, u64), String)> {
    let bare = s.strip_prefix('v').unwrap_or(s);
    let v = parse_v3(&format!("v{bare}"))?;
    Some((v, format!("v{}.{}.{}", v.0, v.1, v.2)))
}

/// Release tags are API data: `v` + a short run of version characters,
/// nothing else reaches a message or a comparison.
fn tag_shape_ok(t: &str) -> bool {
    t.len() <= 64
        && t.starts_with('v')
        && t.len() > 1
        && t[1..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// The download URL for the release asset named exactly `name` — and it
/// must live under this repo's own release path; anything else in the API
/// answer is refused, not followed.
fn asset_url(release: &Json, name: &str) -> Option<String> {
    let assets = match release.get("assets") {
        Some(Json::Arr(a)) => a,
        _ => return None,
    };
    let url = assets
        .iter()
        .find(|a| a.str_field("name") == Some(name))
        .and_then(|a| a.str_field("browser_download_url"))?;
    url.starts_with("https://github.com/snaraj/theme/releases/download/")
        .then(|| url.to_string())
}

/// The unique SHA256SUMS entry whose filename names `target`. Lines are
/// `<64-hex>  <name>` (` *name` binary-marker form accepted). Zero or
/// multiple matches refuse — a release that cannot name this platform
/// unambiguously does not get installed.
fn pick_from_sums(sums: &str, target: &str) -> Result<(String, String), String> {
    let mut hit: Option<(String, String)> = None;
    for line in sums.lines() {
        let line = line.trim_end_matches('\r');
        let (hex, name) = match line.split_once(' ') {
            Some(t) => t,
            None => continue,
        };
        let name = name.trim_start().trim_start_matches('*');
        if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        if name.is_empty() || name.contains('/') || !name.contains(target) {
            continue;
        }
        if hit.is_some() {
            return Err(format!("SHA256SUMS names more than one {target} asset"));
        }
        hit = Some((hex.to_ascii_lowercase(), name.to_string()));
    }
    hit.ok_or_else(|| format!("SHA256SUMS has no entry for {target}"))
}

/// First line + Location of a `curl -D` header dump.
fn parse_head(hdr: &str) -> (u16, Option<String>) {
    let mut lines = hdr.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let loc = lines
        .map(|l| l.trim_end_matches('\r'))
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("location")
                .then(|| v.trim().to_string())
        })
        .filter(|v| !v.is_empty());
    (status, loc)
}

fn hop_host_ok(url: &str) -> bool {
    url.starts_with("https://")
        && url_host(url)
            .map(|h| ASSET_HOSTS.contains(&h.as_str()))
            .unwrap_or(false)
}

/// Fetch a release asset with every redirect walked by hand: each hop runs
/// curl with `--max-redirs 0`, and a Location only gets fetched if its host
/// is on [`ASSET_HOSTS`] and its scheme is https. Two hops is one more than
/// the observed chain needs.
fn fetch_asset(url: &str, dest: &Path, cap: u64) -> Result<(), String> {
    let mut here = url.to_string();
    for _ in 0..=2 {
        if !hop_host_ok(&here) {
            return Err(format!(
                "refusing non-GitHub download host '{}'",
                display_text(&url_host(&here).unwrap_or_default())
            ));
        }
        let hdr = scratch::new();
        let ok = Command::new("curl")
            .args([
                "-sg",
                "--proto",
                "=https",
                "--max-redirs",
                "0",
                "--max-filesize",
                &cap.to_string(),
                "--max-time",
                "300",
                "-A",
                UA,
                "-D",
            ])
            .arg(&hdr)
            .arg("-o")
            .arg(dest)
            .arg("--url")
            .arg(&here)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        let head = std::fs::read_to_string(&hdr).unwrap_or_default();
        scratch::done(&hdr);
        if !ok {
            return Err("transfer failed or exceeded its byte cap".into());
        }
        let (status, loc) = parse_head(&head);
        match status {
            200 => return Ok(()),
            301 | 302 | 303 | 307 | 308 => {
                here = loc.ok_or("redirect without a Location")?;
            }
            s => return Err(format!("unexpected HTTP status {s}")),
        }
    }
    Err("redirect chain too long".into())
}

/// Stream-hash a downloaded file and compare against its SHA256SUMS entry.
/// Runs BEFORE the archive is unpacked — unverified bytes never reach tar,
/// let alone the install path.
fn verify_file(path: &Path, expect_hex: &str) -> Result<(), String> {
    let mut src = File::open(path).map_err(|e| format!("cannot reread the download: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        match src.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) => return Err(format!("read failed during verification: {e}")),
        }
    }
    let got: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if got != expect_hex.to_ascii_lowercase() {
        return Err(
            "SHA256 verification FAILED — the download does not match SHA256SUMS; nothing was installed"
                .into(),
        );
    }
    Ok(())
}

/// A reader that refuses to yield more than `left` bytes in total — the
/// ceiling on everything consumed FROM THE DECOMPRESSOR, the
/// decompression-bomb backstop behind the per-member header check.
struct Capped<R> {
    inner: R,
    left: u64,
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            return Err(std::io::Error::other("decompressed byte cap exceeded"));
        }
        let want = buf.len().min(self.left as usize);
        let n = self.inner.read(&mut buf[..want])?;
        self.left -= n as u64;
        Ok(n)
    }
}

/// Octal field of a tar header: leading spaces/NULs tolerated, digits,
/// terminated by space/NUL. Anything else — including GNU base-256 — is
/// None.
fn parse_octal(field: &[u8]) -> Option<u64> {
    let mut v: u64 = 0;
    let mut seen = false;
    for &b in field {
        match b {
            b'0'..=b'7' => {
                seen = true;
                v = v.checked_mul(8)?.checked_add((b - b'0') as u64)?;
            }
            b' ' | 0 => {
                if seen {
                    break;
                }
            }
            _ => return None,
        }
    }
    seen.then_some(v)
}

/// The strict single-member tar walk, in-process — NOTHING PATH-resolved
/// touches the verified-bytes-to-installed-bytes chain (the round-2
/// finding that moved extraction in-house; the same principle that picked
/// sha2 over a shelled shasum). The release chain ships exactly one shape,
/// probed on the real v0.1.0 asset (bsdtar- and GNU-built alike): one
/// plain ustar header named `theme`, typeflag '0', empty prefix, data,
/// zero padding, zero end blocks. This reader accepts exactly that:
///   - EXACTLY one member — a regular file named `theme` at the root;
///   - duplicates, links, directories, pax/GNU extension entries, long
///     names, base-256 sizes: refused (a duplicate is a nonzero byte where
///     only zeros may remain, so concatenation is impossible by
///     construction);
///   - the header checksum must verify, the member size must fit `cap`,
///     and every padding/trailing byte to EOF must be zero.
///
/// Streams the member into `out` and returns its byte count. The caller
/// wraps `r` in [`Capped`], which bounds TOTAL decompressed consumption.
fn unpack_single_member<R: Read>(r: &mut R, out: &mut impl Write, cap: u64) -> Result<u64, String> {
    let shape =
        |what: &str| format!("the verified archive is not a single 'theme' binary ({what})");
    let mut hdr = [0u8; 512];
    let mut filled = 0usize;
    while filled < 512 {
        let n = r
            .read(&mut hdr[filled..])
            .map_err(|e| format!("read failed mid-unpack: {e}"))?;
        if n == 0 {
            return Err(shape("truncated archive"));
        }
        filled += n;
    }
    if hdr.iter().all(|&b| b == 0) {
        return Err(shape("empty archive"));
    }
    let stored = parse_octal(&hdr[148..156]).ok_or_else(|| shape("unreadable header checksum"))?;
    let sum: u64 = hdr
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                0x20
            } else {
                b as u64
            }
        })
        .sum();
    if sum != stored {
        return Err(shape("header checksum mismatch"));
    }
    let name_end = hdr[..100].iter().position(|&b| b == 0).unwrap_or(100);
    if &hdr[..name_end] != b"theme" {
        return Err(shape("member is not 'theme'"));
    }
    // A ustar prefix would relocate the member into a subdirectory.
    if hdr[345..500].iter().any(|&b| b != 0) {
        return Err(shape("member is not at the archive root"));
    }
    if hdr[156] != b'0' && hdr[156] != 0 {
        return Err(shape("member is not a regular file"));
    }
    let size = parse_octal(&hdr[124..136]).ok_or_else(|| shape("unreadable member size"))?;
    if size > cap {
        return Err(shape("member exceeds the byte cap"));
    }
    let mut left = size;
    let mut buf = [0u8; 64 * 1024];
    while left > 0 {
        let want = buf.len().min(left as usize);
        let n = r
            .read(&mut buf[..want])
            .map_err(|e| format!("read failed mid-unpack: {e}"))?;
        if n == 0 {
            return Err(shape("truncated member data"));
        }
        out.write_all(&buf[..n])
            .map_err(|e| format!("write failed mid-install: {e}"))?;
        left -= n as u64;
    }
    // Block padding, end-of-archive marker, blocking-factor padding: all of
    // it must be zeros to EOF. A second member's header is a nonzero byte
    // here — duplicates refuse instead of concatenating.
    loop {
        let n = r
            .read(&mut buf)
            .map_err(|e| format!("read failed mid-unpack: {e}"))?;
        if n == 0 {
            return Ok(size);
        }
        if buf[..n].iter().any(|&b| b != 0) {
            return Err(shape("trailing entries after the member"));
        }
    }
}

/// Unpack-then-rename. The release asset is a tar.gz holding the single
/// member `theme` (the release workflow's `tar -C … theme`); its digest was
/// verified against SHA256SUMS before this runs, and between verification
/// and unpack it sits in the 0700 scratch directory — the same
/// no-other-principal custody every save in this tool relies on. The
/// gzip layer is in-process (flate2, pure Rust) and the tar layer is
/// [`unpack_single_member`], which streams the member STRAIGHT into a
/// fresh temp file opened O_CREAT|O_EXCL 0755 in the TARGET'S own
/// directory (same filesystem) — no intermediate file, no external
/// process; only a clean strict-shape unpack with non-empty output earns
/// fsync + the atomic rename(2) over the target. Any failure unlinks the
/// temp and the target is untouched — there is no window where it is
/// partial or unverified.
fn install_over(target: &Path, tarball: &Path) -> Result<(), String> {
    let dir = target.parent().ok_or("install target has no directory")?;
    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("install target has no name")?;
    let dirfd = rustix::fs::open(dir, OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty())
        .map_err(|e| format!("cannot open {}: {e}", dir.display()))?;
    let tmp_name = format!(".{name}.update.{}", std::process::id());
    let tmp = rustix::fs::openat(
        &dirfd,
        tmp_name.as_str(),
        OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW,
        Mode::from_raw_mode(0o755),
    )
    .map_err(|e| {
        format!(
            "cannot write {} ({e}) — theme never elevates; install it somewhere you own",
            dir.display()
        )
    })?;
    let mut tmp = File::from(tmp);
    // Unlink the temp NAME; the open fd closes when `tmp` drops on return.
    let sweep = || {
        let _ = rustix::fs::unlinkat(&dirfd, tmp_name.as_str(), rustix::fs::AtFlags::empty());
    };

    let src = match File::open(tarball) {
        Ok(f) => f,
        Err(e) => {
            sweep();
            return Err(format!("cannot reread the download: {e}"));
        }
    };
    let mut capped = Capped {
        inner: flate2::read::GzDecoder::new(src),
        left: MAX_DOWNLOAD_BYTES,
    };
    let total = match unpack_single_member(&mut capped, &mut tmp, MAX_DOWNLOAD_BYTES) {
        Ok(n) => n,
        Err(e) => {
            sweep();
            return Err(e);
        }
    };
    if total == 0 {
        sweep();
        return Err("the verified archive is not a single 'theme' binary (empty member)".into());
    }
    if let Err(e) = tmp.sync_all() {
        sweep();
        return Err(format!("fsync failed: {e}"));
    }
    drop(tmp);
    // rename(2), NEVER an in-place copy: macOS caches code-signing state by
    // vnode, and overwriting a previously-executed binary's inode poisons
    // it — the next exec dies SIGKILL. The rename swaps the directory entry
    // to a fresh inode (atomic, and the running image keeps its old vnode).
    rustix::fs::renameat(&dirfd, tmp_name.as_str(), &dirfd, name).map_err(|e| {
        let _ = rustix::fs::unlinkat(&dirfd, tmp_name.as_str(), rustix::fs::AtFlags::empty());
        format!("cannot replace {}: {e}", target.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // The release chain's STABLE asset names (no version infix — #14 round
    // 3 dropped the publish-job rename so latest/download URLs stay live).
    const SUMS: &str = "\
0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  theme-aarch64-apple-darwin.tar.gz\n\
fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210 *theme-x86_64-unknown-linux-gnu.tar.gz\n";

    #[test]
    fn sums_selection_is_unique_and_shape_checked() {
        let (hex, name) = pick_from_sums(SUMS, "aarch64-apple-darwin").unwrap();
        assert_eq!(name, "theme-aarch64-apple-darwin.tar.gz");
        assert!(hex.starts_with("0123"));
        // The `*` binary-marker form parses too.
        let (_, name) = pick_from_sums(SUMS, "x86_64-unknown-linux-gnu").unwrap();
        assert_eq!(name, "theme-x86_64-unknown-linux-gnu.tar.gz");
        // No entry and ambiguous entries both refuse.
        assert!(pick_from_sums(SUMS, "riscv64gc-unknown-none").is_err());
        let dup = format!("{SUMS}{SUMS}");
        assert!(pick_from_sums(&dup, "aarch64-apple-darwin").is_err());
        // A short or non-hex digest never selects.
        assert!(
            pick_from_sums("abc  theme-aarch64-apple-darwin\n", "aarch64-apple-darwin").is_err()
        );
    }

    #[test]
    fn header_parse_reads_status_and_location() {
        let (s, l) = parse_head(
            "HTTP/2 302 \r\nlocation: https://release-assets.githubusercontent.com/x\r\n\r\n",
        );
        assert_eq!(s, 302);
        assert_eq!(
            l.as_deref(),
            Some("https://release-assets.githubusercontent.com/x")
        );
        let (s, l) = parse_head("HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n");
        assert_eq!(s, 200);
        assert!(l.is_none());
        assert_eq!(parse_head("").0, 0);
    }

    #[test]
    fn only_github_asset_hosts_pass_the_hop_gate() {
        assert!(hop_host_ok(
            "https://github.com/snaraj/theme/releases/download/v1/x"
        ));
        assert!(hop_host_ok(
            "https://release-assets.githubusercontent.com/prod/abc?sig=1"
        ));
        assert!(hop_host_ok("https://objects.githubusercontent.com/abc"));
        assert!(!hop_host_ok("https://evil.invalid/x"));
        assert!(!hop_host_ok("https://github.com.evil.invalid/x"));
        assert!(!hop_host_ok("http://github.com/downgrade")); // https only
        assert!(!hop_host_ok("https://user@evil.invalid/x@github.com"));
    }

    #[test]
    fn tag_shapes() {
        assert!(tag_shape_ok("v0.1.0"));
        assert!(tag_shape_ok("v1.2.3-rc.1"));
        assert!(!tag_shape_ok("0.1.0"));
        assert!(!tag_shape_ok("v"));
        assert!(!tag_shape_ok("v0.1.0\x1b]52;x\x07"));
        assert!(!tag_shape_ok("v0.1.0/../../x"));
    }

    #[test]
    fn strict_semver_triples_only() {
        assert_eq!(parse_v3("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_v3("v0.0.0"), Some((0, 0, 0)));
        assert!(parse_v3("1.2.3").is_none()); // v required here
        assert!(parse_v3("v1.2").is_none());
        assert!(parse_v3("v1.2.3.4").is_none());
        assert!(parse_v3("v1.2.3-rc.1").is_none());
        assert!(parse_v3("v1.2.x").is_none());
        assert!(parse_v3("v1.2.\u{1b}]52;c;x\u{7}3").is_none());
        assert!(parse_v3("v99999999999999999999.0.0").is_none()); // overflow
    }

    #[test]
    fn the_note_gate_fires_only_on_strictly_newer() {
        assert_eq!(newer_than("v9.9.9", "0.0.1"), Some((9, 9, 9)));
        assert_eq!(newer_than("v0.0.1", "0.0.1"), None); // equal
        assert_eq!(newer_than("v0.0.0", "0.0.1"), None); // dev build newer
        assert_eq!(newer_than("", "0.0.1"), None); // no data
        assert_eq!(newer_than("v9.9.9junk", "0.0.1"), None); // malformed
        assert_eq!(newer_than("v0.10.0", "0.9.9"), Some((0, 10, 0))); // numeric, not lexical
    }

    #[test]
    fn version_arg_normalizes_or_refuses() {
        assert_eq!(
            parse_ver_arg("0.1.0"),
            Some(((0, 1, 0), "v0.1.0".to_string()))
        );
        assert_eq!(
            parse_ver_arg("v0.1.0"),
            Some(((0, 1, 0), "v0.1.0".to_string()))
        );
        // Path shapes, flags, and junk refuse BEFORE any URL is built.
        assert!(parse_ver_arg("../../evil").is_none());
        assert!(parse_ver_arg("v0.1.0/../x").is_none());
        assert!(parse_ver_arg("latest").is_none());
        assert!(parse_ver_arg("").is_none());
        assert!(parse_ver_arg("v0.1.0-rc.1").is_none());
    }

    fn sha_hex(b: &[u8]) -> String {
        Sha256::digest(b)
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect()
    }

    /// A tar.gz shaped like the release asset: one member, named `member`,
    /// holding `bytes`. Built from raw blocks — a LOCAL macOS tar would add
    /// AppleDouble/pax entries for this machine's provenance xattrs, which
    /// the strict walker rightly refuses; the shipped shape (probed on the
    /// real v0.1.0 asset) is the plain single-entry form raw_tar emits.
    fn tarball(dir: &Path, member: &str, bytes: &[u8]) -> std::path::PathBuf {
        let tb = dir.join("asset.tar.gz");
        let mut enc =
            flate2::write::GzEncoder::new(File::create(&tb).unwrap(), flate2::Compression::fast());
        enc.write_all(&raw_tar(&[(member, bytes, b'0')])).unwrap();
        enc.finish().unwrap();
        tb
    }

    #[test]
    fn verification_is_exact_and_case_insensitive() {
        let d = tempdir();
        let f = d.join("dl");
        std::fs::write(&f, b"RELEASE").unwrap();
        assert!(verify_file(&f, &sha_hex(b"RELEASE")).is_ok());
        assert!(verify_file(&f, &sha_hex(b"RELEASE").to_ascii_uppercase()).is_ok());
        let err = verify_file(&f, &sha_hex(b"tampered")).unwrap_err();
        assert!(err.contains("verification FAILED"), "got: {err}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn install_unpacks_the_member_atomically() {
        let d = tempdir();
        let target = d.join("theme");
        std::fs::write(&target, b"OLD").unwrap();
        let tb = tarball(&d, "theme", b"NEW-BINARY-BYTES");
        install_over(&target, &tb).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"NEW-BINARY-BYTES");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&target).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
        assert_eq!(dotfiles_beside(&d), 0, "no temp survives success");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_archive_without_the_binary_is_refused() {
        let d = tempdir();
        let target = d.join("theme");
        std::fs::write(&target, b"OLD").unwrap();
        let tb = tarball(&d, "not-theme", b"WRONG-MEMBER");
        let err = install_over(&target, &tb).unwrap_err();
        assert!(err.contains("not a single 'theme' binary"), "got: {err}");
        assert_eq!(std::fs::read(&target).unwrap(), b"OLD");
        assert_eq!(dotfiles_beside(&d), 0, "the temp was swept");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Hand-rolled tar bytes — the hostile shapes no real tar will write.
    fn raw_header(name: &str, size: u64, typ: u8) -> [u8; 512] {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        h[156] = typ;
        let sum: u64 = h
            .iter()
            .enumerate()
            .map(|(i, &b)| {
                if (148..156).contains(&i) {
                    0x20
                } else {
                    b as u64
                }
            })
            .sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        h
    }

    fn raw_tar(entries: &[(&str, &[u8], u8)]) -> Vec<u8> {
        let mut t = Vec::new();
        for (name, data, typ) in entries {
            t.extend_from_slice(&raw_header(name, data.len() as u64, *typ));
            t.extend_from_slice(data);
            t.resize(t.len().div_ceil(512) * 512, 0);
        }
        t.resize(t.len() + 1024, 0);
        t
    }

    fn walk(tar: &[u8], cap: u64) -> Result<(u64, Vec<u8>), String> {
        let mut out = Vec::new();
        let mut capped = Capped {
            inner: std::io::Cursor::new(tar),
            left: cap,
        };
        unpack_single_member(&mut capped, &mut out, cap).map(|n| (n, out))
    }

    #[test]
    fn the_walker_accepts_exactly_the_shipped_shape() {
        let (n, out) = walk(&raw_tar(&[("theme", b"BINARY-BYTES", b'0')]), 1 << 20).unwrap();
        assert_eq!(n, 12);
        assert_eq!(out, b"BINARY-BYTES");
        // Old-style regular typeflag (NUL) is the one tolerated variant.
        assert!(walk(&raw_tar(&[("theme", b"X", 0)]), 1 << 20).is_ok());
    }

    #[test]
    fn duplicate_members_refuse_not_concatenate() {
        let err = walk(
            &raw_tar(&[("theme", b"FIRST", b'0'), ("theme", b"SECOND", b'0')]),
            1 << 20,
        )
        .unwrap_err();
        assert!(err.contains("trailing entries"), "got: {err}");
    }

    #[test]
    fn non_regular_and_extension_members_refuse() {
        for typ in *b"125xgL" {
            let err = walk(&raw_tar(&[("theme", b"X", typ)]), 1 << 20).unwrap_err();
            assert!(err.contains("not a regular file"), "typ {typ}: {err}");
        }
    }

    #[test]
    fn foreign_names_and_prefixes_refuse() {
        for name in ["notme", "./theme", "theme2", "a/theme"] {
            let err = walk(&raw_tar(&[(name, b"X", b'0')]), 1 << 20).unwrap_err();
            assert!(err.contains("is not 'theme'"), "name {name}: {err}");
        }
        let mut t = raw_tar(&[("theme", b"X", b'0')]);
        t[345] = b'a'; // ustar prefix relocates the member — recompute sum
        let sum: u64 = t[..512]
            .iter()
            .enumerate()
            .map(|(i, &b)| {
                if (148..156).contains(&i) {
                    0x20
                } else {
                    b as u64
                }
            })
            .sum();
        t[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        let err = walk(&t, 1 << 20).unwrap_err();
        assert!(err.contains("archive root"), "got: {err}");
    }

    #[test]
    fn a_tampered_header_refuses() {
        let mut t = raw_tar(&[("theme", b"X", b'0')]);
        t[0] = b'T'; // name edit without checksum fix
        let err = walk(&t, 1 << 20).unwrap_err();
        assert!(err.contains("checksum mismatch"), "got: {err}");
    }

    #[test]
    fn the_caps_bound_member_and_total_alike() {
        // Header claims more than the cap: refused before a byte is read.
        let err = walk(&raw_tar(&[("theme", b"OVERSIZED", b'0')]), 4).unwrap_err();
        assert!(err.contains("byte cap"), "got: {err}");
        // A bomb hiding PAST the member (endless zero tail) trips the
        // TOTAL-consumption cap inside the zeros-to-EOF scan.
        let mut t = raw_tar(&[("theme", b"OK", b'0')]);
        let tail = t.len() + (1 << 20);
        t.resize(tail, 0);
        let err = walk(&t, 4096).unwrap_err();
        assert!(err.contains("cap exceeded"), "got: {err}");
        // Trailing NONZERO garbage refuses as shape, not as cap.
        let mut t = raw_tar(&[("theme", b"OK", b'0')]);
        t.push(b'!');
        let err = walk(&t, 1 << 20).unwrap_err();
        assert!(err.contains("trailing entries"), "got: {err}");
    }

    #[test]
    fn an_empty_member_never_installs() {
        let d = tempdir();
        let target = d.join("theme");
        std::fs::write(&target, b"OLD").unwrap();
        let tb = tarball(&d, "theme", b"");
        let err = install_over(&target, &tb).unwrap_err();
        assert!(err.contains("empty member"), "got: {err}");
        assert_eq!(std::fs::read(&target).unwrap(), b"OLD");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_unwritable_directory_is_a_clear_refusal() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempdir();
        let sub = d.join("bin");
        std::fs::create_dir(&sub).unwrap();
        let target = sub.join("theme");
        std::fs::write(&target, b"OLD").unwrap();
        let tb = tarball(&d, "theme", b"NEW");
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();
        let err = install_over(&target, &tb).unwrap_err();
        assert!(err.contains("never elevates"), "got: {err}");
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"OLD");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Test scratch inside target/ like the save tests — never /tmp, whose
    /// world-writable ancestry other cases legitimately refuse. A counter
    /// joins the timestamp: parallel tests can share a clock tick, and a
    /// name collision lets one test's cleanup delete another's files.
    fn tempdir() -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-tmp")
            .join(format!(
                "upd-{}-{:x}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|t| t.as_nanos())
                    .unwrap_or(0),
                SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn test_cfg(cache: &Path) -> Config {
        Config {
            wallpaper_dirs: Vec::new(),
            wallpaper_dirs_display: String::new(),
            cache_dir: cache.to_path_buf(),
            kitty_dir: cache.to_path_buf(),
            current: cache.join("current-theme.conf"),
            formats: Vec::new(),
            contrast: 4.5,
            no_apply: true,
        }
    }

    #[test]
    fn the_cache_stamp_is_fail_closed() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempdir();
        let cache = d.join("cache");
        std::fs::create_dir(&cache).unwrap();
        let victim = d.join("victim");
        std::fs::write(&victim, b"twenty-four bytes long!!").unwrap();
        std::os::unix::fs::symlink(&victim, cache.join("update-check")).unwrap();
        let cfg = test_cfg(&cache);
        // World-writable dir: custody refuses — nothing written, symlink
        // untouched, victim byte-identical.
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o777)).unwrap();
        write_check(&cfg, "v9.9.9");
        assert_eq!(std::fs::read(&victim).unwrap(), b"twenty-four bytes long!!");
        assert!(
            std::fs::symlink_metadata(cache.join("update-check"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        // Owner-only dir: the stamp lands by REPLACING the symlink entry
        // (renameat), never following it — the victim still untouched.
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700)).unwrap();
        write_check(&cfg, "v9.9.9");
        assert_eq!(std::fs::read(&victim).unwrap(), b"twenty-four bytes long!!");
        let md = std::fs::symlink_metadata(cache.join("update-check")).unwrap();
        assert!(md.file_type().is_file());
        assert_eq!(
            std::fs::read_to_string(cache.join("update-check")).unwrap(),
            "v9.9.9"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_check_read_never_follows_a_symlink() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempdir();
        let cache = d.join("cache");
        std::fs::create_dir(&cache).unwrap();
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700)).unwrap();
        let victim = d.join("victim");
        std::fs::write(&victim, b"v9.9.9").unwrap();
        std::os::unix::fs::symlink(&victim, cache.join("update-check")).unwrap();
        let dirfd = check_dir(&test_cfg(&cache)).unwrap();
        let (fresh, content) = read_check(&dirfd);
        assert!(!fresh, "a symlinked cache must read as no-data");
        assert!(
            content.is_empty(),
            "the linked file's content must not leak"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Install temp files (`.theme.update.*`) left in the directory —
    /// success and refusal alike must leave zero.
    fn dotfiles_beside(d: &Path) -> usize {
        std::fs::read_dir(d)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
            .count()
    }
}

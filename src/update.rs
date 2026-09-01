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

use crate::config::{MAX_DOWNLOAD_BYTES, UA};
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

const API_LATEST: &str = "https://api.github.com/repos/snaraj/theme/releases/latest";
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

pub fn cmd_update() {
    if TARGET.is_empty() {
        die("no published release build for this platform — build from source");
    }
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
            "30",
            "-A",
            UA,
            "-K",
            "-",
            "--url",
            API_LATEST,
        ],
    )
    .unwrap_or_else(|| die("cannot reach the GitHub release API (no network or no release yet)"));
    if body.len() as u64 > API_CAP {
        die("release API answer exceeds its size cap");
    }
    let json = String::from_utf8(body)
        .ok()
        .and_then(|s| Json::parse(&s))
        .unwrap_or_else(|| die("release API answered unparseable JSON"));

    let tag = json
        .str_field("tag_name")
        .filter(|t| tag_shape_ok(t))
        .map(str::to_string)
        .unwrap_or_else(|| die("release has no usable tag"));
    let current = env!("CARGO_PKG_VERSION");
    if tag.trim_start_matches('v') == current {
        println!("already up to date (v{current})");
        return;
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

/// Unpack-then-rename. The release asset is a tar.gz holding the single
/// member `theme` (the release workflow's `tar -C … theme`); its digest was
/// verified against SHA256SUMS before this runs, and between verification
/// and unpack it sits in the 0700 scratch directory — the same
/// no-other-principal custody every save in this tool relies on. tar
/// streams the member (`-xzOf`) STRAIGHT into a fresh temp file opened
/// O_CREAT|O_EXCL 0755 in the TARGET'S own directory (same filesystem), so
/// there is no intermediate extracted file; only a clean tar exit with
/// non-empty output earns fsync + the atomic rename(2) over the target.
/// Any failure unlinks the temp and the target is untouched — there is no
/// window where it is partial or unverified.
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

    let mut tar = match Command::new("tar")
        .arg("-xzOf")
        .arg(tarball)
        .arg("theme")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            sweep();
            return Err(format!("cannot run tar: {e}"));
        }
    };
    let mut out = tar.stdout.take().expect("piped stdout");
    let mut total: u64 = 0;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = match out.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                let _ = tar.kill();
                let _ = tar.wait();
                sweep();
                return Err(format!("read failed mid-unpack: {e}"));
            }
        };
        total += n as u64;
        if let Err(e) = tmp.write_all(&buf[..n]) {
            let _ = tar.kill();
            let _ = tar.wait();
            sweep();
            return Err(format!("write failed mid-install: {e}"));
        }
    }
    let tar_ok = tar.wait().map(|s| s.success()).unwrap_or(false);
    if !tar_ok || total == 0 {
        sweep();
        return Err("the verified archive holds no 'theme' binary — refusing to install".into());
    }
    if let Err(e) = tmp.sync_all() {
        sweep();
        return Err(format!("fsync failed: {e}"));
    }
    drop(tmp);
    rustix::fs::renameat(&dirfd, tmp_name.as_str(), &dirfd, name).map_err(|e| {
        let _ = rustix::fs::unlinkat(&dirfd, tmp_name.as_str(), rustix::fs::AtFlags::empty());
        format!("cannot replace {}: {e}", target.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUMS: &str = "\
0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  theme-v1.2.3-aarch64-apple-darwin\n\
fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210 *theme-v1.2.3-x86_64-unknown-linux-gnu\n";

    #[test]
    fn sums_selection_is_unique_and_shape_checked() {
        let (hex, name) = pick_from_sums(SUMS, "aarch64-apple-darwin").unwrap();
        assert_eq!(name, "theme-v1.2.3-aarch64-apple-darwin");
        assert!(hex.starts_with("0123"));
        // The `*` binary-marker form parses too.
        let (_, name) = pick_from_sums(SUMS, "x86_64-unknown-linux-gnu").unwrap();
        assert_eq!(name, "theme-v1.2.3-x86_64-unknown-linux-gnu");
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

    fn sha_hex(b: &[u8]) -> String {
        Sha256::digest(b)
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect()
    }

    /// A tar.gz shaped like the release asset: one member, named `member`,
    /// holding `bytes`.
    fn tarball(dir: &Path, member: &str, bytes: &[u8]) -> std::path::PathBuf {
        let src = dir.join(member);
        std::fs::write(&src, bytes).unwrap();
        let tb = dir.join("asset.tar.gz");
        let ok = Command::new("tar")
            .arg("-czf")
            .arg(&tb)
            .arg("-C")
            .arg(dir)
            .arg(member)
            .status()
            .unwrap()
            .success();
        assert!(ok, "test tarball creation failed");
        std::fs::remove_file(&src).unwrap();
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
        assert!(err.contains("no 'theme' binary"), "got: {err}");
        assert_eq!(std::fs::read(&target).unwrap(), b"OLD");
        assert_eq!(dotfiles_beside(&d), 0, "the temp was swept");
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
    /// world-writable ancestry other cases legitimately refuse.
    fn tempdir() -> std::path::PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-tmp")
            .join(format!(
                "upd-{}-{:x}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|t| t.as_nanos())
                    .unwrap_or(0)
            ));
        std::fs::create_dir_all(&d).unwrap();
        d
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

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
use crate::net::{curl_config_trusted, url_host};
use crate::scratch;
use crate::ui::{die, display_text};
use rustix::fs::{Mode, OFlags};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
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

/// How this copy of theme got here. Everything but [`Install::File`] has an
/// owner that is not us, and the answer is that owner's own command.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Install {
    Homebrew,
    Deb,
    Rpm,
    Package,
    Cargo,
    File,
}

/// Where a `cargo install` binary can land, in cargo's own precedence.
/// Canonicalized to compare against a canonicalized exe; a directory that
/// does not exist is simply not a candidate.
fn cargo_bins() -> Vec<PathBuf> {
    [
        ("CARGO_INSTALL_ROOT", "bin"),
        ("CARGO_HOME", "bin"),
        ("HOME", ".cargo/bin"),
    ]
    .iter()
    .filter_map(|(var, sub)| std::env::var_os(var).map(|v| Path::new(&v).join(sub)))
    .filter_map(|p| std::fs::canonicalize(p).ok())
    .collect()
}

/// Pure over its inputs — the filesystem questions are the caller's — so
/// every route is testable without planting a tree. Order matters: a keg
/// lives under a Homebrew prefix that is nobody else's, a cargo bin
/// outranks the distro directories it may be symlinked beside, and only a
/// path under a distro bin directory is a package's to own.
fn classify(exe: &Path, cargo_bins: &[PathBuf], deb_installed: bool, rpm_db: bool) -> Install {
    // COMPONENTS, never substrings: `Cellar` then this formula's name, so
    // `/opt/homebrew`, `/usr/local` and linuxbrew prefixes all match while a
    // directory merely named `Cellarium` — or another formula's keg that
    // ships a `theme` — does not.
    if exe
        .components()
        .collect::<Vec<_>>()
        .windows(2)
        .any(|w| w[0].as_os_str() == "Cellar" && w[1].as_os_str() == "theme")
    {
        return Install::Homebrew;
    }
    if cargo_bins.iter().any(|d| exe.starts_with(d)) {
        return Install::Cargo;
    }
    if ["/usr/bin", "/usr/sbin", "/bin", "/sbin"]
        .iter()
        .any(|d| exe.starts_with(d))
    {
        return if deb_installed {
            Install::Deb
        } else if rpm_db {
            Install::Rpm
        } else {
            Install::Package
        };
    }
    Install::File
}

/// The managed-install answer, printed in the manager's own terms; true
/// when it was given, false for a plain file install that falls through to
/// the self-updater. Reads the exe path and two well-known package
/// databases — no network, no child process, nothing installed.
fn route(exe: &Path) -> bool {
    let p = display_text(&exe.display().to_string());
    let kind = classify(
        exe,
        &cargo_bins(),
        Path::new("/var/lib/dpkg/info/theme.list").is_file(),
        Path::new("/var/lib/rpm").is_dir() || Path::new("/usr/lib/sysimage/rpm").is_dir(),
    );
    match kind {
        Install::File => return false,
        Install::Homebrew => {
            println!("theme here is a Homebrew keg ({p}) — Homebrew updates it:");
            println!("  brew upgrade snaraj/theme/theme");
            println!(
                "(the tap's formula can trail a release until its bump merges; every build is at https://github.com/snaraj/theme/releases)"
            );
        }
        // One sentence, one place to keep true; only the extension and the
        // manager's own install line differ.
        Install::Deb | Install::Rpm => {
            let (ext, cmd) = if kind == Install::Deb {
                (".deb", "sudo apt install ./theme_*.deb")
            } else {
                (".rpm", "sudo dnf install ./theme-*.rpm")
            };
            println!(
                "theme here was installed from a {ext} ({p}) — take the next one from https://github.com/snaraj/theme/releases/latest and run:"
            );
            println!("  {cmd}");
        }
        Install::Package => {
            println!(
                "theme here lives under a package manager's directory ({p}) — take the next .deb or .rpm from https://github.com/snaraj/theme/releases/latest"
            );
        }
        Install::Cargo => {
            println!(
                "theme here was built by cargo install ({p}) — rebuild it from the current source:"
            );
            println!("  cargo install --git https://github.com/snaraj/theme --locked");
        }
    }
    true
}

pub fn cmd_update(cfg: &Config, want: &str) {
    // WHO OWNS THESE BYTES comes first — before the platform check, before
    // the transport check, before any network. A keg or a distro package
    // belongs to its manager (that route would otherwise fetch a whole
    // release and then refuse at `install_over`'s "never elevates"), and a
    // cargo build was chosen deliberately — sometimes because no prebuilt
    // binary can run there (musl), where the glibc tarball would install a
    // binary that cannot start. `theme version` and the bare screen still
    // say "run theme update": this is the one entry point, dispatching.
    let target = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .unwrap_or_else(|_| die("cannot resolve the running binary's path"));
    if route(&target) {
        return;
    }
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
    // The transport bar, stated up front with its own message: replacing
    // the running executable through a curl that is not the root-owned
    // system one is refused outright — PATH is never consulted here.
    if crate::net::trusted_curl().is_none() {
        die(
            "no trusted system curl (a root-owned /usr/bin/curl) — refusing to fetch a binary through an unvetted transport",
        );
    }
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
    install_over(&target, &staged).unwrap_or_else(|e| die(&e));
    scratch::done(&staged);
    println!("theme v{current} → {tag}");
    println!("updated: {}", display_text(&target.display().to_string()));
}

/// One release-API request: hardened flags, bounded size, parsed JSON.
/// `max_time` is the caller's latency budget — 30s for the explicit
/// `theme update`, 2s for either update check (the footer's silent refresh
/// and `theme version`'s live ask both wait behind a person).
///
/// The transport is the TRUSTED curl only — resolved and re-validated per
/// call, never PATH (round 8: one planted curl would control metadata,
/// digest file, and hashed bytes at once, making SHA-256 self-referential).
/// `-q` sits FIRST on the argv so no curlrc can inject options, and
/// [`curl_config_trusted`] scrubs every proxy variable from the child env.
fn fetch_release(url: &str, max_time: &str) -> Option<Json> {
    let curl = crate::net::trusted_curl()?;
    let body = curl_config_trusted(
        &curl,
        "header = \"Accept: application/vnd.github+json\"\nheader = \"X-GitHub-Api-Version: 2022-11-28\"\n",
        &[
            "-q",
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

/// The kill switch, read in one place: THEME_NO_UPDATE_CHECK non-empty
/// disables every update check — the footer's, and `theme version`'s
/// closing line with it.
fn check_off() -> bool {
    std::env::var("THEME_NO_UPDATE_CHECK")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

/// The CACHED answer, the footer note's contract: one small cache file is
/// read, and at most one bounded refresh (2s hard cap) runs per
/// [`CHECK_TTL`] window — stamped even on failure, so an offline machine
/// pays it once per window, not per run.
pub fn latest_tag(cfg: &Config) -> Option<(u64, u64, u64)> {
    latest_tag_with(cfg, false)
}

/// The latest published release, as a strict triple — or None whenever it
/// cannot be KNOWN: the kill-switch (THEME_NO_UPDATE_CHECK, non-empty) is
/// set, custody refuses the cache dir, no trusted transport exists, the
/// fetch failed, or the cache is malformed. Callers render silence on
/// None; none of them may guess. Every cache touch — read and stamp alike
/// — goes through [`check_dir`]'s fail-closed custody.
///
/// `live` is the deliberate question's mode (`theme version`, issue #42):
/// the cache's freshness is never consulted, the trusted transport is
/// required, and one bounded request runs on EVERY call — under the same
/// 2s cap the footer has always lived on, because the caller is waiting.
/// A usable tag stamps the shared cache so the footer benefits from the
/// ask; a failed one stamps NOTHING — overwriting a good stamp with a
/// failure would silence the footer for a whole TTL window — and returns
/// None, so the caller makes no claim it cannot back.
fn latest_tag_with(cfg: &Config, live: bool) -> Option<(u64, u64, u64)> {
    if check_off() {
        return None;
    }
    // Custody first: a cache dir that fails the fail-closed audit gets no
    // read, no stamp, no answer — and no network attempt either.
    let dirfd = check_dir(cfg)?;
    if live {
        crate::net::trusted_curl()?;
        let tag = fetch_release(&format!("{RELEASES_API}/latest"), "2").and_then(|j| {
            j.str_field("tag_name")
                .filter(|t| tag_shape_ok(t))
                .map(str::to_string)
        })?;
        write_check_at(&dirfd, &tag);
        return parse_v3(&tag);
    }
    let (fresh, mut cached) = read_check(&dirfd);
    if !fresh {
        // No trusted transport ⇒ no network AND no stamp (decided, round
        // 8): the TTL stamp exists to rate-limit NETWORK attempts, and no
        // attempt happened — validating a candidate is one local stat, so
        // there is nothing to throttle and a masked window would only hide
        // a transport that recovers a minute later. A still-fresh cache
        // above renders fine without any transport at all.
        crate::net::trusted_curl()?;
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
    // The cached tag is REMOTE data: it survives only as a strict numeric
    // semver triple, so nothing a caller prints is ever the cached string.
    parse_v3(cached.trim())
}

/// This build's own version. `CARGO_PKG_VERSION` is a compile-time constant
/// this crate controls, so None means a broken build, not remote data — and
/// the comparisons below then stay silent rather than guess.
fn current_v3() -> Option<(u64, u64, u64)> {
    parse_v3(&format!("v{}", env!("CARGO_PKG_VERSION")))
}

/// The update-available footer on the bare `theme` screen. Silent on every
/// failure mode — offline, rate-limited, bad JSON, malformed cache — and
/// printed ONLY when the latest is strictly newer than this build. Both
/// printed values are RECONSTRUCTED from the parsed numbers, so a
/// remote-supplied string or URL is never echoed.
pub fn maybe_note(cfg: &Config) {
    let Some(cur) = current_v3() else { return };
    if let Some((a, b, c)) = latest_tag(cfg).filter(|l| *l > cur) {
        println!(
            "\nupdate to the latest theme version: v{a}.{b}.{c} -> https://github.com/snaraj/theme/releases/tag/v{a}.{b}.{c}"
        );
        println!("to update run: theme update");
    }
}

/// The three plain lines — this build, the repository, the maintainer.
/// Compile-time constants only: no cache read, no custody walk, no
/// network, so printing them costs a process start and nothing more.
fn print_facts() {
    println!(
        "version: v{}\ngithub: https://github.com/snaraj/theme\nmaintainer: Samuel Naranjo",
        env!("CARGO_PKG_VERSION")
    );
}

/// `theme -V` / `theme --version` — the build alone. This is the form
/// scripts and other tools call, so it asks NOTHING and can never wait on
/// a network round trip: a version banner that blocks is one that hangs
/// somebody's pipeline. `theme version`, the word, is the question.
pub fn cmd_version_plain() {
    print_facts();
}

/// `theme version` — the release ledger's other question, which is why it
/// lives beside `update`: the owner wants it to answer "am I current?",
/// and a deliberate question gets a LIVE answer (issue #42 — a day-old
/// stamp once said "latest" a minute after the next release). The shared
/// cache is only WRITTEN here, never trusted.
///
/// It STREAMS: the three lines `-V` would print are WRITTEN first, so the
/// facts never wait on the network, and exactly one closing line follows
/// when the ask resolves — including when it fails, which says so rather
/// than leaving the reader to guess. The kill switch removes that line
/// entirely. The latest is reconstructed from its parsed triple, never
/// echoed from the answer.
pub fn cmd_version(cfg: &Config) {
    print_facts();
    // Insurance, not the mechanism: std's stdout is a LineWriter, so the
    // three lines are already out at the newline for every sink this has —
    // the explicit flush is what keeps that true if the sink ever stops
    // being line-buffered.
    let _ = std::io::stdout().flush();
    if check_off() {
        return;
    }
    match (current_v3(), latest_tag_with(cfg, true)) {
        // "could not check", not "could not reach": two of the three
        // causes — refused custody, no trusted curl — never make a
        // request, so naming the network would be a lie about what
        // happened.
        (_, None) => println!("latest release: unknown (could not check)"),
        (Some(c), Some(l)) if l > c => println!(
            "latest release: v{}.{}.{} — update with 'theme update'",
            l.0, l.1, l.2
        ),
        (Some(c), Some(l)) if l == c => println!("you're on the latest release."),
        (_, Some(l)) => println!("latest release: v{}.{}.{}", l.0, l.1, l.2),
    }
}

/// Fail-closed custody of the cache directory (Codex rounds 4+5), through
/// the saver's OWN audit machinery — [`crate::save::audit_dir`] /
/// [`crate::save::audit_chain`], reused, not copied. Custody means ALL of:
///
/// 1. **Spelled-chain audit** — every component of the path as the user
///    spelled it (the dirs that would HOLD any symlink) is self-or-root
///    owned, free of group/world write, and free of foreign write-class
///    ACLs. This kills endpoint steering: a symlink sitting in a
///    world-writable ancestor refuses no matter where it points.
/// 2. **Canonical-chain audit** — the resolved directory and every real
///    ancestor pass the same owner/mode/ACL audit, FAIL-CLOSED when ACLs
///    cannot be interrogated. (INTERMEDIATE symlinks into audited
///    territory stay legal — every macOS path traverses
///    `/var -> /private/var` — but the LEAF itself opens O_NOFOLLOW, so a
///    symlink at the cache-dir name refuses outright.)
/// 3. **Bound endpoint** — the dirfd comes from an openat O_NOFOLLOW walk
///    of the canonical chain, its fstat must show our uid, directory
///    type, and no group/world write, and its (dev, ino) must equal a
///    fresh stat of the audited path — the fd IS the audited endpoint.
///
/// Created 0700 when absent — but only mkdirat THROUGH the already-bound
/// parent fd, after the parent chain passed in full: a refused chain
/// creates nothing anywhere (round 7). Audited, never chmodded. Any
/// failure gets NOTHING: no stamp, no read, no note, no network.
fn check_dir(cfg: &Config) -> Option<rustix::fd::OwnedFd> {
    let acl = crate::save::AclAudit::native();
    let parent = cfg.cache_dir.parent()?;
    let leaf = cfg.cache_dir.file_name()?;
    let mut spelled = Vec::new();
    let mut prefix = std::path::PathBuf::new();
    for c in parent.components() {
        match c {
            std::path::Component::RootDir => prefix.push("/"),
            std::path::Component::Normal(n) => prefix.push(n),
            _ => return None, // relative paths and dot-components: no custody
        }
        if prefix.parent().is_none() {
            continue; // "/" itself is every chain's root; audited below
        }
        spelled.push(prefix.clone());
    }
    let pcanon = std::fs::canonicalize(parent).ok()?;
    // Both chains and the endpoint are ONE question, so their ACLs are read
    // in one `ls` before the first verdict is asked for. Reading decides
    // nothing: the audits below run in the order they always ran, and each
    // still re-stats its path for owner and mode.
    let mut ask = spelled.clone();
    ask.extend(crate::save::chain_of(&pcanon));
    ask.push(pcanon.join(leaf));
    acl.prefetch(&ask);
    // The PARENT chain is audited and bound BEFORE anything else — even
    // the first-run mkdir: a hostile spelled chain must get NOTHING, not
    // an empty 0700 dir conjured at an attacker-steered target (round 7).
    for p in &spelled {
        crate::save::audit_dir(p, &acl).ok()?;
    }
    crate::save::audit_chain(&pcanon, &acl).ok()?;
    let pfd = open_chain_nofollow(&pcanon)?;
    let me = rustix::process::getuid().as_raw();
    let pst = rustix::fs::fstat(&pfd).ok()?;
    if !fd_custody_ok(&pst, me) {
        return None;
    }
    let pnow = rustix::fs::stat(&pcanon).ok()?;
    if pnow.st_dev != pst.st_dev || pnow.st_ino != pst.st_ino {
        return None;
    }
    // Creation is dirfd-relative THROUGH the trusted parent (a single
    // component — nothing to traverse), and the leaf opens O_NOFOLLOW
    // from the same fd, so a symlink planted at the name refuses.
    let _ = rustix::fs::mkdirat(&pfd, leaf, Mode::from_raw_mode(0o700));
    let fd = rustix::fs::openat(
        &pfd,
        leaf,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .ok()?;
    let st = rustix::fs::fstat(&fd).ok()?;
    if !fd_custody_ok(&st, me) {
        return None;
    }
    let epath = pcanon.join(leaf);
    crate::save::audit_dir(&epath, &acl).ok()?;
    let now = rustix::fs::stat(&epath).ok()?;
    if now.st_dev != st.st_dev || now.st_ino != st.st_ino {
        return None;
    }
    Some(fd)
}

/// Root-down openat walk of an absolute, canonical path: every component
/// opens O_NOFOLLOW|O_DIRECTORY relative to the previous fd, so a symlink
/// racing in anywhere along the chain refuses instead of being followed.
pub(crate) fn open_chain_nofollow(path: &Path) -> Option<rustix::fd::OwnedFd> {
    use std::path::Component;
    let mut comps = path.components();
    if comps.next() != Some(Component::RootDir) {
        return None;
    }
    let mut fd = rustix::fs::open("/", OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty()).ok()?;
    for c in comps {
        let Component::Normal(name) = c else {
            return None;
        };
        fd = rustix::fs::openat(
            &fd,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .ok()?;
    }
    Some(fd)
}

/// The fd-level custody predicate, pure over the stat so the foreign-owner
/// arm is testable without root — the same trick own_socket's test uses
/// for its unforgeable-uid branch.
pub(crate) fn fd_custody_ok(st: &rustix::fs::Stat, my_uid: u32) -> bool {
    rustix::fs::FileType::from_raw_mode(st.st_mode) == rustix::fs::FileType::Directory
        && st.st_uid == my_uid
        && st.st_mode & 0o022 == 0
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

/// The SHA256SUMS entry for this platform's tarball, matched on the EXACT
/// asset name `theme-<target>.tar.gz`. Lines are `<64-hex>  <name>`
/// (` *name` binary-marker form accepted). Exact, not "contains": the
/// release also carries distro packages, and any future asset that merely
/// mentioned the triple would make every update refuse as ambiguous.
/// Zero or multiple matches still refuse — a release that cannot name this
/// platform unambiguously does not get installed.
fn pick_from_sums(sums: &str, target: &str) -> Result<(String, String), String> {
    let asset = format!("theme-{target}.tar.gz");
    let mut hit: Option<String> = None;
    for line in sums.lines() {
        let line = line.trim_end_matches('\r');
        let (hex, name) = match line.split_once(' ') {
            Some(t) => t,
            None => continue,
        };
        if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        if name.trim_start().trim_start_matches('*') != asset {
            continue;
        }
        if hit.is_some() {
            return Err(format!("SHA256SUMS names more than one {target} asset"));
        }
        hit = Some(hex.to_ascii_lowercase());
    }
    hit.map(|hex| (hex, asset))
        .ok_or_else(|| format!("SHA256SUMS has no entry for {target}"))
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
        // Trusted transport, RE-VALIDATED per hop, under the SAME env
        // boundary as the metadata request ([`crate::save::trusted_spawn`]
        // — empty child env, round 9): `-q` first (no curlrc), --noproxy
        // on the argv as belt-and-braces, PATH never consulted.
        let curl = crate::net::trusted_curl()
            .ok_or("no trusted system curl (a root-owned /usr/bin/curl)")?;
        let mut cmd = crate::save::trusted_spawn(&curl);
        cmd.args([
            "-q",
            "-sg",
            "--noproxy",
            "*",
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
        .arg(&here);
        let ok = cmd.status().map(|s| s.success()).unwrap_or(false);
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
    use std::process::Command;

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
        // The release digests its distro packages in the same SHA256SUMS,
        // so a substring match would read them as a second candidate and
        // refuse every update.
        let with_pkgs = format!(
            "{SUMS}\
             1111111111111111111111111111111111111111111111111111111111111111  theme_0.2.2_arm64.deb\n\
             2222222222222222222222222222222222222222222222222222222222222222  theme-aarch64-apple-darwin.deb\n\
             3333333333333333333333333333333333333333333333333333333333333333  theme-aarch64-apple-darwin.rpm\n"
        );
        let (hex, name) = pick_from_sums(&with_pkgs, "aarch64-apple-darwin").unwrap();
        assert_eq!(name, "theme-aarch64-apple-darwin.tar.gz");
        assert!(hex.starts_with("0123"));
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

    /// The footer and `theme version` gate on ONE decision — the parsed
    /// remote triple against this build's own — so it is pinned directly:
    /// strictly newer offers the update, equal says "current", and nothing
    /// unparseable ever reaches a comparison.
    #[test]
    fn the_release_comparison_is_numeric_and_strict() {
        let cur = Some((0, 0, 1));
        assert!(parse_v3("v9.9.9") > cur);
        assert_eq!(parse_v3("v0.0.1"), cur); // equal
        assert!(parse_v3("v0.0.0") < cur); // dev build newer
        assert_eq!(parse_v3(""), None); // no data
        assert_eq!(parse_v3("v9.9.9junk"), None); // malformed
        assert!(parse_v3("v0.10.0") > parse_v3("v0.9.9")); // numeric, not lexical
        assert!(current_v3().is_some()); // this build's own version parses
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

    /// The route table: exe path, whether the cargo bins below are known,
    /// the two package-database answers, and the install this must be.
    #[test]
    #[rustfmt::skip]
    fn the_install_route_reads_only_where_the_binary_lives() {
        use Install::{Cargo, Deb, File, Homebrew, Package, Rpm};
        // CARGO_INSTALL_ROOT/bin, CARGO_HOME/bin, HOME/.cargo/bin.
        let cargo = ["/ci/root/bin", "/home/u/.cargohome/bin", "/home/u/.cargo/bin"].map(PathBuf::from);
        let cases: &[(&str, bool, bool, bool, Install)] = &[
            // Every Homebrew prefix, matched on COMPONENTS — so `Cellarium`
            // is not a keg, and another formula's keg shipping a `theme` is
            // not ours. A keg outranks both package databases.
            ("/opt/homebrew/Cellar/theme/0.3.0/bin/theme",            true,  true,  true,  Homebrew),
            ("/usr/local/Cellar/theme/0.3.0/bin/theme",               false, false, false, Homebrew),
            ("/home/linuxbrew/.linuxbrew/Cellar/theme/1.0/bin/theme", false, false, false, Homebrew),
            ("/opt/Cellarium/theme/bin/theme",                        false, false, false, File),
            ("/opt/homebrew/Cellar/other/1.0/bin/theme",              false, false, false, File),
            // Each cargo bin — and each one ONLY while it is on the list.
            ("/ci/root/bin/theme",                                    true,  false, false, Cargo),
            ("/ci/root/bin/theme",                                    false, false, false, File),
            ("/home/u/.cargohome/bin/theme",                          true,  false, false, Cargo),
            ("/home/u/.cargohome/bin/theme",                          false, false, false, File),
            ("/home/u/.cargo/bin/theme",                              true,  false, false, Cargo),
            ("/home/u/.cargo/bin/theme",                              false, false, false, File),
            // A distro bin directory, named by whichever database exists.
            ("/usr/bin/theme",                                        false, true,  false, Deb),
            ("/usr/bin/theme",                                        false, false, true,  Rpm),
            ("/usr/bin/theme",                                        false, true,  true,  Deb),
            ("/usr/bin/theme",                                        false, false, false, Package),
            // Anything a person put somewhere themselves updates in place.
            ("/usr/local/bin/theme",                                  false, true,  true,  File),
            ("/home/u/.local/bin/theme",                              false, true,  true,  File),
        ];
        for (exe, known, deb, rpm, want) in cases {
            let bins: &[PathBuf] = if *known { &cargo } else { &[] };
            assert_eq!(classify(Path::new(exe), bins, *deb, *rpm), *want, "{exe}");
        }
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
    fn the_chain_walk_refuses_symlink_components() {
        let d = tempdir();
        std::fs::create_dir(d.join("real")).unwrap();
        std::fs::create_dir(d.join("real").join("sub")).unwrap();
        std::os::unix::fs::symlink(d.join("real"), d.join("link")).unwrap();
        // A real chain opens; a symlink FINAL component refuses (the round-4
        // O_NOFOLLOW that survived mutation, now pinned); so does an
        // INTERMEDIATE one; so does a relative path.
        assert!(open_chain_nofollow(&d.join("real").join("sub")).is_some());
        assert!(open_chain_nofollow(&d.join("link")).is_none());
        assert!(open_chain_nofollow(&d.join("link").join("sub")).is_none());
        assert!(open_chain_nofollow(Path::new("relative/path")).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn cache_custody_refuses_steering_and_hostile_ancestry() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempdir();
        let wopen = d.join("wopen");
        let real = d.join("real");
        std::fs::create_dir(&wopen).unwrap();
        std::fs::create_dir(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&real, wopen.join("link")).unwrap();
        std::fs::create_dir(wopen.join("plain")).unwrap();
        std::fs::set_permissions(wopen.join("plain"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        std::fs::set_permissions(&wopen, std::fs::Permissions::from_mode(0o777)).unwrap();
        // Codex round-5 repro: symlink behind a world-writable ancestor,
        // pointing at a perfectly clean 0700 dir — the SPELLED chain audit
        // refuses the steering regardless of the target's own hygiene.
        assert!(check_dir(&test_cfg(&wopen.join("link"))).is_none());
        // A real 0700 dir under the same hostile ancestor refuses too.
        assert!(check_dir(&test_cfg(&wopen.join("plain"))).is_none());
        // Control: the clean dir reached directly is accepted…
        assert!(check_dir(&test_cfg(&real)).is_some());
        // …the LEAF itself opens O_NOFOLLOW, so a symlink AT the cache-dir
        // name refuses even from clean territory (Codex round-6 killer d)…
        std::os::unix::fs::symlink(&real, d.join("goodlink")).unwrap();
        assert!(check_dir(&test_cfg(&d.join("goodlink"))).is_none());
        // …while an INTERMEDIATE benign symlink still carries custody —
        // every macOS path crosses /var -> /private/var — with the stamp
        // landing in the audited real target.
        std::fs::create_dir(real.join("sub2")).unwrap();
        let via = d.join("goodlink").join("sub2");
        assert!(check_dir(&test_cfg(&via)).is_some());
        write_check(&test_cfg(&via), "v7.7.7");
        assert_eq!(
            std::fs::read_to_string(real.join("sub2").join("update-check")).unwrap(),
            "v7.7.7"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The whole custody question — spelled chain, canonical chain and the
    /// endpoint — costs ONE interrogation. Before this it cost one per
    /// ancestor per chain (a score of spawns for a tree this deep, ~25 ms),
    /// which is what the default bare screen paid on every run.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_whole_custody_audit_asks_ls_once() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempdir();
        let cache = d.join("cache");
        std::fs::create_dir(&cache).unwrap();
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700)).unwrap();
        crate::save::SPAWNS.with(|n| n.set(0));
        assert!(check_dir(&test_cfg(&cache)).is_some(), "clean control");
        assert_eq!(
            crate::save::SPAWNS.with(|n| n.get()),
            1,
            "the custody audit asked more than once"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Platform-gated honestly: `chmod +a` is the Darwin ACL mechanism.
    /// The POSIX arm is pinned on Linux below, and the pure in-process
    /// ACL-xattr parser (incl. its unparseable-refuses branches) is pinned
    /// for every platform in save_tests::forced_posix_acl_predicate.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_foreign_write_acl_on_a_0700_cache_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempdir();
        let cache = d.join("cache");
        std::fs::create_dir(&cache).unwrap();
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700)).unwrap();
        let grant = "user:daemon allow add_file,delete_child";
        let ok = Command::new("/bin/chmod")
            .args(["+a", grant])
            .arg(&cache)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "chmod +a failed");
        assert!(
            check_dir(&test_cfg(&cache)).is_none(),
            "a 0700 mode must not outrank a foreign write ACL"
        );
        let _ = Command::new("/bin/chmod")
            .args(["-a", grant])
            .arg(&cache)
            .status();
        assert!(check_dir(&test_cfg(&cache)).is_some(), "control after -a");
        // The rule is identity-FREE (round 7: matching "our" principal
        // needed a PATH-resolved `id`, which laundered ACLs): even a grant
        // to the owner's own user refuses — out of contract, documented.
        if let Ok(user) = std::env::var("USER")
            && !user.is_empty()
        {
            let own = format!("user:{user} allow add_file");
            let ok = Command::new("/bin/chmod")
                .args(["+a", &own])
                .arg(&cache)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            assert!(ok, "chmod +a self-grant failed");
            assert!(
                check_dir(&test_cfg(&cache)).is_none(),
                "the identity-free rule must refuse even a self ACL"
            );
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Platform-gated honestly: setfacl is how a POSIX ACL is CREATED for
    /// the test — the audit itself reads the ACL xattr in-process now and
    /// runs no subprocess at all.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_posix_write_acl_on_the_cache_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempdir();
        let cache = d.join("cache");
        std::fs::create_dir(&cache).unwrap();
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            check_dir(&test_cfg(&cache)).is_some(),
            "clean control first"
        );
        let ok = Command::new("setfacl")
            .args(["-m", "u:root:rwx"])
            .arg(&cache)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "setfacl failed — is the acl package missing?");
        assert!(
            check_dir(&test_cfg(&cache)).is_none(),
            "a POSIX write ACL for another principal must refuse"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn refused_custody_creates_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempdir();
        let wopen = d.join("wopen");
        let real = d.join("real");
        std::fs::create_dir(&wopen).unwrap();
        std::fs::create_dir(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&real, wopen.join("link")).unwrap();
        std::fs::set_permissions(&wopen, std::fs::Permissions::from_mode(0o777)).unwrap();
        // Round-7 LOW repro: hostile ancestor + ABSENT final component —
        // custody refuses AND no directory is conjured at the steered
        // target (the old flow mkdir'd before auditing).
        assert!(check_dir(&test_cfg(&wopen.join("newcache"))).is_none());
        assert!(
            !wopen.join("newcache").exists(),
            "refusal must create nothing"
        );
        assert!(check_dir(&test_cfg(&wopen.join("link").join("newcache"))).is_none());
        assert!(
            !real.join("newcache").exists(),
            "steered creation must not reach the symlink target"
        );
        // First-run creation still works through a TRUSTED parent: the
        // leaf appears 0700 via mkdirat on the bound parent fd.
        let fresh = d.join("fresh-cache");
        assert!(check_dir(&test_cfg(&fresh)).is_some());
        let mode = std::fs::metadata(&fresh).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "first-run cache dir must be 0700");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn foreign_owner_and_loose_modes_fail_fd_custody() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempdir();
        let fd = rustix::fs::open(&d, OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty()).unwrap();
        let st = rustix::fs::fstat(&fd).unwrap();
        let me = rustix::process::getuid().as_raw();
        assert!(fd_custody_ok(&st, me));
        // The unforgeable-without-root arm, driven with a doctored uid —
        // own_socket's test plays the same trick.
        assert!(!fd_custody_ok(&st, me.wrapping_add(1)));
        let loose = d.join("loose");
        std::fs::create_dir(&loose).unwrap();
        std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o770)).unwrap();
        let lfd =
            rustix::fs::open(&loose, OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty()).unwrap();
        assert!(!fd_custody_ok(&rustix::fs::fstat(&lfd).unwrap(), me));
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

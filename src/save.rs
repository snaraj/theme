//! Destination selection AND creation, bound to a DIRECTORY DESCRIPTOR — the
//! 1:1 port of the shell's SAVE_PY helper.
//!
//! Every pathname operation re-resolves the whole string, so no amount of
//! canonicalizing can bind a check to the write that follows it: an attacker
//! who renames the checked provider directory and drops a symlink at the same
//! name redirects the open. Binding requires a descriptor. The library root
//! is opened following symlinks (a library on another volume through a
//! symlink is normal); the provider subdirectory is opened RELATIVE to that
//! descriptor with O_NOFOLLOW — one operation proving it is a real,
//! non-symlink child — and every create is O_CREAT|O_EXCL|O_NOFOLLOW against
//! that same descriptor, so the file lands in the directory that was checked,
//! whatever happens to its NAME meanwhile.
//!
//! The pathname handed BACK is only as trustworthy as its whole chain of
//! directories (everything downstream re-resolves it), so the audit walks the
//! canonical chain to the root: ownership, group/world write bits, and ACL
//! write grants — including the indirect ones (writesecurity, chown), which
//! let a principal grant itself the rest. A directory that cannot be
//! interrogated for ACLs is refused, never trusted on its mode alone.

use rustix::fs::{AtFlags, Mode, OFlags};
use rustix::io::Errno;
use std::fs;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::Command;

pub enum Saved {
    Created(PathBuf),
    Reused(PathBuf),
}

/// The user-facing save: extension from the CONTENT type, slugified name
/// Record one provenance fact as a `theme.*` xattr, best-effort (the same
/// mechanism as `theme.source`). The value is UNTRUSTED (an API record):
/// control bytes are stripped and the length capped BEFORE it persists, and a
/// dash-leading value is refused outright so it can never read as an `xattr`
/// option.
pub fn record_meta(path: &Path, key: &str, value: &str) {
    let clean: String = crate::ui::display_text(value).chars().take(256).collect();
    if clean.is_empty() || clean.starts_with('-') {
        return;
    }
    let _ = Command::new("xattr")
        .args(["-w", key, &clean])
        .arg(path)
        .output();
}

/// hint, provenance xattr, size note and the small-width warning. Returns
/// the settled library path.
pub fn save_wallpaper(
    cfg: &crate::config::Config,
    src: &Path,
    mime: &str,
    hint: &str,
    subdir: &str,
    source_url: &str,
) -> PathBuf {
    let ext = match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/avif" => "avif",
        other => other.strip_prefix("image/").unwrap_or(other),
    };
    let stem = hint.rsplit_once('.').map(|(s, _)| s).unwrap_or(hint);
    let mut base = crate::library::slugify(stem);
    if base.is_empty() {
        base = format!("wallpaper-{}", crate::timestamp());
    }
    let lib = crate::library::download_dir(cfg);
    let saved = match save_into(src, lib, subdir, &base, ext, native_platform()) {
        Ok(s) => s,
        Err(msg) => crate::ui::die(&msg),
    };
    let write_source = |dest: &Path, only_if_missing: bool| {
        if source_url.is_empty() {
            return;
        }
        if only_if_missing {
            let has = Command::new("xattr")
                .args(["-p", "theme.source"])
                .arg(dest)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if has {
                return;
            }
        }
        let _ = Command::new("xattr")
            .args(["-w", "theme.source", source_url])
            .arg(dest)
            .output();
    };
    match saved {
        Saved::Reused(dest) => {
            write_source(&dest, true);
            let name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("");
            crate::ui::note(&format!("already have {name} — reusing it"));
            dest
        }
        Saved::Created(dest) => {
            write_source(&dest, false);
            let size = crate::imaging::img_size(&dest);
            let name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let suffix = if size.is_empty() {
                String::new()
            } else {
                format!(" ({size})")
            };
            crate::ui::note(&format!("saved {name}{suffix}"));
            if let Some(w) = crate::imaging::width_of(&dest)
                && w < crate::config::MIN_WIDTH
            {
                {
                    crate::ui::note(&format!(
                        "warning: only {w}px wide, below the {}px desktop floor",
                        crate::config::MIN_WIDTH
                    ));
                }
            }
            dest
        }
    }
}

/// Which ACL interrogator governs. Runtime-forced in tests so the
/// posix/getfacl branch is exercised on every platform.
#[derive(Clone, Copy, PartialEq)]
pub enum AclPlatform {
    Darwin,
    Posix,
}

pub fn native_platform() -> AclPlatform {
    if cfg!(target_os = "macos") {
        AclPlatform::Darwin
    } else {
        AclPlatform::Posix
    }
}

const LS: &str = "/bin/ls";

/// The one external interrogator left (darwin's `ls -lde`) must itself be
/// beyond an attacker's reach before its output is trusted: reached by
/// ABSOLUTE path — PATH plays no part in a custody decision — and both the
/// binary and its directory must be root-owned with no group/world write.
/// Shared with the update transport (round 8): the curl that fetches bytes
/// destined for executable replacement passes the same bar.
pub(crate) fn trusted_system_binary(p: &str) -> bool {
    let clean = |q: &Path| {
        rustix::fs::stat(q)
            .map(|st| st.st_uid == 0 && st.st_mode & 0o022 == 0)
            .unwrap_or(false)
    };
    let p = Path::new(p);
    p.parent().map(clean).unwrap_or(false) && clean(p)
}

/// The ONE environment boundary for every spawn inside the security
/// boundary — the update lane's curl (both the metadata request and every
/// download hop) and the darwin ACL interrogator alike. A validated binary
/// still inherits its parent's environment, and that channel alone restores
/// single-actor control (round 9): CURL_CA_BUNDLE/SSL_CERT_FILE=/dev/null
/// substitute the TLS trust anchors right through `-q`, and
/// LD_PRELOAD/LD_AUDIT/LD_LIBRARY_PATH (DYLD_* on macOS) execute attacker
/// code inside the trusted binary itself. So the child starts from an
/// EMPTY environment — the allowlist is deliberately empty: this lane's
/// children need no HOME (`-q` already refuses curlrc), no PATH (the
/// binary is absolute and spawns nothing), no TMPDIR (every output lands
/// at an absolute path we pass), no locale (we parse structural output
/// only). Add a variable here only with a written justification.
pub(crate) fn trusted_spawn(program: &Path) -> Command {
    #[cfg(test)]
    SPAWNS.with(|n| n.set(n.get() + 1));
    let mut cmd = Command::new(program);
    cmd.env_clear();
    cmd
}

#[cfg(test)]
thread_local! {
    /// The spawn census, per thread so a parallel suite cannot blur it,
    /// and compiled only for tests — a release binary carries no seam.
    /// Every child of the security boundary passes through
    /// `trusted_spawn`, so counting there counts them all.
    pub(crate) static SPAWNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A REASON to refuse, or None — the STRICTEST, identity-free rule (round
/// 7: a planted `id` on PATH laundered a foreign write ACL, and getfacl
/// was PATH-resolved): any ALLOW entry carrying a write-shaped right
/// refuses no matter WHOSE it is — the owner's own included — and anything
/// the audit cannot fully interrogate refuses as unproven. No identity
/// mapping means no subprocess to plant. An owner who deliberately ACLs
/// their own library or cache is out of contract (documented in README).
/// DENY entries still restrict rather than grant, so macOS's standard
/// `group:everyone deny delete` keeps passing. writesecurity/chown count
/// as write-shaped: they let a principal grant itself the rest a moment
/// later.
fn acl_write_grant(path: &Path, platform: AclPlatform) -> Option<String> {
    match platform {
        AclPlatform::Darwin => {
            if !trusted_system_binary(LS) {
                return Some("could not be audited for ACLs: /bin/ls failed validation".into());
            }
            let out = match trusted_spawn(Path::new(LS)).arg("-lde").arg(path).output() {
                Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
                _ => return Some("could not be audited for ACLs: ls -lde failed".into()),
            };
            acl_write_grant_from(out.lines())
        }
        AclPlatform::Posix => match read_posix_acl(path) {
            Err(why) => Some(why),
            Ok(None) => None,
            Ok(Some(bytes)) => posix_acl_grant(&bytes),
        },
    }
}

/// The darwin verdict itself, over ONE directory's `ls -lde` lines — the
/// single parser, shared by the per-path reading above and the batched one
/// below, so neither can drift from the other.
fn acl_write_grant_from<'a>(lines: impl IntoIterator<Item = &'a str>) -> Option<String> {
    const GRANTS: [&str; 10] = [
        "write",
        "append",
        "delete",
        "delete_child",
        "add_file",
        "add_subdirectory",
        "chown",
        "writeattr",
        "writeextattr",
        "writesecurity",
    ];
    for ln in lines {
        let Some(ace) = ace_body(ln) else {
            continue;
        };
        let parts: Vec<&str> = ace.split_whitespace().collect();
        if !parts.contains(&"allow") {
            continue;
        }
        let who = parts.first().copied().unwrap_or("");
        let perms: Vec<&str> = parts.last().copied().unwrap_or("").split(',').collect();
        let hit: Vec<&str> = GRANTS
            .iter()
            .copied()
            .filter(|g| perms.contains(g))
            .collect();
        if hit.is_empty() {
            continue;
        }
        return Some(format!(
            "has an ACL granting {who} {}, which lets them replace entries regardless of the mode",
            hit.join(",")
        ));
    }
    None
}

/// The body of a numbered ACE line — `N: <who> allow|deny <perms>`, the
/// shape `ls -lde` gives each entry — or None for anything else, the
/// directory's own long line included: its first colon sits in the
/// timestamp, behind non-digits.
fn ace_body(l: &str) -> Option<&str> {
    let (num, ace) = l.trim_start().split_once(':')?;
    (!num.is_empty() && num.bytes().all(|b| b.is_ascii_digit())).then_some(ace)
}

/// The name `ls -l` echoed back for one operand: eight fixed fields (mode,
/// links, owner, group, size, month, day, time) and then the rest of the
/// line — which is the ABSOLUTE PATH WE PASSED, the one field of the
/// output that comes from our own argv instead of from the filesystem,
/// which is what makes attribution unforgeable. A symlink operand adds
/// ` -> target`, which is not part of the name. None for a line of any
/// other shape, and None refuses the whole batch.
fn long_line_name(ln: &str) -> Option<&str> {
    let mut rest = ln;
    for _ in 0..8 {
        rest = rest.trim_start();
        rest = &rest[rest.find(char::is_whitespace)?..];
    }
    let name = rest.trim_start();
    let name = if ln.starts_with('l') {
        name.split(" -> ").next()?
    } else {
        name
    };
    (!name.is_empty()).then_some(name)
}

/// Split one multi-operand `ls -lde` listing into (path, its ACE lines).
/// `ls -d` prints each operand's own long line and then that operand's
/// numbered entries, so every ACE belongs to the header above it. Strict:
/// a line that is neither an ACE nor a header naming one of the paths we
/// asked about, an ACE before any header, a path listed twice, or a path
/// missing from the output all return None — the caller then trusts
/// nothing from the batch and each path is read on its own, exactly as
/// today.
fn attribute<'a>(out: &'a str, want: &[&'a str]) -> Option<Vec<(&'a str, Vec<&'a str>)>> {
    let mut blocks: Vec<(&str, Vec<&str>)> = Vec::new();
    for ln in out.lines() {
        if ace_body(ln).is_some() {
            blocks.last_mut()?.1.push(ln);
            continue;
        }
        let name = long_line_name(ln)?;
        let p = want.iter().copied().find(|w| *w == name)?;
        if blocks.iter().any(|(k, _)| *k == p) {
            return None;
        }
        blocks.push((p, Vec::new()));
    }
    (blocks.len() == want.len()).then_some(blocks)
}

/// The ACL answers ONE custody question has already paid for.
///
/// macOS has no in-process ACL reader — `com.apple.system.Security` is
/// EPERM to read whether or not an ACL exists and is never listed, and
/// acl(3) needs the FFI this crate denies — so the answer costs a spawn,
/// and a chain audit asks it of eight or so directories: the spelled
/// chain, the canonical chain, the endpoint. So it is asked ONCE, for
/// every path in one argv, through the same validated binary and empty
/// environment as before, and every ancestor's verdict comes out of that
/// one reading. Only the ACL reading is remembered: ownership and the mode
/// bits are re-stat'd for every path, every time. The map lives exactly as
/// long as the custody question that made it — never process-wide, and
/// never on disk, because an audit result must not be stored in the
/// directory it audits.
pub(crate) struct AclAudit {
    platform: AclPlatform,
    seen: std::cell::RefCell<std::collections::HashMap<PathBuf, Option<String>>>,
}

impl AclAudit {
    pub(crate) fn new(platform: AclPlatform) -> Self {
        Self {
            platform,
            seen: std::cell::RefCell::new(std::collections::HashMap::new()),
        }
    }

    pub(crate) fn native() -> Self {
        Self::new(native_platform())
    }

    /// The verdict for one path — remembered, so the spelled chain, the
    /// canonical chain and the endpoint never ask twice.
    fn write_grant(&self, path: &Path) -> Option<String> {
        if self.platform != AclPlatform::Darwin {
            // Linux is untouched: its reader is an in-process getxattr, so
            // there is nothing to save by remembering it, and a reading
            // that costs nothing is taken fresh every time.
            return acl_write_grant(path, self.platform);
        }
        if let Some(v) = self.seen.borrow().get(path) {
            return v.clone();
        }
        let v = acl_write_grant(path, self.platform);
        self.seen.borrow_mut().insert(path.to_path_buf(), v.clone());
        v
    }

    /// Read the ACLs of every path this question is about to interrogate,
    /// in ONE `ls`. Cheap and safe to over-ask: nothing here decides
    /// anything, it only fills in answers the audit is about to want, in
    /// the audit's own order. Anything unexpected — a validation failure,
    /// a non-zero `ls` (one unlistable operand fails the whole run), a
    /// listing that cannot be attributed line for line — records NOTHING,
    /// and each path is then read on its own by the code that shipped
    /// before this: a batch can lose speed, never a verdict.
    pub(crate) fn prefetch(&self, paths: &[PathBuf]) {
        if self.platform != AclPlatform::Darwin || !trusted_system_binary(LS) {
            return;
        }
        let mut want: Vec<&str> = Vec::new();
        for p in paths {
            // A path `ls` cannot list, or cannot be named in one argv,
            // stays out of the batch and is refused on its own terms.
            let Some(s) = p.to_str() else { continue };
            if want.contains(&s) || self.seen.borrow().contains_key(p.as_path()) {
                continue;
            }
            if rustix::fs::lstat(p).is_err() {
                continue;
            }
            want.push(s);
        }
        if want.len() < 2 {
            return; // one path is the per-path reading, spelled twice
        }
        let out = match trusted_spawn(Path::new(LS))
            .arg("-lde")
            .args(&want)
            .output()
        {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
            _ => return,
        };
        let Some(blocks) = attribute(&out, &want) else {
            return;
        };
        let mut seen = self.seen.borrow_mut();
        for (p, aces) in blocks {
            seen.insert(PathBuf::from(p), acl_write_grant_from(aces));
        }
    }
}

/// `system.posix_acl_access`, read IN-PROCESS via getxattr — no getfacl,
/// no subprocess, nothing PATH-resolved. Ok(None) means exactly one thing:
/// the filesystem answered "no ACL is set here" (NODATA).
#[cfg(target_os = "linux")]
fn read_posix_acl(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let mut buf = vec![0u8; 4096];
    match rustix::fs::getxattr(path, "system.posix_acl_access", &mut buf) {
        Ok(n) => {
            buf.truncate(n);
            Ok(Some(buf))
        }
        Err(e) => acl_xattr_verdict(e),
    }
}

/// Verdict for a failed ACL-xattr read. ONLY NODATA — the documented "no
/// ACL set" answer — passes as clean. OPNOTSUPP is NOT that answer: a
/// filesystem that cannot hold the question cannot attest the directory
/// either (round 8 — it previously laundered through as "no ACL"), so it
/// refuses as unauditable along with every other errno.
/// (Non-Linux production builds never reach it — only the injected-errno
/// test does — hence the narrow dead_code allowance.)
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn acl_xattr_verdict(e: rustix::io::Errno) -> Result<Option<Vec<u8>>, String> {
    if e == rustix::io::Errno::NODATA {
        Ok(None)
    } else {
        Err(format!(
            "could not be audited for ACLs: getxattr failed: {e}"
        ))
    }
}

/// Outside Linux the POSIX arm has no in-process interrogator, and an
/// uninterrogable directory is an UNPROVEN one. (Unreachable in practice:
/// the native platform on macOS is Darwin; this arm exists for the
/// forced-platform tests.)
#[cfg(not(target_os = "linux"))]
fn read_posix_acl(_path: &Path) -> Result<Option<Vec<u8>>, String> {
    Err("could not be audited for ACLs: no in-process POSIX ACL reader on this platform".into())
}

/// The documented binary format of `system.posix_acl_access` (version 2):
/// a u32 LE header, then 8-byte entries of (u16 tag, u16 perm, u32 id).
/// Strict both ways: any NAMED user/group entry carrying the write bit
/// refuses — whoever it names — and anything this parser cannot fully
/// account for (bad length, unknown version, unknown tag) refuses as
/// unauditable. Owner, owning-group, mask, and other entries express
/// through the mode bits, which the mode audit already judges.
fn posix_acl_grant(data: &[u8]) -> Option<String> {
    if data.len() < 4 || !(data.len() - 4).is_multiple_of(8) {
        return Some("could not be audited for ACLs: malformed ACL xattr".into());
    }
    if u32::from_le_bytes([data[0], data[1], data[2], data[3]]) != 2 {
        return Some("could not be audited for ACLs: unknown ACL xattr version".into());
    }
    for e in data[4..].as_chunks::<8>().0 {
        let tag = u16::from_le_bytes([e[0], e[1]]);
        let perm = u16::from_le_bytes([e[2], e[3]]);
        let id = u32::from_le_bytes([e[4], e[5], e[6], e[7]]);
        match tag {
            0x01 | 0x04 | 0x10 | 0x20 => {}
            0x02 | 0x08 => {
                if perm & 0x2 != 0 {
                    let kind = if tag == 0x02 { "user" } else { "group" };
                    return Some(format!(
                        "has an ACL granting {kind} #{id} write, which lets them replace entries regardless of the mode"
                    ));
                }
            }
            _ => return Some("could not be audited for ACLs: unknown ACL entry tag".into()),
        }
    }
    None
}

pub(crate) fn audit_dir(path: &Path, acl: &AclAudit) -> Result<(), String> {
    let st = rustix::fs::stat(path).map_err(|e| {
        format!(
            "refusing to save: {} could not be audited: {e}",
            path.display()
        )
    })?;
    let uid = rustix::process::getuid().as_raw();
    if st.st_uid != 0 && st.st_uid != uid {
        return Err(format!(
            "refusing to save: {} is owned by another user, so it can be replaced underneath the saved path",
            path.display()
        ));
    }
    if st.st_mode & 0o022 != 0 {
        return Err(format!(
            "refusing to save: {} is group- or world-writable, so another principal can replace entries in it",
            path.display()
        ));
    }
    if let Some(why) = acl.write_grant(path) {
        return Err(format!("refusing to save: {} {why}", path.display()));
    }
    Ok(())
}

/// Canonical, so a symlinked component is audited as what it really is; then
/// every ancestor to the root. Shared custody machinery: the update-check
/// cache (update.rs) audits through this too, never through a copy. The
/// chain is collected first so its ACLs are read in one go, then audited
/// leaf-upward in exactly the order it always was.
pub(crate) fn audit_chain(path: &Path, acl: &AclAudit) -> Result<(), String> {
    let chain = chain_of(path);
    acl.prefetch(&chain);
    for d in &chain {
        audit_dir(d, acl)?;
    }
    Ok(())
}

/// A canonical path and every ancestor of it, leaf first — the audit's
/// walk order, as a list.
pub(crate) fn chain_of(path: &Path) -> Vec<PathBuf> {
    let mut q = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = Vec::new();
    loop {
        out.push(q.clone());
        match q.parent() {
            Some(p) if p != q => q = p.to_path_buf(),
            _ => return out,
        }
    }
}

/// The two directories actually written into are judged through the OPEN
/// DESCRIPTOR, which cannot be raced.
fn trusted(fd: &OwnedFd, what: &str) -> Result<rustix::fs::Stat, String> {
    let st = rustix::fs::fstat(fd).map_err(|e| format!("cannot stat {what}: {e}"))?;
    if rustix::fs::FileType::from_raw_mode(st.st_mode) != rustix::fs::FileType::Directory {
        return Err(format!("{what} is not a directory"));
    }
    if st.st_uid != rustix::process::getuid().as_raw() {
        return Err(format!(
            "refusing to save into {what} - it is not owned by you, so another principal could move it mid-save"
        ));
    }
    if st.st_mode & 0o022 != 0 {
        return Err(format!(
            "refusing to save into {what} - it is group- or world-writable, so another principal could replace it mid-save"
        ));
    }
    Ok(st)
}

/// The CURRENT name of the descriptor, asked of the kernel — used only to
/// REPORT where the checked object lives, never to make a decision.
fn fdpath(fd: &OwnedFd, fallback: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Ok(p) = rustix::fs::getpath(fd)
            && let Ok(s) = p.into_string()
        {
            return PathBuf::from(s);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        use std::os::fd::AsRawFd;
        if let Ok(p) = fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd())) {
            return p;
        }
    }
    fallback.to_path_buf()
}

/// Every exit that hands back a pathname goes through here: the string must
/// still name the very object the descriptor checked, by identity (dev, ino)
/// and not by spelling — for a REUSED file exactly as much as a created one.
fn settled(
    dirfd: &OwnedFd,
    where_: &Path,
    st: &rustix::fs::Stat,
    name: &str,
) -> Result<PathBuf, String> {
    let final_path = fdpath(dirfd, where_).join(name);
    match rustix::fs::stat(&final_path) {
        Ok(fs2) if fs2.st_dev == st.st_dev && fs2.st_ino == st.st_ino => Ok(final_path),
        _ => Err(format!(
            "the provider folder changed underneath the save - {} no longer refers to the file that was checked; refusing to hand back a path that moved",
            final_path.display()
        )),
    }
}

/// The saver core: src bytes into `lib[/sub]` as `base.ext` (or the next
/// free `-2`, `-3`, … suffix). Identical content under an occupied name is
/// reused; nothing is ever overwritten. Errors are complete user-facing
/// messages.
pub fn save_into(
    src: &Path,
    lib: &Path,
    sub: &str,
    base: &str,
    ext: &str,
    platform: AclPlatform,
) -> Result<Saved, String> {
    // One save is one custody question: the library chain and the provider
    // chain share ancestors, so they share the reading of them.
    let acl = AclAudit::new(platform);
    let libfd = rustix::fs::open(lib, OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty())
        .map_err(|e| format!("cannot open the wallpaper library {}: {e}", lib.display()))?;
    trusted(&libfd, &lib.display().to_string())?;
    let mut where_ = fdpath(&libfd, lib);
    audit_chain(&where_, &acl)?;

    let dirfd = if !sub.is_empty() {
        // A provider label is ONE component. Anything else is a caller bug,
        // and a caller bug here is a directory traversal.
        if sub == "." || sub == ".." || sub.contains('/') {
            return Err(format!("invalid provider folder {sub}"));
        }
        match rustix::fs::mkdirat(&libfd, sub, Mode::from_raw_mode(0o755)) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(e) => {
                return Err(format!("cannot create {}/{sub}: {e}", lib.display()));
            }
        }
        let sd = rustix::fs::openat(
            &libfd,
            sub,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|e| match e {
            Errno::LOOP | Errno::MLINK | Errno::NOTDIR => format!(
                "refusing to save through {}/{sub} - the provider folder is a symlink, not a directory in the library",
                lib.display()
            ),
            e => format!("cannot open {}/{sub}: {e}", lib.display()),
        })?;
        trusted(&sd, &format!("{}/{sub}", lib.display()))?;
        where_ = fdpath(&sd, &lib.join(sub));
        audit_chain(&where_, &acl)?;
        sd
    } else {
        libfd
    };

    // Independent byte cap, BEFORE the whole-file read: curl's --max-filesize
    // can be defeated by a chunked response with no Content-Length, so the
    // saver re-checks the file on disk and fails closed over the ceiling
    // rather than allocating a Vec of attacker-chosen size.
    if let Ok(m) = fs::metadata(src)
        && m.len() > crate::config::MAX_DOWNLOAD_BYTES
    {
        return Err(format!(
            "refusing to save {} - {} bytes exceeds the {}-byte limit",
            src.display(),
            m.len(),
            crate::config::MAX_DOWNLOAD_BYTES
        ));
    }
    let data = fs::read(src).map_err(|e| format!("cannot read the downloaded file: {e}"))?;

    for n in 1..=99u32 {
        let name = if n == 1 {
            format!("{base}.{ext}")
        } else {
            format!("{base}-{n}.{ext}")
        };
        match rustix::fs::openat(
            &dirfd,
            name.as_str(),
            OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW,
            Mode::from_raw_mode(0o644),
        ) {
            Ok(fd) => {
                let st = rustix::fs::fstat(&fd)
                    .map_err(|e| format!("cannot write {}/{name}: {e}", where_.display()))?;
                let mut f = std::fs::File::from(fd);
                f.write_all(&data)
                    .map_err(|e| format!("cannot write {}/{name}: {e}", where_.display()))?;
                drop(f);
                let _ = rustix::fs::chmodat(
                    &dirfd,
                    name.as_str(),
                    Mode::from_raw_mode(0o644),
                    AtFlags::SYMLINK_NOFOLLOW,
                );
                return settled(&dirfd, &where_, &st, &name).map(Saved::Created);
            }
            Err(Errno::EXIST) => {
                // Occupied. Reuse ONLY a byte-identical regular file that is
                // not a symlink: an alias to identical bytes is still not
                // that file.
                // NONBLOCK so a FIFO planted at the name returns from open
                // immediately (a blocking O_RDONLY FIFO open waits forever for
                // a writer) — the S_ISREG check below then rejects it. On a
                // regular file NONBLOCK is a no-op.
                let Ok(efd) = rustix::fs::openat(
                    &dirfd,
                    name.as_str(),
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK,
                    Mode::empty(),
                ) else {
                    continue;
                };
                let est = match rustix::fs::fstat(&efd) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                if rustix::fs::FileType::from_raw_mode(est.st_mode)
                    != rustix::fs::FileType::RegularFile
                {
                    continue;
                }
                // An oversized occupant cannot equal our (capped) bytes, so do
                // not read it into a Vec to find that out — skip to the next
                // free name instead of matching allocation to its size.
                if est.st_size as u64 > crate::config::MAX_DOWNLOAD_BYTES {
                    continue;
                }
                let mut existing = Vec::new();
                let mut f = std::fs::File::from(efd);
                if f.read_to_end(&mut existing).is_err() {
                    continue;
                }
                if existing == data {
                    return settled(&dirfd, &where_, &est, &name).map(Saved::Reused);
                }
            }
            Err(e) => {
                return Err(format!("cannot write {}/{name}: {e}", where_.display()));
            }
        }
    }
    Err(format!(
        "cannot save into {} - 99 names starting {base} are taken",
        where_.display()
    ))
}

#[cfg(test)]
#[path = "save_tests.rs"]
mod tests;

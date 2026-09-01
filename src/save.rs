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

fn me() -> String {
    Command::new("id")
        .arg("-un")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// A REASON to refuse, or None. Only ALLOW entries granting a write-shaped
/// right to someone who is not us count: a DENY entry restricts rather than
/// grants, and every macOS home carries `group:everyone deny delete`.
/// writesecurity/chown are included: they let a principal grant itself the
/// direct rights a moment later.
fn acl_write_grant(path: &Path, platform: AclPlatform) -> Option<String> {
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
    match platform {
        AclPlatform::Darwin => {
            let user = me();
            let out = match Command::new("/bin/ls").arg("-lde").arg(path).output() {
                Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
                _ => return Some("could not be audited for ACLs: ls -lde failed".into()),
            };
            for ln in out.lines() {
                let t = ln.trim_start();
                // ACE lines are numbered: `N: <who> allow|deny <perms>`.
                let Some((num, ace)) = t.split_once(':') else {
                    continue;
                };
                if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
                    continue;
                }
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
                if who == format!("user:{user}") || who == user {
                    continue;
                }
                return Some(format!(
                    "has an ACL granting {who} {}, which lets them replace entries regardless of the mode",
                    hit.join(",")
                ));
            }
            None
        }
        AclPlatform::Posix => posix_acl_audit(path, which("getfacl").as_deref(), &me()),
    }
}

/// The posix half of the audit, pure over its inputs so the forced-platform
/// tests can drive it on any OS: `getfacl` is the interrogator to run (None
/// = not installed), `user` is who we are.
fn posix_acl_audit(path: &Path, getfacl: Option<&Path>, user: &str) -> Option<String> {
    let Some(getfacl) = getfacl else {
        // An uninterrogable directory is an UNPROVEN one. Failing open here
        // silently downgrades every non-darwin install to the mode-only
        // check the review rejected.
        return Some(
            "cannot be audited for ACLs: getfacl is not installed - install it (Debian/Ubuntu: apt install acl)"
                .into(),
        );
    };
    let out = match Command::new(getfacl).arg("-cp").arg(path).output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => return Some("could not be audited for ACLs: getfacl failed".into()),
    };
    for ln in out.lines() {
        let f: Vec<&str> = ln.trim().split(':').collect();
        if f.len() < 3 || ln.starts_with('#') {
            continue;
        }
        let (kind, who, perm) = (f[0], f[1], f[2]);
        if !(kind == "user" || kind == "group") || who.is_empty() || who == user {
            continue;
        }
        if perm.contains('w') {
            return Some(format!(
                "has an ACL granting {kind}:{who} write, which lets them replace entries regardless of the mode"
            ));
        }
    }
    None
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|c| c.is_file())
}

fn audit_dir(path: &Path, platform: AclPlatform) -> Result<(), String> {
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
    if let Some(why) = acl_write_grant(path, platform) {
        return Err(format!("refusing to save: {} {why}", path.display()));
    }
    Ok(())
}

/// Canonical, so a symlinked component is audited as what it really is; then
/// every ancestor to the root.
fn audit_chain(path: &Path, platform: AclPlatform) -> Result<(), String> {
    let mut q = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    loop {
        audit_dir(&q, platform)?;
        match q.parent() {
            Some(p) if p != q => q = p.to_path_buf(),
            _ => return Ok(()),
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
    let libfd = rustix::fs::open(lib, OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty())
        .map_err(|e| format!("cannot open the wallpaper library {}: {e}", lib.display()))?;
    trusted(&libfd, &lib.display().to_string())?;
    let mut where_ = fdpath(&libfd, lib);
    audit_chain(&where_, platform)?;

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
        audit_chain(&where_, platform)?;
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

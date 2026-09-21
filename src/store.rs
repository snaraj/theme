//! Small private data records sharing the updater's audited cache custody.
//! Mode bits alone do not prevent inherited ACLs from exposing new records.
use crate::config::Config;
use rustix::fs::{AtFlags, Mode, OFlags};
use std::fs::File;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};

const LIMIT: usize = 32 * 1024 * 1024;
static SERIAL: AtomicU64 = AtomicU64::new(0);

#[cfg(target_os = "linux")]
fn no_acl(fd: &rustix::fd::OwnedFd, directory: bool) -> bool {
    let names = ["system.posix_acl_access", "system.posix_acl_default"];
    names[..if directory { 2 } else { 1 }].iter().all(|name| {
        let mut byte = [0u8; 1];
        // Only a positively absent attribute passes. Present, oversized,
        // unsupported, and unreadable ACLs all refuse persistence.
        matches!(
            rustix::fs::fgetxattr(fd, *name, byte.as_mut_slice()),
            Err(rustix::io::Errno::NODATA)
        )
    })
}

fn private_directory(fd: &rustix::fd::OwnedFd) -> bool {
    #[cfg(target_os = "linux")]
    return no_acl(fd, true);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = fd;
        true
    }
}

#[cfg(target_os = "macos")]
fn prepare_private_file(dir: &rustix::fd::OwnedFd, name: &str, fd: &rustix::fd::OwnedFd) -> bool {
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::process::Stdio;

    const CHMOD: &str = "/bin/chmod";
    let Some((dirpath, before)) = rustix::fs::getpath(dir)
        .ok()
        .zip(rustix::fs::fstat(fd).ok())
    else {
        return false;
    };
    let path = Path::new(std::ffi::OsStr::from_bytes(dirpath.to_bytes())).join(name);
    let matches_empty = |st: rustix::fs::Stat| {
        st.st_dev == before.st_dev
            && st.st_ino == before.st_ino
            && rustix::fs::FileType::from_raw_mode(st.st_mode) == rustix::fs::FileType::RegularFile
            && st.st_uid == rustix::process::getuid().as_raw()
            && st.st_mode & 0o077 == 0
            && st.st_size == 0
    };
    let bound = || {
        rustix::fs::lstat(&path).is_ok_and(matches_empty)
            && rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW).is_ok_and(matches_empty)
            && rustix::fs::fstat(fd).is_ok_and(matches_empty)
    };
    // The audited directory excludes foreign writers. Bind the native
    // operation to this still-empty inode before and after it. chmod -N
    // positively clears ACLs; ls cannot distinguish every ACL lookup error
    // from absence, and /dev/fd metadata does not expose the real file ACL.
    crate::save::trusted_system_binary(CHMOD)
        && bound()
        && crate::save::trusted_spawn(Path::new(CHMOD))
            .arg("-N")
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
        && bound()
}

#[cfg(not(target_os = "macos"))]
fn prepare_private_file(_dir: &rustix::fd::OwnedFd, _name: &str, fd: &rustix::fd::OwnedFd) -> bool {
    #[cfg(target_os = "linux")]
    return no_acl(fd, false);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = fd;
        false
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

pub fn read(cfg: &Config, name: &str, max: usize) -> Option<Vec<u8>> {
    if !valid_name(name) || !cfg.cache_dir.is_dir() {
        return None;
    }
    let dir = crate::update::check_dir(cfg)?;
    read_at(&dir, name, max)
}

/// The caller retains the audited directory for multiple records in one screen.
pub(crate) fn read_at(dir: &rustix::fd::OwnedFd, name: &str, max: usize) -> Option<Vec<u8>> {
    if !valid_name(name) || !private_directory(dir) {
        return None;
    }
    let fd = rustix::fs::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .ok()?;
    let st = rustix::fs::fstat(&fd).ok()?;
    let max = max.min(LIMIT);
    if rustix::fs::FileType::from_raw_mode(st.st_mode) != rustix::fs::FileType::RegularFile
        || st.st_uid != rustix::process::getuid().as_raw()
        || st.st_mode & 0o077 != 0
        || st.st_size < 0
        || st.st_size as u64 > max as u64
    {
        return None;
    }
    #[cfg(target_os = "linux")]
    if !no_acl(&fd, false) {
        return None;
    }
    // macOS reads retain the original owner/mode/custody checks. Reading an
    // existing record does not change its ACL or grant access to other users.
    let mut bytes = Vec::new();
    File::from(fd)
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= max).then_some(bytes)
}

pub fn write(cfg: &Config, name: &str, bytes: &[u8]) -> Result<(), String> {
    if !valid_name(name) || bytes.len() > LIMIT {
        return Err("invalid or oversized cache record".into());
    }
    if cfg.no_apply {
        return Ok(());
    }
    let dir = crate::update::check_dir(cfg).ok_or("cache directory is not private and trusted")?;
    write_at(&dir, name, bytes)
}

pub(crate) fn write_at(dir: &rustix::fd::OwnedFd, name: &str, bytes: &[u8]) -> Result<(), String> {
    if !valid_name(name) || bytes.len() > LIMIT {
        return Err("invalid or oversized cache record".into());
    }
    if !private_directory(dir) {
        return Err("cache directory ACL privacy could not be established".into());
    }
    let temp = format!(
        ".{name}.{}.{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    );
    let fd = rustix::fs::openat(
        dir,
        temp.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|e| format!("cannot create cache record: {e}"))?;
    finish_write(dir, &temp, name, fd, bytes)
}

fn finish_write(
    dir: &rustix::fd::OwnedFd,
    temp: &str,
    name: &str,
    fd: rustix::fd::OwnedFd,
    bytes: &[u8],
) -> Result<(), String> {
    let result = (|| {
        if !prepare_private_file(dir, temp, &fd) {
            return Err("cache record ACL privacy could not be established".into());
        }
        let mut file = File::from(fd);
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        rustix::fs::renameat(dir, temp, dir, name).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = rustix::fs::unlinkat(dir, temp, AtFlags::empty());
    }
    result
}

/// Length framing preserves whitespace and arbitrary UTF-8 without delimiters.
pub fn pack(fields: &[String]) -> Vec<u8> {
    let mut out = b"theme-record-1\n".to_vec();
    for field in fields {
        out.extend_from_slice(&(field.len() as u64).to_le_bytes());
        out.extend_from_slice(field.as_bytes());
    }
    out
}

pub fn unpack(bytes: &[u8]) -> Option<Vec<String>> {
    if bytes.len() > LIMIT {
        return None;
    }
    let mut rest = bytes.strip_prefix(b"theme-record-1\n")?;
    let mut fields = Vec::new();
    while !rest.is_empty() {
        let n = usize::try_from(u64::from_le_bytes(rest.get(..8)?.try_into().ok()?)).ok()?;
        rest = &rest[8..];
        fields.push(std::str::from_utf8(rest.get(..n)?).ok()?.to_owned());
        rest = &rest[n..];
        if fields.len() > 500_000 {
            return None;
        }
    }
    Some(fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target/test-tmp")
                .join(format!(
                    "store-{}-{}",
                    std::process::id(),
                    SERIAL.fetch_add(1, Ordering::Relaxed)
                ));
            std::fs::create_dir_all(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
        fn config(&self) -> Config {
            Config {
                wallpaper_dirs: Vec::new(),
                wallpaper_dirs_display: String::new(),
                cache_dir: self.0.join("cache"),
                kitty_dir: self.0.clone(),
                current: self.0.join("current-theme.conf"),
                formats: Vec::new(),
                contrast: 4.5,
                no_apply: false,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn ordinary_private_records_persist_and_no_apply_writes_nothing() {
        let fixture = Fixture::new();
        let mut cfg = fixture.config();
        cfg.no_apply = true;
        write(&cfg, "history-v1", b"private fixture").unwrap();
        assert!(!cfg.cache_dir.exists());
        cfg.no_apply = false;
        write(&cfg, "history-v1", b"private fixture").unwrap();
        assert_eq!(read(&cfg, "history-v1", 100).unwrap(), b"private fixture");
        assert_eq!(
            std::fs::metadata(cfg.cache_dir.join("history-v1"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(read(&cfg, "history-v1", 2).is_none());
        write(&cfg, "history-v1", b"replacement").unwrap();
        assert_eq!(read(&cfg, "history-v1", 100).unwrap(), b"replacement");
        assert_eq!(std::fs::read_dir(&cfg.cache_dir).unwrap().count(), 1);
    }

    #[test]
    fn audited_descriptor_rejects_hostile_record_replacements() {
        let fixture = Fixture::new();
        let cfg = fixture.config();
        write(&cfg, "desktop", b"private").unwrap();
        let dir = crate::update::check_dir(&cfg).unwrap();
        let path = cfg.cache_dir.join("desktop");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_at(&dir, "desktop", 100).is_none());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let target = fixture.0.join("outside-record");
        std::fs::rename(&path, &target).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(read_at(&dir, "desktop", 100).is_none());
        std::fs::remove_file(&path).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        assert!(read_at(&dir, "desktop", 100).is_none());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(read_at(&dir, "desktop", 100).is_none());
        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(&target, &path).unwrap();
        assert_eq!(read_at(&dir, "desktop", 100).unwrap(), b"private");
    }

    #[cfg(target_os = "macos")]
    fn set_acl(path: &Path, acl: &str) {
        assert!(
            std::process::Command::new("/bin/chmod")
                .args(["+a", acl])
                .arg(path)
                .status()
                .unwrap()
                .success()
        );
    }

    #[cfg(target_os = "macos")]
    fn acl_listing(path: &Path) -> String {
        let output = std::process::Command::new("/bin/ls")
            .arg("-ldne")
            .arg(path)
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_clears_inherited_read_acl_before_persisting() {
        let fixture = Fixture::new();
        let cfg = fixture.config();
        std::fs::create_dir(&cfg.cache_dir).unwrap();
        set_acl(
            &cfg.cache_dir,
            "everyone allow read,file_inherit,only_inherit",
        );
        let control = cfg.cache_dir.join("empty-control");
        let empty = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&control)
            .unwrap();
        assert!(acl_listing(&control).contains(" inherited allow read"));
        drop(empty);
        std::fs::remove_file(control).unwrap();
        write(&cfg, "history-v1", b"private fixture").unwrap();
        assert_eq!(
            acl_listing(&cfg.cache_dir.join("history-v1"))
                .lines()
                .count(),
            1
        );
        assert_eq!(read(&cfg, "history-v1", 100).unwrap(), b"private fixture");
        assert_eq!(std::fs::read_dir(&cfg.cache_dir).unwrap().count(), 1);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_refuses_to_clear_a_nonempty_or_different_record() {
        let fixture = Fixture::new();
        let cfg = fixture.config();
        write(&cfg, "history-v1", b"private fixture").unwrap();
        let dir = crate::update::check_dir(&cfg).unwrap();
        let fd = rustix::fs::openat(&dir, "history-v1", OFlags::RDONLY, Mode::empty()).unwrap();
        assert!(!prepare_private_file(&dir, "history-v1", &fd));
        let empty = rustix::fs::openat(
            &dir,
            "empty",
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
            Mode::from_raw_mode(0o600),
        )
        .unwrap();
        assert!(!prepare_private_file(&dir, "history-v1", &empty));
        let temporary = cfg.cache_dir.join("temporary");
        std::fs::write(&temporary, b"").unwrap();
        assert!(finish_write(&dir, "temporary", "history-v1", empty, b"unwritten").is_err());
        assert!(
            !temporary.exists(),
            "the refused empty temporary is removed"
        );
        assert_eq!(
            std::fs::metadata(cfg.cache_dir.join("empty"))
                .unwrap()
                .len(),
            0
        );
        assert_eq!(read(&cfg, "history-v1", 100).unwrap(), b"private fixture");
    }

    #[cfg(target_os = "linux")]
    fn set_posix_acl(fd: &rustix::fd::OwnedFd, default: bool) {
        let mut acl = 2u32.to_le_bytes().to_vec();
        // A masked named read entry is still an ACL and conservatively refused.
        for (tag, permissions, id) in [
            (1u16, 6u16, u32::MAX),
            (2, 4, rustix::process::getuid().as_raw() + 1),
            (4, 0, u32::MAX),
            (16, 0, u32::MAX),
            (32, 0, u32::MAX),
        ] {
            acl.extend_from_slice(&tag.to_le_bytes());
            acl.extend_from_slice(&permissions.to_le_bytes());
            acl.extend_from_slice(&id.to_le_bytes());
        }
        rustix::fs::fsetxattr(
            fd,
            if default {
                "system.posix_acl_default"
            } else {
                "system.posix_acl_access"
            },
            &acl,
            rustix::fs::XattrFlags::empty(),
        )
        .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_refuses_access_and_default_acls() {
        let fixture = Fixture::new();
        let cfg = fixture.config();
        write(&cfg, "history-v1", b"private fixture").unwrap();
        let dir = crate::update::check_dir(&cfg).unwrap();
        let fd = rustix::fs::openat(&dir, "history-v1", OFlags::RDONLY, Mode::empty()).unwrap();
        set_posix_acl(&fd, false);
        assert!(!no_acl(&fd, false));
        assert!(read(&cfg, "history-v1", 100).is_none());
        set_posix_acl(&dir, true);
        assert!(!private_directory(&dir));
        assert!(write(&cfg, "another-v1", b"private fixture").is_err());
        assert_eq!(std::fs::read_dir(&cfg.cache_dir).unwrap().count(), 1);
    }

    #[test]
    fn framing_preserves_names_and_rejects_partial_records() {
        let fields = vec!["ocean\nwith\ttabs 🌊".into(), String::new(), "last".into()];
        let data = pack(&fields);
        assert_eq!(unpack(&data), Some(fields));
        assert!(unpack(&data[..data.len() - 1]).is_none());
        assert!(unpack(b"theme-record-1\n\xff\xff\xff\xff\xff\xff\xff\xff").is_none());
    }
    #[test]
    fn record_names_are_single_fixed_components() {
        for name in [
            "",
            "../elsewhere",
            "/absolute",
            ".hidden",
            "two/parts",
            "with space",
        ] {
            assert!(!valid_name(name));
        }
        assert!(valid_name("library-index-v1"));
    }
}

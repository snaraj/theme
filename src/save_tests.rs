//! The saver's trust decisions, driven directly — including the two
//! check/use windows the shell fixture opened with a FIFO, and the
//! forced-posix ACL branch that must run on every platform.

use super::*;
use std::os::unix::fs::PermissionsExt;

fn tmpdir(name: &str) -> PathBuf {
    // Not env::temp_dir(): on Linux runners /tmp is world-writable (1777),
    // and the saver's ancestor audit — correctly — refuses any library whose
    // chain contains such a directory. The workspace target/ dir sits on a
    // user-owned chain wherever the repo is sanely checked out.
    let d = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-tmp")
        .join(format!("save-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// Deterministic check/use window driver. A FIFO's nonblocking writer open
/// succeeds exactly when a reader is present — and the saver only reads the
/// source AFTER the provider directory was opened and validated — so waiting
/// for that open to succeed, THEN swapping, THEN delivering the bytes puts
/// the swap inside the window with no sleeps and no races. Gives up after
/// 10s so a saver that refused early (and so never reads) makes the test
/// FAIL on its assertions instead of deadlocking a blocked writer.
fn swap_then_feed(fifo: &Path, swap: impl FnOnce(), bytes: &[u8]) {
    use rustix::fs::{Mode, OFlags};
    use std::io::Write;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let fd = loop {
        match rustix::fs::open(fifo, OFlags::WRONLY | OFlags::NONBLOCK, Mode::empty()) {
            Ok(fd) => break fd,
            Err(rustix::io::Errno::NXIO) => {
                if std::time::Instant::now() >= deadline {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => panic!("fifo writer open: {e}"),
        }
    };
    swap();
    std::fs::File::from(fd).write_all(bytes).unwrap();
}

#[test]
fn creates_reuses_and_suffixes() {
    let d = tmpdir("basic");
    let src = d.join("src.bin");
    fs::write(&src, b"bytes-one").unwrap();
    let lib = d.join("lib");
    fs::create_dir(&lib).unwrap();
    let Saved::Created(p1) = save_into(&src, &lib, "", "pic", "png", native_platform()).unwrap()
    else {
        panic!("expected create")
    };
    assert!(p1.ends_with("pic.png"));
    // Identical bytes: reused, not rewritten.
    let Saved::Reused(p2) = save_into(&src, &lib, "", "pic", "png", native_platform()).unwrap()
    else {
        panic!("expected reuse")
    };
    assert_eq!(p1, p2);
    // Different bytes: next free suffix, never overwrite.
    fs::write(&src, b"bytes-two").unwrap();
    let Saved::Created(p3) = save_into(&src, &lib, "", "pic", "png", native_platform()).unwrap()
    else {
        panic!("expected suffixed create")
    };
    assert!(p3.ends_with("pic-2.png"));
    assert_eq!(fs::read(&p1).unwrap(), b"bytes-one");
}

#[test]
fn provider_symlink_is_refused_and_untouched() {
    let d = tmpdir("provsym");
    let lib = d.join("lib");
    let out = d.join("out");
    fs::create_dir_all(&lib).unwrap();
    fs::create_dir_all(&out).unwrap();
    std::os::unix::fs::symlink(&out, lib.join("unsplash")).unwrap();
    let src = d.join("src.bin");
    fs::write(&src, b"x").unwrap();
    let err = save_into(&src, &lib, "unsplash", "pic", "png", native_platform()).unwrap_err();
    assert!(err.contains("refusing to save"), "{err}");
    assert!(fs::read_dir(&out).unwrap().next().is_none());
    assert!(
        lib.join("unsplash")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn traversal_shaped_provider_labels_are_refused() {
    let d = tmpdir("trav");
    let lib = d.join("lib");
    fs::create_dir_all(&lib).unwrap();
    let src = d.join("src.bin");
    fs::write(&src, b"x").unwrap();
    for sub in [".", "..", "a/b"] {
        let err = save_into(&src, &lib, sub, "pic", "png", native_platform()).unwrap_err();
        assert!(err.contains("invalid provider folder"), "{sub}: {err}");
    }
}

#[test]
fn world_writable_ancestor_is_refused() {
    let d = tmpdir("chain");
    let parent = d.join("parent");
    let lib = parent.join("lib");
    fs::create_dir_all(lib.join("unsplash")).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o777)).unwrap();
    let src = d.join("src.bin");
    fs::write(&src, b"x").unwrap();
    let err = save_into(&src, &lib, "unsplash", "pic", "png", native_platform()).unwrap_err();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(err.contains("group- or world-writable"), "{err}");
    assert!(!lib.join("unsplash/pic.png").exists());
}

/// The check/use window, opened deterministically: the source is a FIFO, so
/// the saver blocks in fs::read AFTER the provider directory was opened and
/// validated. The swap happens while it is blocked — and the bytes must
/// land in the directory that was CHECKED, with the returned path naming it
/// (or a refusal), never the attacker's symlink target.
#[test]
fn mid_save_provider_swap_writes_nothing_outside() {
    let d = tmpdir("swap");
    let lib = d.join("lib");
    let outside = d.join("outside");
    fs::create_dir_all(lib.join("unsplash")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    let fifo = d.join("src.fifo");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let libc = lib.clone();
    let outc = outside.clone();
    let fifoc = fifo.clone();
    let swapper = std::thread::spawn(move || {
        swap_then_feed(
            &fifoc,
            || {
                fs::rename(libc.join("unsplash"), libc.join("checked-dir")).unwrap();
                std::os::unix::fs::symlink(&outc, libc.join("unsplash")).unwrap();
            },
            b"png-bytes",
        );
    });
    let res = save_into(&fifo, &lib, "unsplash", "pic", "png", native_platform());
    swapper.join().unwrap();
    assert!(
        fs::read_dir(&outside).unwrap().next().is_none(),
        "write escaped the library"
    );
    match res {
        Ok(Saved::Created(p)) => {
            assert!(
                p.display().to_string().contains("checked-dir"),
                "returned path must name the checked directory, got {}",
                p.display()
            );
            assert_eq!(
                fs::read(lib.join("checked-dir/pic.png")).unwrap(),
                b"png-bytes"
            );
        }
        Err(e) => assert!(e.contains("changed underneath the save"), "{e}"),
        Ok(Saved::Reused(p)) => panic!("unexpected reuse at {}", p.display()),
    }
}

/// The REUSE arm is a separate exit and needs its own mutant: identical
/// bytes on both sides of the swap, and the handed-back path must read the
/// CHECKED file, never the attacker copy.
#[test]
fn reuse_under_a_swap_never_reads_the_attacker_copy() {
    let d = tmpdir("reuse");
    let lib = d.join("lib");
    let outside = d.join("outside");
    fs::create_dir_all(lib.join("unsplash")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(lib.join("unsplash/pic.png"), b"trusted-bytes").unwrap();
    fs::write(outside.join("pic.png"), b"attacker-bytes").unwrap();
    let fifo = d.join("src.fifo");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let libc = lib.clone();
    let outc = outside.clone();
    let fifoc = fifo.clone();
    let swapper = std::thread::spawn(move || {
        swap_then_feed(
            &fifoc,
            || {
                fs::rename(libc.join("unsplash"), libc.join("checked-dir")).unwrap();
                std::os::unix::fs::symlink(&outc, libc.join("unsplash")).unwrap();
            },
            b"trusted-bytes",
        );
    });
    let res = save_into(&fifo, &lib, "unsplash", "pic", "png", native_platform());
    swapper.join().unwrap();
    match res {
        Ok(Saved::Reused(p)) => {
            assert_eq!(fs::read(&p).unwrap(), b"trusted-bytes");
            assert!(!p.starts_with(&outside));
        }
        Err(e) => assert!(e.contains("changed underneath the save"), "{e}"),
        Ok(Saved::Created(p)) => panic!("unexpected create at {}", p.display()),
    }
    assert_eq!(
        fs::read(lib.join("checked-dir/pic.png")).unwrap(),
        b"trusted-bytes"
    );
}

/// The posix ACL parser, pure over the raw xattr bytes on every platform,
/// with NO subprocess anywhere (round 7 removed getfacl and `id` from the
/// audit): a named write grant refuses whoever it names, read-only and
/// base entries pass, and anything unparseable refuses as unauditable.
#[test]
fn forced_posix_acl_predicate() {
    let entry = |tag: u16, perm: u16, id: u32| {
        let mut v = Vec::new();
        v.extend(tag.to_le_bytes());
        v.extend(perm.to_le_bytes());
        v.extend(id.to_le_bytes());
        v
    };
    let acl = |entries: &[Vec<u8>]| {
        let mut v = 2u32.to_le_bytes().to_vec();
        for e in entries {
            v.extend(e);
        }
        v
    };
    // Base entries only (owner/owning-group/other/mask express through the
    // mode bits, which the mode audit judges): clean.
    let base = [
        entry(0x01, 7, u32::MAX),
        entry(0x04, 5, u32::MAX),
        entry(0x20, 5, u32::MAX),
    ];
    assert_eq!(posix_acl_grant(&acl(&base)), None);
    // A named user with the write bit refuses — identity-free, so even a
    // grant to "ourselves" counts (out of contract, documented).
    let why = posix_acl_grant(&acl(&[
        entry(0x01, 7, u32::MAX),
        entry(0x02, 6, 1000),
        entry(0x10, 7, u32::MAX),
    ]))
    .unwrap();
    assert!(why.contains("user #1000 write"), "{why}");
    // A named group likewise.
    let why = posix_acl_grant(&acl(&[entry(0x08, 2, 50)])).unwrap();
    assert!(why.contains("group #50 write"), "{why}");
    // A named read-only entry does not.
    assert_eq!(posix_acl_grant(&acl(&[entry(0x02, 4, 1000)])), None);
    // Unparseable input is UNPROVEN input: truncated, wrong version,
    // unknown tag all refuse.
    let why = posix_acl_grant(&[1, 2, 3]).unwrap();
    assert!(why.contains("malformed"), "{why}");
    let mut wrong = acl(&base);
    wrong[0] = 1;
    let why = posix_acl_grant(&wrong).unwrap();
    assert!(why.contains("unknown ACL xattr version"), "{why}");
    let why = posix_acl_grant(&acl(&[entry(0x40, 2, 5)])).unwrap();
    assert!(why.contains("unknown ACL entry tag"), "{why}");
}

/// Round 9: every spawn inside the security boundary starts from an EMPTY
/// environment. TLS-trust substitution (CURL_CA_BUNDLE/SSL_CERT_FILE to
/// /dev/null flips a working request to error 77) and loader injection
/// (LD_PRELOAD/LD_AUDIT/LD_LIBRARY_PATH, DYLD_* on macOS) all travel by
/// inherited env, so the strongest proof is that NOTHING crosses: env(1)
/// under the boundary prints not one variable, while this parent
/// demonstrably carries PATH at minimum.
#[test]
fn boundary_spawns_carry_no_environment() {
    assert!(std::env::var_os("PATH").is_some());
    let out = trusted_spawn(Path::new("/usr/bin/env")).output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "");
}

/// The xattr-read errno mapping, driven with INJECTED errnos (round 8):
/// only NODATA — the filesystem's actual "no ACL set" answer — passes.
/// OPNOTSUPP means the interrogation itself is unsupported, which is
/// inability to audit, not a clean bill; it and every other errno refuse.
#[test]
fn acl_xattr_errno_mapping() {
    use rustix::io::Errno;
    assert!(matches!(acl_xattr_verdict(Errno::NODATA), Ok(None)));
    let unsup = acl_xattr_verdict(Errno::OPNOTSUPP).unwrap_err();
    assert!(unsup.contains("could not be audited"), "{unsup}");
    let denied = acl_xattr_verdict(Errno::ACCESS).unwrap_err();
    assert!(denied.contains("could not be audited"), "{denied}");
}

/// macOS-native ACL semantics, end-to-end where chmod +a exists: an ALLOW
/// grant to another principal refuses, a DENY ace is not mistaken for one,
/// and writesecurity (ACL administration) counts as a grant.
#[test]
#[cfg(target_os = "macos")]
fn darwin_acl_grants() {
    let d = tmpdir("acl");
    let lib = d.join("lib");
    fs::create_dir_all(&lib).unwrap();
    let src = d.join("src.bin");
    fs::write(&src, b"x").unwrap();
    let chmod_a = |spec: &str, on: bool| {
        Command::new("chmod")
            .arg(if on { "+a" } else { "-a" })
            .arg(spec)
            .arg(&lib)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    if !chmod_a("everyone allow add_file,delete_child", true) {
        eprintln!("SKIP darwin_acl_grants: chmod +a unavailable here");
        return;
    }
    let err = save_into(&src, &lib, "", "a", "png", AclPlatform::Darwin).unwrap_err();
    assert!(err.contains("ACL granting"), "{err}");
    chmod_a("everyone allow add_file,delete_child", false);

    chmod_a("everyone deny delete", true);
    let ok = save_into(&src, &lib, "", "b", "png", AclPlatform::Darwin);
    chmod_a("everyone deny delete", false);
    assert!(
        matches!(ok, Ok(Saved::Created(_))),
        "a DENY ace blocked the save: {ok:?}"
    );

    chmod_a("everyone allow writesecurity", true);
    let err = save_into(&src, &lib, "", "c", "png", AclPlatform::Darwin).unwrap_err();
    chmod_a("everyone allow writesecurity", false);
    assert!(err.contains("writesecurity"), "{err}");
}

/// A FIFO planted at the first collision name must not hang the save: the
/// reuse arm opens NONBLOCK and the S_ISREG check rejects it, so the saver
/// steps to the next free name and returns promptly. Removing either the
/// NONBLOCK open or the regular-file check reintroduces the deadlock — the
/// bounded join below then fails instead of blocking the whole suite.
#[test]
fn a_fifo_at_the_collision_name_does_not_hang_the_save() {
    let d = tmpdir("fifo-collide");
    let lib = d.join("lib");
    fs::create_dir_all(lib.join("unsplash")).unwrap();
    // Occupy the first name with a FIFO (no writer will ever open it).
    let victim = lib.join("unsplash/pic.png");
    assert!(
        Command::new("mkfifo")
            .arg(&victim)
            .status()
            .unwrap()
            .success()
    );
    let src = d.join("src.bin");
    fs::write(&src, b"real-bytes").unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let r = save_into(&src, &lib, "unsplash", "pic", "png", native_platform());
        let _ = tx.send(r.map(|s| format!("{s:?}")));
    });
    match rx.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(Ok(desc)) => assert!(desc.contains("pic-2.png"), "stepped somewhere odd: {desc}"),
        Ok(Err(e)) => panic!("unexpected refusal: {e}"),
        Err(_) => panic!("save hung on the planted FIFO"),
    }
}

/// The download byte cap is enforced from the saver, before the whole-file
/// read: a source one byte over the ceiling is refused without allocating a
/// Vec its size (a sparse set_len makes the oversized file for free, and the
/// refusal returns before the read). The boundary is inclusive by
/// construction (`>` MAX), and small-file acceptance is proven throughout the
/// rest of this suite.
#[test]
fn an_oversized_source_is_refused_before_allocation() {
    let d = tmpdir("oversize");
    let lib = d.join("lib");
    fs::create_dir(&lib).unwrap();
    let big = d.join("big.bin");
    let f = fs::File::create(&big).unwrap();
    f.set_len(crate::config::MAX_DOWNLOAD_BYTES + 1).unwrap();
    drop(f);
    let err = save_into(&big, &lib, "", "pic", "png", native_platform()).unwrap_err();
    assert!(err.contains("exceeds the"), "{err}");
    assert!(!lib.join("pic.png").exists());
}

/// Scratch files live inside a private, owner-only (0700) directory — the
/// containment that makes a planted-symlink clobber impossible. A mutant
/// returning a bare $TMPDIR name fails these invariants.
#[test]
fn scratch_paths_are_inside_a_private_owner_only_dir() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let a = crate::scratch::new();
    let b = crate::scratch::new();
    assert_ne!(a, b, "two scratch names collided");
    let dir = a.parent().unwrap();
    assert_eq!(dir, b.parent().unwrap(), "scratch files escaped the dir");
    let m = fs::symlink_metadata(dir).unwrap();
    assert!(m.file_type().is_dir(), "scratch dir is not a directory");
    assert_eq!(
        m.permissions().mode() & 0o777,
        0o700,
        "scratch dir not 0700"
    );
    assert_eq!(
        m.uid(),
        rustix::process::getuid().as_raw(),
        "not owner-only"
    );
    crate::scratch::cleanup();
}

impl std::fmt::Debug for Saved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Saved::Created(p) => write!(f, "Created({})", p.display()),
            Saved::Reused(p) => write!(f, "Reused({})", p.display()),
        }
    }
}

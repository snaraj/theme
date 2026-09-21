//! One registry of scratch files, swept explicitly at normal command exit,
//! `die`, and broken stdout. Panics and external termination do not sweep it.
//!
//! Every scratch file lives inside a per-process directory created 0700 and
//! owned by us. Because no other user can traverse or create entries in it,
//! a scratch pathname cannot be pre-empted by a planted symlink — closing the
//! TOCTOU window a bare $TMPDIR name left open to `curl -o`/`-D`, `fs::copy`,
//! and the in-place transformers, all of which write by pathname.

use crate::ui::die;
use rustix::fs::Mode;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
static SEQ: AtomicU32 = AtomicU32::new(0);

/// This process's private scratch directory, created once at 0700. `mkdir`
/// is atomic and fails if the name already exists (as a directory, file, or
/// symlink), so a squatter cannot pre-create it; we try fresh names before
/// giving up.
fn scratch_dir() -> PathBuf {
    let mut slot = DIR.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(d) = slot.as_ref() {
        return d.clone();
    }
    let base = std::env::temp_dir();
    let mut last = String::new();
    for attempt in 0..64u32 {
        let name = format!(
            "theme.{}.{:x}.{attempt}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let d = base.join(&name);
        match rustix::fs::mkdir(&d, Mode::from_raw_mode(0o700)) {
            Ok(()) => {
                *slot = Some(d.clone());
                return d;
            }
            Err(e) => last = e.to_string(),
        }
    }
    drop(slot);
    die(&format!(
        "cannot create a private scratch directory in {}: {last}",
        base.display()
    ))
}

/// A fresh scratch file path inside the private directory. The path does not
/// exist yet; its writer (curl/copy) creates it, and the 0700 parent is what
/// guarantees no attacker file or symlink can already occupy the name.
pub fn new() -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    scratch_dir().join(format!("s{n}"))
}

/// Delete one scratch file eagerly (it lives inside the private directory).
pub fn done(p: &Path) {
    let _ = std::fs::remove_file(p);
}

/// Remove the whole private directory — called by `die` and at normal exit.
/// try_lock, never block: this runs on the exit path, possibly re-entered
/// from `die` while another frame holds the lock.
pub fn cleanup() {
    if let Ok(mut slot) = DIR.try_lock()
        && let Some(d) = slot.take()
    {
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[test]
fn paused_transaction_modules_cannot_print_or_exit() {
    for source in [include_str!("spaces.rs"), include_str!("save.rs")] {
        for line in source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
        {
            // These modules may not acquire the process-exiting stdout shim.
            for forbidden in [
                "print!",
                "println!",
                "print !",
                "println !",
                "ui::out(",
                "process::exit(",
            ] {
                assert!(
                    !line.contains(forbidden),
                    "printing/exit in a transaction module: {line}"
                );
            }
        }
    }
    // spaces itself must propagate errors so Paused::drop/finish always runs.
    let spaces = include_str!("spaces.rs");
    assert!(!spaces.contains("ui::die(") && !spaces.contains("ui::note("));
}

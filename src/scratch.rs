//! One registry of scratch files, swept on EVERY exit path — `die` calls
//! [`cleanup`] before the process exits, so a failure mid-transform leaves
//! nothing behind (the shell's one top-level EXIT trap, made explicit).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

static SCRATCHES: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// A fresh scratch file path under $TMPDIR, registered for sweep.
pub fn new() -> PathBuf {
    let dir = std::env::temp_dir();
    let name = format!(
        "theme.{}.{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let p = dir.join(name);
    if let Ok(mut v) = SCRATCHES.lock() {
        v.push(p.clone());
    }
    p
}

/// Delete one scratch eagerly and deregister it.
pub fn done(p: &Path) {
    let _ = std::fs::remove_file(p);
    if let Ok(mut v) = SCRATCHES.lock() {
        v.retain(|q| q != p);
    }
}

/// Delete everything still registered — called by `die` and at normal exit.
pub fn cleanup() {
    if let Ok(mut v) = SCRATCHES.lock() {
        for p in v.drain(..) {
            let _ = std::fs::remove_file(&p);
        }
    }
}

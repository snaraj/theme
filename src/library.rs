//! The wallpaper library: recursive walks over every configured directory,
//! name resolution with cardinality checks (exactly one candidate or a
//! refusal — never a guess between two), and the library-only resolver
//! destructive verbs use.

use crate::config::Config;
use crate::ui::die;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Cap long slugs at a WORD boundary — never a mid-word chop.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.chars().count() > 72 {
        let mut t: String = out.chars().take(72).collect();
        if let Some(pos) = t.rfind('-') {
            t.truncate(pos);
        }
        return t;
    }
    out
}

/// Every regular file under `dir`, recursively, symlinks not followed into
/// (matching `find dir -type f`: a symlinked FILE is not `-type f` under -P,
/// but find follows nothing by default and reports only real files).
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let p = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            walk(&p, out);
        } else if ft.is_file() {
            out.push(p);
        }
    }
}

/// All format-matching files across every library dir, in walk order.
pub fn all_images(cfg: &Config) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for d in &cfg.wallpaper_dirs {
        walk(d, &mut files);
    }
    files.retain(|p| cfg.format_matches(p));
    files
}

/// All regular files (any extension) across every library dir — resolution
/// by name is extension-blind, exactly like the shell's `find -name`.
fn all_files(cfg: &Config) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for d in &cfg.wallpaper_dirs {
        walk(d, &mut files);
    }
    files
}

fn file_name(p: &Path) -> &str {
    p.file_name().and_then(|n| n.to_str()).unwrap_or("")
}

/// A truncated title (trailing `…` or `...` welcome) resolves when it is
/// the prefix of exactly ONE library file; zero or several matches refuse.
fn prefix_match(cfg: &Config, name: &str) -> Option<PathBuf> {
    let p = name.strip_suffix('…').unwrap_or(name);
    let p = p.strip_suffix("...").unwrap_or(p);
    if p.is_empty() {
        return None;
    }
    let hits: Vec<PathBuf> = all_files(cfg)
        .into_iter()
        .filter(|f| file_name(f).starts_with(p))
        .collect();
    if hits.len() == 1 {
        hits.into_iter().next()
    } else {
        None
    }
}

/// One cardinality-checked resolution for every non-exact name: a bare stem
/// (`foo` for foo.jpg) resolves only while it names exactly ONE file — with
/// foo.jpg AND foo.png present it refuses instead of picking one — then a
/// truncated title falls through to the same unique-prefix rule.
fn library_match(cfg: &Config, stem: &str) -> Option<PathBuf> {
    let hits: Vec<PathBuf> = all_files(cfg)
        .into_iter()
        .filter(|f| {
            let n = file_name(f);
            n.strip_prefix(stem)
                .map(|rest| rest.starts_with('.') && !rest[1..].contains('.') && rest.len() > 1)
                .unwrap_or(false)
        })
        .collect();
    match hits.len() {
        1 => hits.into_iter().next(),
        0 => prefix_match(cfg, stem),
        _ => None,
    }
}

/// Resolve a `set`/`preview` argument: an existing path, a name under a
/// library dir, or a unique stem/prefix.
pub fn resolve_local(cfg: &Config, arg: &str) -> Option<PathBuf> {
    let p = Path::new(arg);
    if p.is_file() {
        return Some(p.to_path_buf());
    }
    for d in &cfg.wallpaper_dirs {
        let cand = d.join(arg);
        if cand.is_file() {
            return Some(cand);
        }
    }
    library_match(cfg, arg)
}

/// A random format-matching image, or None on an empty library.
pub fn random_local(cfg: &Config) -> Option<PathBuf> {
    let mut files = all_images(cfg);
    if files.is_empty() {
        return None;
    }
    // A shuffle needs unpredictability, not cryptography: nanoseconds and
    // the pid, splitmix64-scrambled.
    let mut x = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ (d.as_secs() << 20))
        .unwrap_or(0)
        ^ (std::process::id() as u64).rotate_left(32);
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    let idx = (x as usize) % files.len();
    Some(files.swap_remove(idx))
}

/// Library-only resolver for DESTRUCTIVE verbs (rm / rename): bare NAMES
/// only — any path separator or leading dot is refused, so `..`, absolute
/// and nested paths never reach a destructive verb — and the match is
/// re-checked by canonical physical path (symlink-proof) against the
/// specific library dir that holds it.
pub fn resolve_library(cfg: &Config, name: &str) -> Option<PathBuf> {
    if name.contains('/') || name.starts_with('.') || name.is_empty() {
        return None;
    }
    let mut cand: Option<PathBuf> = None;
    for d in &cfg.wallpaper_dirs {
        let c = d.join(name);
        if c.is_file() {
            cand = Some(c);
            break;
        }
    }
    let cand = match cand {
        Some(c) => c,
        None => library_match(cfg, name)?,
    };
    // Canonical containment: the file's physical directory must be one of
    // the library roots or a descendant. A symlink pointing outside still
    // canonicalizes outside and is refused.
    let parent = fs::canonicalize(cand.parent()?).ok()?;
    let contained = cfg.wallpaper_dirs.iter().any(|d| {
        fs::canonicalize(d)
            .map(|root| parent == root || parent.starts_with(&root))
            .unwrap_or(false)
    });
    if contained { Some(cand) } else { None }
}

/// The first library dir that exists takes downloads. No creation and no
/// writability probe here: an unwritable pick fails closed in the saver.
pub fn download_dir(cfg: &Config) -> &Path {
    for d in &cfg.wallpaper_dirs {
        if d.is_dir() {
            return d;
        }
    }
    die(&format!(
        "no wallpaper library directory exists (looked at {})",
        cfg.wallpaper_dirs_display
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_matches_the_shell() {
        assert_eq!(slugify("Hello,  World!"), "hello-world");
        assert_eq!(slugify("--x--"), "x");
        assert_eq!(slugify("a.b.jpg"), "a-b-jpg");
        assert_eq!(slugify("ÜBER cool"), "ber-cool");
        // 72-char cap lands on a word boundary, never mid-word.
        let long = "word ".repeat(20);
        let s = slugify(&long);
        assert!(s.chars().count() <= 72);
        assert!(!s.ends_with('-'));
        assert!(s.ends_with("word"));
    }
}

//! Palette cache: keyed by image identity (path, mtime, size) and derivation
//! parameters, stored in the plain-text format. FNV-1a keys are fine here —
//! the cache directory is user-owned and the keys are not adversarial.

use crate::{Error, Options, Palette, derive};
use std::fs;
use std::path::Path;

/// Bumped whenever derivation or the cache format changes meaning.
const VERSION: u32 = 1;

fn fnv1a(s: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The cache filename stem for `path` + `opts` — stable across runs, unique
/// per (image identity, parameters, engine version).
pub fn cache_key(path: &Path, opts: &Options) -> Result<String, Error> {
    let canon =
        fs::canonicalize(path).map_err(|e| Error::Cache(format!("{}: {e}", path.display())))?;
    let meta =
        fs::metadata(&canon).map_err(|e| Error::Cache(format!("{}: {e}", canon.display())))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let id = format!(
        "{}|{}|{}|{:?}|{}|{}|{}",
        canon.display(),
        mtime,
        meta.len(),
        opts.mode,
        opts.clusters,
        opts.seed,
        VERSION,
    );
    Ok(format!("{:016x}", fnv1a(&id)))
}

/// Derive with a read-through cache in `cache_dir` (created if missing).
/// A hit is a file read and parse — no image decode.
pub fn cached_derive(path: &Path, opts: &Options, cache_dir: &Path) -> Result<Palette, Error> {
    let file = cache_dir.join(format!("{}.palette", cache_key(path, opts)?));
    // An unparseable cache entry is stale format, not an error: re-derive.
    if let Ok(text) = fs::read_to_string(&file)
        && let Some(p) = Palette::from_cache_format(&text)
    {
        return Ok(p);
    }
    let palette = derive(path, opts)?;
    fs::create_dir_all(cache_dir)
        .map_err(|e| Error::Cache(format!("{}: {e}", cache_dir.display())))?;
    let tmp = file.with_extension("tmp");
    fs::write(&tmp, palette.to_cache_format())
        .and_then(|()| fs::rename(&tmp, &file))
        .map_err(|e| Error::Cache(format!("{}: {e}", file.display())))?;
    Ok(palette)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Rgb;

    fn test_dirs(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let base =
            std::env::temp_dir().join(format!("pigment-cache-{}-{name}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        (base.join("img.png"), base)
    }

    fn write_img(path: &Path, color: [u8; 3]) {
        let buf: Vec<u8> = std::iter::repeat_n(color, 64 * 64).flatten().collect();
        image::save_buffer(path, &buf, 64, 64, image::ColorType::Rgb8).unwrap();
    }

    #[test]
    fn roundtrip_hit_equals_miss() {
        let (img, dir) = test_dirs("roundtrip");
        write_img(&img, [40, 90, 160]);
        let opts = Options::default();
        let miss = cached_derive(&img, &opts, &dir).unwrap();
        let hit = cached_derive(&img, &opts, &dir).unwrap();
        assert_eq!(miss, hit);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn content_change_invalidates() {
        let (img, dir) = test_dirs("invalidate");
        write_img(&img, [200, 30, 30]);
        let opts = Options::default();
        let k1 = cache_key(&img, &opts).unwrap();
        let first = cached_derive(&img, &opts, &dir).unwrap();
        // Rewrite with different bytes; size stays equal, mtime may tick.
        // Force a distinct mtime so the key must change.
        write_img(&img, [30, 30, 200]);
        let newer = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
        let f = fs::File::open(&img).unwrap();
        f.set_modified(newer).unwrap();
        let k2 = cache_key(&img, &opts).unwrap();
        assert_ne!(k1, k2, "mtime change must change the key");
        let second = cached_derive(&img, &opts, &dir).unwrap();
        assert_ne!(first.wallpaper_average, second.wallpaper_average);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_cache_entry_rederives() {
        let (img, dir) = test_dirs("corrupt");
        write_img(&img, [90, 90, 90]);
        let opts = Options::default();
        let file = dir.join(format!("{}.palette", cache_key(&img, &opts).unwrap()));
        fs::write(&file, "pigment1\ngarbage\n").unwrap();
        let p = cached_derive(&img, &opts, &dir).unwrap();
        assert_eq!(
            p.wallpaper_average,
            Rgb {
                r: 90,
                g: 90,
                b: 90
            }
        );
        // The corrupt entry was replaced with a parseable one.
        assert!(Palette::from_cache_format(&fs::read_to_string(&file).unwrap()).is_some());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_image_is_an_error() {
        let (img, dir) = test_dirs("missing");
        let err = cached_derive(&img, &Options::default(), &dir);
        assert!(err.is_err());
        fs::remove_dir_all(&dir).ok();
    }
}

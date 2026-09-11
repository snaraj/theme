//! Palette cache: keyed by image identity (path, mtime, size) and derivation
//! parameters, stored in the plain-text format. FNV-1a keys are fine here —
//! the cache directory is user-owned and the keys are not adversarial.

use crate::emit::CacheRecord;
use crate::{Error, Options, Palette, Rgb, derive};
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Bumped whenever derivation or the cache format changes meaning.
const VERSION: u32 = 2;
const MAX_CACHE_BYTES: u64 = 8192;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

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
    Ok(cache_snapshot(path, opts)?.0)
}

fn cache_snapshot(path: &Path, opts: &Options) -> Result<(String, fs::Metadata), Error> {
    let canon =
        fs::canonicalize(path).map_err(|e| Error::Cache(format!("{}: {e}", path.display())))?;
    let meta =
        fs::metadata(&canon).map_err(|e| Error::Cache(format!("{}: {e}", canon.display())))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut id = format!(
        "{}|{}|{}|{:?}|{}|{}|{}",
        canon.display(),
        mtime,
        meta.len(),
        opts.mode,
        opts.clusters,
        opts.seed,
        VERSION,
    );
    // In-place edits preserving mtime and atomic replacement at the same path
    // must not reuse an old profile. No full-image hash on the warm path.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        id.push_str(&format!(
            "|{}|{}|{}|{}",
            meta.dev(),
            meta.ino(),
            meta.ctime(),
            meta.ctime_nsec()
        ));
    }
    Ok((format!("{:016x}", fnv1a(&id)), meta))
}

fn unchanged(path: &Path, opts: &Options, key: &str, before: &fs::Metadata) -> Result<bool, Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = (opts, key);
        let after =
            fs::metadata(path).map_err(|e| Error::Cache(format!("{}: {e}", path.display())))?;
        let stamp = |m: &fs::Metadata| {
            (
                m.dev(),
                m.ino(),
                m.len(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec(),
            )
        };
        // Follow the current source path: a changed symlink target or an
        // atomic replacement must not validate against the old canonical path.
        // A different hard link to the same unchanged inode is the same image.
        Ok(stamp(before) == stamp(&after))
    }
    #[cfg(not(unix))]
    {
        let _ = before;
        Ok(cache_key(path, opts)? == key)
    }
}

fn read_entry<T>(path: &Path, parse: impl FnOnce(&str) -> Option<T>) -> Option<T> {
    let descriptor = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::NOFOLLOW,
        rustix::fs::Mode::empty(),
    )
    .ok()?;
    let file = fs::File::from(descriptor);
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_CACHE_BYTES {
        return None;
    }
    let mut text = String::with_capacity(metadata.len() as usize + 1);
    file.take(MAX_CACHE_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() as u64 > MAX_CACHE_BYTES {
        return None;
    }
    parse(&text)
}

fn read_cached_entry<T>(
    path: &Path,
    opts: &Options,
    cache_dir: &Path,
    parse: impl FnOnce(&str) -> Option<T>,
) -> Result<Option<T>, Error> {
    let (key, before) = cache_snapshot(path, opts)?;
    let Some(palette) = read_entry(&cache_dir.join(format!("{key}.palette")), parse) else {
        // Nothing was accepted on a miss, so there is no cached identity to
        // revalidate. Derivation performs its own before/after identity check.
        return Ok(None);
    };
    if !unchanged(path, opts, &key, &before)? {
        return Ok(None);
    }
    Ok(Some(palette))
}

/// Read a valid cached palette without creating files or decoding the image.
pub fn read_cached(
    path: &Path,
    opts: &Options,
    cache_dir: &Path,
) -> Result<Option<Palette>, Error> {
    Ok(
        read_cached_entry(path, opts, cache_dir, CacheRecord::parse)?
            .and_then(CacheRecord::into_palette),
    )
}

/// Read the 16 cached colors without computing image-profile features. The
/// complete record and the image identity are validated exactly as in
/// [`read_cached`]; this never creates files or decodes an image.
pub fn read_cached_colors(
    path: &Path,
    opts: &Options,
    cache_dir: &Path,
) -> Result<Option<[Rgb; 16]>, Error> {
    read_cached_entry(path, opts, cache_dir, CacheRecord::parse_colors)
}

/// Derive with a read-through cache in `cache_dir` (created if missing).
/// A hit is a file read and parse — no image decode.
pub fn cached_derive(path: &Path, opts: &Options, cache_dir: &Path) -> Result<Palette, Error> {
    let (key, before) = cache_snapshot(path, opts)?;
    let file = cache_dir.join(format!("{key}.palette"));
    // An unparseable cache entry is stale format, not an error: re-derive.
    if let Some(p) = read_entry(&file, CacheRecord::parse).and_then(CacheRecord::into_palette)
        && unchanged(path, opts, &key, &before)?
    {
        return Ok(p);
    }
    let palette = derive(path, opts)?;
    if !unchanged(path, opts, &key, &before)? {
        return Err(Error::Cache(
            "image changed during derivation; retry".into(),
        ));
    }
    fs::create_dir_all(cache_dir)
        .map_err(|e| Error::Cache(format!("{}: {e}", cache_dir.display())))?;
    let tmp = file.with_extension(format!(
        "{}-{}.tmp",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|error| Error::Cache(format!("{}: {error}", file.display())))?;
    let result = output
        .write_all(palette.to_cache_format().as_bytes())
        .and_then(|()| fs::rename(&tmp, &file));
    if let Err(error) = result {
        let _ = fs::remove_file(&tmp);
        return Err(Error::Cache(format!("{}: {error}", file.display())));
    }
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

    #[cfg(unix)]
    #[test]
    fn snapshot_rechecks_current_target_replacements_and_restored_mtime() {
        use std::os::unix::fs::symlink;
        let (img, dir) = test_dirs("snapshot");
        fs::write(&img, b"first").unwrap();
        let other = dir.join("other");
        fs::write(&other, b"other").unwrap();
        let hard = dir.join("hard");
        fs::hard_link(&img, &hard).unwrap();
        let link = dir.join("link");
        symlink(&img, &link).unwrap();
        let opts = Options::default();
        let (key, before) = cache_snapshot(&link, &opts).unwrap();
        assert!(unchanged(&link, &opts, &key, &before).unwrap());
        fs::remove_file(&link).unwrap();
        symlink(&other, &link).unwrap();
        assert!(!unchanged(&link, &opts, &key, &before).unwrap());
        fs::remove_file(&link).unwrap();
        symlink(&hard, &link).unwrap();
        assert!(unchanged(&link, &opts, &key, &before).unwrap());
        fs::rename(&other, &img).unwrap();
        assert!(!unchanged(&img, &opts, &key, &before).unwrap());
        let (key, before) = cache_snapshot(&img, &opts).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        fs::write(&img, b"again").unwrap();
        fs::File::open(&img)
            .unwrap()
            .set_modified(before.modified().unwrap())
            .unwrap();
        assert_eq!(fs::metadata(&img).unwrap().len(), before.len());
        assert_eq!(
            fs::metadata(&img).unwrap().modified().unwrap(),
            before.modified().unwrap()
        );
        assert!(!unchanged(&img, &opts, &key, &before).unwrap());
        fs::remove_file(&img).unwrap();
        assert!(unchanged(&img, &opts, &key, &before).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cache_read_rejects_replacement_after_record_parsing() {
        let (img, dir) = test_dirs("read-replacement");
        let opts = Options::default();
        write_img(&img, [30, 60, 90]);
        cached_derive(&img, &opts, &dir).unwrap();
        let replacement = dir.join("replacement.png");
        write_img(&replacement, [90, 60, 30]);
        let result = read_cached_entry(&img, &opts, &dir, |text| {
            let colors = CacheRecord::parse_colors(text);
            assert!(colors.is_some());
            fs::rename(&replacement, &img).unwrap();
            colors
        })
        .unwrap();
        assert!(result.is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn fractional_mtime_rewrites_invalidate_the_cached_profile() {
        use std::time::{Duration, UNIX_EPOCH};
        let (img, dir) = test_dirs("fine-mtime");
        let opts = Options::default();
        write_img(&img, [30, 60, 90]);
        fs::File::open(&img)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::new(1_700_000_000, 100_000_000))
            .unwrap();
        let first_key = cache_key(&img, &opts).unwrap();
        let first = cached_derive(&img, &opts, &dir).unwrap();
        write_img(&img, [90, 60, 30]);
        fs::File::open(&img)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::new(1_700_000_000, 200_000_000))
            .unwrap();
        assert_ne!(cache_key(&img, &opts).unwrap(), first_key);
        assert!(read_cached(&img, &opts, &dir).unwrap().is_none());
        assert!(read_cached_colors(&img, &opts, &dir).unwrap().is_none());
        let second = cached_derive(&img, &opts, &dir).unwrap();
        assert_ne!(first.profile.signature, second.profile.signature);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn readonly_cache_never_creates_and_rejects_oversized_entries() {
        let (img, dir) = test_dirs("readonly");
        write_img(&img, [30, 60, 90]);
        let opts = Options::default();
        let cache = dir.join("absent");
        assert!(read_cached(&img, &opts, &cache).unwrap().is_none());
        assert!(read_cached_colors(&img, &opts, &cache).unwrap().is_none());
        assert!(!cache.exists());
        let file = dir.join(format!("{}.palette", cache_key(&img, &opts).unwrap()));
        let mut content = derive(&img, &opts).unwrap().to_cache_format();
        content.push_str(&" ".repeat(MAX_CACHE_BYTES as usize));
        fs::write(file, content).unwrap();
        assert!(read_cached(&img, &opts, &dir).unwrap().is_none());
        assert!(read_cached_colors(&img, &opts, &dir).unwrap().is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn colors_only_matches_full_palette_without_decode_or_writes() {
        let (img, dir) = test_dirs("colors-only");
        write_img(&img, [30, 60, 90]);
        let opts = Options::default();
        let palette = derive(&img, &opts).unwrap();
        // Reading an existing cache requires only image identity, not a decoder.
        let asset = dir.join("identity-only.dat");
        fs::write(&asset, "not an image").unwrap();
        let file = dir.join(format!("{}.palette", cache_key(&asset, &opts).unwrap()));
        let content = palette.to_cache_format();
        fs::write(&file, &content).unwrap();
        let modified = fs::metadata(&file).unwrap().modified().unwrap();
        assert_eq!(
            read_cached_colors(&asset, &opts, &dir).unwrap(),
            Some(palette.colors)
        );
        assert_eq!(read_cached(&asset, &opts, &dir).unwrap(), Some(palette));
        assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), modified);
        assert_eq!(fs::read_to_string(file).unwrap(), content);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn colors_only_rejects_invalid_fields_beyond_the_sixteen_colors() {
        let (img, dir) = test_dirs("colors-validation");
        write_img(&img, [30, 60, 90]);
        let opts = Options::default();
        let valid = derive(&img, &opts).unwrap().to_cache_format();
        let lines: Vec<_> = valid.lines().collect();
        let file = dir.join(format!("{}.palette", cache_key(&img, &opts).unwrap()));
        let mut invalid = vec![
            valid.replace("pigment2", "pigment1"),
            valid.clone() + "extra\n",
        ];
        for (line, replacement) in [
            (1, "invalid color"),
            (17, "invalid foreground"),
            (18, "invalid cursor"),
            (19, "invalid average"),
            (20, "invalid mode"),
            (21, "profile 0 64 16 16"),
            (21, "profile 64 64 17 16"),
            (21, "profile 1 64 16 16"),
            (21, "profile 64 64 16 16 extra"),
            (lines.len() - 1, "invalid final sample"),
        ] {
            let mut altered = lines.clone();
            altered[line] = replacement;
            invalid.push(altered.join("\n"));
        }
        invalid.push(lines[..lines.len() - 1].join("\n"));
        for record in invalid {
            fs::write(&file, &record).unwrap();
            assert!(read_cached_colors(&img, &opts, &dir).unwrap().is_none());
            assert!(read_cached(&img, &opts, &dir).unwrap().is_none());
            assert_eq!(fs::read_to_string(&file).unwrap(), record);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cache_reads_reject_owned_nonregular_entries() {
        let (img, dir) = test_dirs("nonregular");
        write_img(&img, [30, 60, 90]);
        let opts = Options::default();
        let file = dir.join(format!("{}.palette", cache_key(&img, &opts).unwrap()));
        let target = dir.join("regular.palette");
        fs::write(&target, derive(&img, &opts).unwrap().to_cache_format()).unwrap();
        std::os::unix::fs::symlink(&target, &file).unwrap();
        assert!(read_cached_colors(&img, &opts, &dir).unwrap().is_none());
        assert!(read_cached(&img, &opts, &dir).unwrap().is_none());
        fs::remove_file(&file).unwrap();
        fs::create_dir(&file).unwrap();
        assert!(read_cached_colors(&img, &opts, &dir).unwrap().is_none());
        fs::remove_dir(&file).unwrap();
        // Only exercise the corrected nonblocking reader; no writer is needed.
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&file)
                .status()
                .unwrap()
                .success()
        );
        assert!(read_cached_colors(&img, &opts, &dir).unwrap().is_none());
        assert!(read_cached(&img, &opts, &dir).unwrap().is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn concurrent_derivations_use_independent_temporary_files() {
        let (img, dir) = test_dirs("parallel");
        write_img(&img, [30, 60, 90]);
        std::thread::scope(|scope| {
            let jobs: Vec<_> = (0..4)
                .map(|_| scope.spawn(|| cached_derive(&img, &Options::default(), &dir).unwrap()))
                .collect();
            let palettes: Vec<_> = jobs.into_iter().map(|job| job.join().unwrap()).collect();
            assert!(palettes.windows(2).all(|pair| pair[0] == pair[1]));
        });
        assert!(
            read_cached(&img, &Options::default(), &dir)
                .unwrap()
                .is_some()
        );
        fs::remove_dir_all(dir).unwrap();
    }
}

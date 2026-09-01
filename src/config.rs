//! Environment-derived configuration, resolved once. The wallpaper library
//! is a COLON-SEPARATED list of directories (owner fix #5): every listing,
//! resolution and random pick searches all of them recursively in order, and
//! downloads land in the first existing one. A single-directory value keeps
//! its old meaning unchanged.

use crate::ui::die;
use std::env;
use std::path::PathBuf;

pub const MIN_WIDTH: u32 = 2560;
/// The largest download the tool will accept, enforced at curl
/// (`--max-filesize`) and re-checked from the saver against the file on disk.
/// 100 MiB: a generous ceiling for a compressed wallpaper (even 8K JPEG/PNG
/// sits far below it), while bounding scratch-disk and the whole-file read the
/// saver performs — a fast endpoint cannot fill the filesystem or the heap.
pub const MAX_DOWNLOAD_BYTES: u64 = 100 * 1024 * 1024;
pub const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36";
const FORMATS_ALL: &str = "jpg jpeg png webp gif bmp tif tiff";

pub struct Config {
    /// The library directories, in configured order.
    pub wallpaper_dirs: Vec<PathBuf>,
    /// The raw configured value, for display in messages and status.
    pub wallpaper_dirs_display: String,
    /// Palette cache directory (colors, colors-kitty.conf, schemes/).
    pub cache_dir: PathBuf,
    pub kitty_dir: PathBuf,
    /// kitty's `current-theme.conf` include shim.
    pub current: PathBuf,
    /// Lowercased include-set minus exclude-set, no leading dots.
    pub formats: Vec<String>,
    pub contrast: f64,
    pub no_apply: bool,
}

fn home() -> PathBuf {
    PathBuf::from(env::var("HOME").unwrap_or_else(|_| "/".into()))
}

impl Config {
    pub fn from_env() -> Config {
        let config_dir = env::var("CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home().join(".config"));
        let raw_dirs = env::var("THEME_WALLPAPER_DIR")
            .or_else(|_| env::var("WALLPAPER_DIR"))
            .unwrap_or_else(|_| config_dir.join("wallpapers").to_string_lossy().into_owned());
        let wallpaper_dirs: Vec<PathBuf> = raw_dirs
            .split(':')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();
        let wallpaper_dirs = if wallpaper_dirs.is_empty() {
            vec![config_dir.join("wallpapers")]
        } else {
            wallpaper_dirs
        };
        let kitty_dir = env::var("KITTY_CONFIG_DIRECTORY")
            .map(PathBuf::from)
            .unwrap_or_else(|_| config_dir.join("kitty"));
        let cache_dir = env::var("THEME_CACHE_DIR")
            .or_else(|_| env::var("WAL_CACHE"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| home().join(".cache/theme"));
        let contrast_raw = env::var("THEME_CONTRAST").unwrap_or_else(|_| "4.5".into());
        // Same validation as the shell dispatch: digits and at most one dot.
        let dotcount = contrast_raw.matches('.').count();
        if contrast_raw.is_empty()
            || contrast_raw == "."
            || dotcount > 1
            || contrast_raw
                .bytes()
                .any(|b| !b.is_ascii_digit() && b != b'.')
        {
            die(&format!(
                "THEME_CONTRAST must be a number (got: {contrast_raw})"
            ));
        }
        let contrast: f64 = contrast_raw.parse().unwrap_or(4.5);

        let inc = env::var("THEME_FORMATS").unwrap_or_else(|_| FORMATS_ALL.into());
        let exc = env::var("THEME_EXCLUDE_FORMATS").unwrap_or_default();
        let norm = |v: &str| -> Vec<String> {
            v.replace(',', " ")
                .to_lowercase()
                .split_whitespace()
                .map(|e| e.trim_start_matches('.').to_string())
                .filter(|e| !e.is_empty())
                .collect()
        };
        let excs = norm(&exc);
        let formats: Vec<String> = norm(&inc)
            .into_iter()
            .filter(|e| !excs.contains(e))
            .collect();
        if formats.is_empty() {
            die("THEME_FORMATS/THEME_EXCLUDE_FORMATS leave no formats to list");
        }

        Config {
            current: kitty_dir.join("current-theme.conf"),
            wallpaper_dirs,
            wallpaper_dirs_display: raw_dirs,
            cache_dir,
            kitty_dir,
            formats,
            contrast,
            no_apply: env::var("THEME_NO_APPLY")
                .map(|v| !v.is_empty())
                .unwrap_or(false),
        }
    }

    /// Does this extension (case-insensitive) belong to the format set?
    pub fn format_matches(&self, path: &std::path::Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| self.formats.contains(&e.to_lowercase()))
            .unwrap_or(false)
    }

    /// The formats line `theme status` prints.
    pub fn formats_display(&self) -> String {
        let inc = env::var("THEME_FORMATS").unwrap_or_else(|_| FORMATS_ALL.into());
        match env::var("THEME_EXCLUDE_FORMATS") {
            Ok(e) if !e.is_empty() => format!("{inc} (minus: {e})"),
            _ => format!("{inc} "),
        }
    }
}

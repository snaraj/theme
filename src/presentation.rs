//! One final-palette preparation path for preview and apply. Preview performs
//! bounded reads and in-memory derivation only; swatches use SGR, never OSC.

use crate::apply::{derive_options, schemes_dir};
use crate::config::Config;
use pigment::{Floored, ImageProfile, Palette, Readability};
use std::borrow::Cow;
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub struct PreparedPalette {
    pub palette: Floored,
    pub opacity: f64,
    pub contrast: f64,
    pub readability: Readability,
    pub profile: ImageProfile,
    pub opacity_limited: bool,
}

/// A configured/explicit opacity, not a query of running Kitty windows.
pub fn opacity(cfg: &Config) -> Result<f64, String> {
    let override_value = std::env::var("THEME_OPACITY").ok();
    configured_opacity(cfg, override_value.as_deref())
}

fn parse_opacity(value: &str) -> Result<f64, String> {
    value
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
        .ok_or_else(|| "opacity must be a finite number from 0 to 1".to_string())
}

fn opacity_error(reason: &str) -> String {
    format!(
        "cannot resolve Kitty opacity ({reason}); set THEME_OPACITY to the intended value from 0 to 1"
    )
}

fn configured_opacity(cfg: &Config, explicit: Option<&str>) -> Result<f64, String> {
    if let Some(value) = explicit {
        return parse_opacity(value);
    }
    let (mut value, mut files, mut bytes) = (1.0, 0, 0);
    read_opacity(
        &cfg.kitty_dir.join("kitty.conf"),
        &mut value,
        &mut Vec::new(),
        &mut files,
        &mut bytes,
    )?;
    Ok(value)
}

struct OpacityFrame {
    path: PathBuf,
    identity: (u64, u64),
    canonical: Option<PathBuf>,
}

fn config_path(path: &Path) -> Result<PathBuf, String> {
    path.canonicalize()
        .map_err(|_| opacity_error("config path"))
}

fn config_identity(path: &Path, opened: &fs::Metadata) -> Result<(u64, u64), String> {
    let identity = (opened.dev(), opened.ino());
    let current = fs::metadata(path).map_err(|_| opacity_error("config path"))?;
    if (current.dev(), current.ino()) != identity {
        return Err(opacity_error("config path changed"));
    }
    Ok(identity)
}

/// Local literal includes only. Dynamic configuration is never executed and
/// unsupported expansion is reported instead of pretending it was resolved.
fn read_opacity(
    path: &Path,
    value: &mut f64,
    stack: &mut Vec<OpacityFrame>,
    files: &mut usize,
    bytes: &mut usize,
) -> Result<(), String> {
    if stack.len() >= 8 || *files >= 32 {
        return Err(opacity_error("include limit"));
    }
    // Non-blocking open lets us reject non-regular files without waiting on a
    // pipe. The descriptor is checked before any contents are read.
    let descriptor = match rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(_) => return Err(opacity_error("unreadable config")),
    };
    let file = fs::File::from(descriptor);
    let metadata = file
        .metadata()
        .map_err(|_| opacity_error("config metadata"))?;
    if !metadata.is_file() {
        return Err(opacity_error("config is not a regular file"));
    }
    let identity = config_identity(path, &metadata)?;
    // Distinct open files cannot form a cycle. Repeated identities still need
    // canonical paths: hardlinks may have different relative-include contexts.
    let canonical = if stack.iter().any(|frame| frame.identity == identity) {
        let canonical = config_path(path)?;
        for frame in stack.iter_mut().filter(|frame| frame.identity == identity) {
            if frame.canonical.is_none() {
                frame.canonical = Some(config_path(&frame.path)?);
            }
            if frame.canonical.as_ref() == Some(&canonical) {
                return Err(opacity_error("include cycle"));
            }
        }
        Some(canonical)
    } else {
        None
    };
    let mut text = String::with_capacity(metadata.len().min(65_537) as usize);
    file.take(65_537)
        .read_to_string(&mut text)
        .map_err(|_| opacity_error("config text"))?;
    *files += 1;
    *bytes += text.len();
    if text.len() > 65_536 || *bytes > 262_144 {
        return Err(opacity_error("config size limit"));
    }
    stack.push(OpacityFrame {
        path: path.to_path_buf(),
        identity,
        canonical,
    });
    let mut logical: Vec<Cow<'_, str>> = Vec::new();
    for physical in text.lines() {
        let line = physical.trim_start();
        if let Some(continued) = line.strip_prefix('\\') {
            let previous = logical
                .last_mut()
                .ok_or_else(|| opacity_error("orphan continuation"))?;
            previous.to_mut().push_str(continued.trim_start());
        } else {
            logical.push(Cow::Borrowed(line));
        }
    }
    for line in logical {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name, argument) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let argument = argument.trim();
        match name {
            "background_opacity" => {
                *value = parse_opacity(argument)
                    .map_err(|_| opacity_error("invalid background_opacity"))?
            }
            "include" => {
                if argument.is_empty()
                    || argument.contains(['$', '~', '*', '?', '[', ']', '{', '}'])
                {
                    return Err(opacity_error(
                        "include expansion requires an explicit opacity",
                    ));
                }
                let include = Path::new(argument);
                let include = if include.is_absolute() {
                    include.to_path_buf()
                } else {
                    path.parent().unwrap_or(Path::new(".")).join(include)
                };
                read_opacity(&include, value, stack, files, bytes)?;
            }
            "globinclude" | "envinclude" | "geninclude" => {
                return Err(opacity_error("dynamic include"));
            }
            _ => {}
        }
    }
    stack.pop();
    Ok(())
}

pub fn from_palette(cfg: &Config, palette: Palette) -> Result<PreparedPalette, String> {
    prepare(palette, opacity(cfg)?, cfg.contrast)
}

/// Pure preparation at explicit settings, also used by deterministic previews.
pub fn prepare(palette: Palette, opacity: f64, contrast: f64) -> Result<PreparedPalette, String> {
    if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
        return Err("opacity must be a finite number from 0 to 1".into());
    }
    if !contrast.is_finite() || !(1.0..=21.0).contains(&contrast) {
        return Err("contrast must be a finite ratio from 1 to 21".into());
    }
    let profile = palette.profile.clone();
    let (palette, achievable) = palette.floor_for_image(opacity, contrast);
    let readability = profile.readability(&palette, opacity, contrast);
    Ok(PreparedPalette {
        palette,
        opacity,
        contrast,
        readability,
        profile,
        opacity_limited: !achievable,
    })
}

/// Read-only preview: cached palette when valid, otherwise derive in memory.
pub fn preview(cfg: &Config, path: &Path) -> Result<PreparedPalette, String> {
    let opts = derive_options();
    let palette =
        match pigment::read_cached(path, &opts, &schemes_dir(cfg)).map_err(|e| e.to_string())? {
            Some(palette) => palette,
            None => {
                let before = pigment::cache_key(path, &opts).map_err(|e| e.to_string())?;
                let palette = pigment::derive(path, &opts).map_err(|e| e.to_string())?;
                if pigment::cache_key(path, &opts).map_err(|e| e.to_string())? != before {
                    return Err("image changed during preview; retry".into());
                }
                palette
            }
        };
    from_palette(cfg, palette)
}

/// Reuse measured profiles across browser sessions. A dry run never writes;
/// inability to save a cache does not prevent a read-only preview.
pub fn cached_preview(cfg: &Config, path: &Path) -> Result<PreparedPalette, String> {
    if !cfg.no_apply
        && let Ok(palette) = pigment::cached_derive(path, &derive_options(), &schemes_dir(cfg))
    {
        return from_palette(cfg, palette);
    }
    preview(cfg, path)
}

impl PreparedPalette {
    /// Apply-only advice. Preview stays limited to the picture, name and colors.
    pub fn readability_warning(&self) -> Option<String> {
        (self.opacity_limited && self.opacity < 1.0).then(|| format!(
            "text may be hard to read over this wallpaper at {:.0}% opacity; increase your terminal's background opacity and apply the theme again",
            self.opacity * 100.0,
        ))
    }

    /// All 16 final colors, wrapped to fit without changing terminal colors.
    /// Width is bounded; every line ends in SGR reset, with no OSC or cursor moves.
    pub fn swatches(&self, width: usize) -> String {
        if width == 0 {
            return String::new();
        }
        let mut lines = Vec::new();
        let per_row = (width / 4).clamp(1, 16);
        for chunk in self.palette.colors.chunks(per_row) {
            let mut line = String::new();
            for color in chunk {
                line.push_str(&format!(
                    "\x1b[48;2;{};{};{}m{}\x1b[0m",
                    color.r,
                    color.g,
                    color.b,
                    " ".repeat(width.min(4)),
                ));
            }
            lines.push(line);
        }
        lines.join("\n") + "\n"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(name: &str) -> Config {
        let root =
            std::env::temp_dir().join(format!("theme-presentation-{}-{name}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        Config {
            wallpaper_dirs: vec![root.clone()],
            wallpaper_dirs_display: String::new(),
            cache_dir: root.join("cache"),
            kitty_dir: root.clone(),
            current: root.join("current-theme.conf"),
            formats: vec!["png".into()],
            contrast: 4.5,
            no_apply: true,
        }
    }

    #[test]
    fn opacity_follows_relative_includes_and_last_value() {
        let cfg = config("includes");
        fs::create_dir(cfg.kitty_dir.join("parts")).unwrap();
        fs::write(
            cfg.kitty_dir.join("kitty.conf"),
            "background_opacity 0.9\ninclude parts/a.conf\n",
        )
        .unwrap();
        fs::write(
            cfg.kitty_dir.join("parts/a.conf"),
            "include b.conf\n  background_opacity 0.\n\\ 65\n",
        )
        .unwrap();
        fs::write(
            cfg.kitty_dir.join("parts/b.conf"),
            "background_opacity 0.7\n",
        )
        .unwrap();
        assert_eq!(configured_opacity(&cfg, None).unwrap(), 0.65);
        assert_eq!(configured_opacity(&cfg, Some("0.8")).unwrap(), 0.8);
        fs::remove_dir_all(&cfg.kitty_dir).unwrap();
    }

    #[test]
    fn opacity_keeps_hardlink_contexts_and_detects_symlink_cycles() {
        let cfg = config("link-contexts");
        fs::create_dir_all(cfg.kitty_dir.join("one/next")).unwrap();
        let outer = cfg.kitty_dir.join("one/shared.conf");
        fs::write(&outer, "include next/shared.conf\ninclude value.conf\n").unwrap();
        fs::hard_link(&outer, cfg.kitty_dir.join("one/next/shared.conf")).unwrap();
        fs::write(
            cfg.kitty_dir.join("one/next/value.conf"),
            "background_opacity 0.8\n",
        )
        .unwrap();
        fs::write(
            cfg.kitty_dir.join("kitty.conf"),
            "include one/shared.conf\n",
        )
        .unwrap();
        // The same inode is active twice, but each relative include belongs to
        // its original parent. A missing include still leaves the value alone.
        assert_eq!(configured_opacity(&cfg, None).unwrap(), 0.8);
        std::os::unix::fs::symlink("kitty.conf", cfg.kitty_dir.join("alias.conf")).unwrap();
        fs::write(cfg.kitty_dir.join("kitty.conf"), "include alias.conf\n").unwrap();
        assert!(
            configured_opacity(&cfg, None)
                .unwrap_err()
                .contains("include cycle")
        );
        fs::remove_dir_all(&cfg.kitty_dir).unwrap();
    }

    #[test]
    fn opacity_rejects_a_replaced_path_after_open() {
        let cfg = config("path-binding");
        let path = cfg.kitty_dir.join("kitty.conf");
        fs::write(&path, "background_opacity 0.4\n").unwrap();
        let opened = fs::File::open(&path).unwrap();
        let metadata = opened.metadata().unwrap();
        assert!(config_identity(&path, &metadata).is_ok());
        let replacement = cfg.kitty_dir.join("replacement.conf");
        fs::write(&replacement, "background_opacity 0.8\n").unwrap();
        fs::rename(&replacement, &path).unwrap();
        assert!(
            config_identity(&path, &metadata)
                .unwrap_err()
                .contains("config path changed")
        );
        fs::remove_dir_all(&cfg.kitty_dir).unwrap();
    }

    #[test]
    fn opacity_preserves_continuations_and_absolute_include_order() {
        let cfg = config("logical-lines");
        let included = cfg.kitty_dir.join("included.conf");
        fs::write(&included, "background_opacity 0.4\n").unwrap();
        for (text, expected) in [
            ("\tbackground_opacity\t0.\n\\ 65\n".to_string(), 0.65),
            (
                "# ignored\n\\ continuation\nbackground_opacity 0.6\n".into(),
                0.6,
            ),
            ("\n\\ background_opacity 0.8\n".into(), 0.8),
            ("include ./included.\n\\ conf\n".into(), 0.4),
            (
                format!(
                    "background_opacity 0.2\ninclude {}\nbackground_opacity 0.7\n",
                    included.display()
                ),
                0.7,
            ),
        ] {
            fs::write(cfg.kitty_dir.join("kitty.conf"), text).unwrap();
            assert_eq!(configured_opacity(&cfg, None).unwrap(), expected);
        }
        fs::write(
            cfg.kitty_dir.join("kitty.conf"),
            "\\ background_opacity 0.8\n",
        )
        .unwrap();
        assert!(
            configured_opacity(&cfg, None)
                .unwrap_err()
                .contains("orphan continuation")
        );
        fs::remove_dir_all(&cfg.kitty_dir).unwrap();
    }

    #[test]
    fn opacity_keeps_depth_file_and_total_byte_limits() {
        let cfg = config("include-bounds");
        let main = cfg.kitty_dir.join("kitty.conf");
        fs::write(&main, "include depth-1.conf\n").unwrap();
        for depth in 1..7 {
            fs::write(
                cfg.kitty_dir.join(format!("depth-{depth}.conf")),
                format!("include depth-{}.conf\n", depth + 1),
            )
            .unwrap();
        }
        let leaf = cfg.kitty_dir.join("depth-7.conf");
        fs::write(&leaf, "background_opacity 0.6\n").unwrap();
        assert_eq!(configured_opacity(&cfg, None).unwrap(), 0.6);
        fs::write(&leaf, "include missing-depth-8.conf\n").unwrap();
        assert!(
            configured_opacity(&cfg, None)
                .unwrap_err()
                .contains("include limit")
        );
        fs::write(&main, "include depth-7.conf\n".repeat(31)).unwrap();
        fs::write(&leaf, "background_opacity 0.6\n").unwrap();
        assert_eq!(configured_opacity(&cfg, None).unwrap(), 0.6);
        fs::write(&main, "include depth-7.conf\n".repeat(32)).unwrap();
        assert!(
            configured_opacity(&cfg, None)
                .unwrap_err()
                .contains("include limit")
        );
        fs::write(cfg.kitty_dir.join("large.conf"), "#".repeat(65_500)).unwrap();
        fs::write(&main, "include large.conf\n".repeat(4)).unwrap();
        assert_eq!(configured_opacity(&cfg, None).unwrap(), 1.0);
        fs::write(&main, "include large.conf\n".repeat(5)).unwrap();
        assert!(
            configured_opacity(&cfg, None)
                .unwrap_err()
                .contains("config size limit")
        );
        fs::remove_dir_all(&cfg.kitty_dir).unwrap();
    }

    #[test]
    fn invalid_opacity_and_unresolved_configs_cannot_silently_pass() {
        let cfg = config("invalid");
        for invalid in ["NaN", "inf", "-0.1", "1.1", ""] {
            assert!(configured_opacity(&cfg, Some(invalid)).is_err());
        }
        for text in [
            "include kitty.conf\n",
            "geninclude generate.py\n",
            "globinclude *.conf\n",
            "background_opacity NaN\n",
        ] {
            fs::write(cfg.kitty_dir.join("kitty.conf"), text).unwrap();
            assert!(
                configured_opacity(&cfg, None)
                    .unwrap_err()
                    .contains("THEME_OPACITY")
            );
        }
        assert_eq!(configured_opacity(&cfg, Some("0.6")).unwrap(), 0.6);
        fs::write(cfg.kitty_dir.join("kitty.conf"), "#".repeat(65_537)).unwrap();
        assert!(configured_opacity(&cfg, None).is_err());
        fs::remove_dir_all(&cfg.kitty_dir).unwrap();
    }

    #[test]
    fn preview_and_apply_preparation_agree_without_cache_or_terminal_writes() {
        let cfg = config("preview");
        fs::write(
            cfg.kitty_dir.join("kitty.conf"),
            "background_opacity 0.65\n",
        )
        .unwrap();
        let path = cfg.kitty_dir.join("image.png");
        image::save_buffer(
            &path,
            &[25, 50, 120, 220, 230, 240],
            2,
            1,
            image::ColorType::Rgb8,
        )
        .unwrap();
        let shown = preview(&cfg, &path).unwrap();
        let raw = pigment::derive(&path, &derive_options()).unwrap();
        let applied = from_palette(&cfg, raw).unwrap();
        assert_eq!(shown.palette.to_kitty(), applied.palette.to_kitty());
        assert_eq!(shown.readability, applied.readability);
        assert_eq!((shown.profile.width, shown.profile.height), (2, 1));
        assert!(!cfg.cache_dir.exists());
        assert!(!cfg.current.exists());
        for width in [0, 1, 3, 20, 80] {
            let swatches = shown.swatches(width);
            assert_eq!(
                swatches.matches("\x1b[48;2;").count(),
                if width == 0 { 0 } else { 16 }
            );
            if width > 0 {
                for color in shown.palette.colors {
                    assert!(
                        swatches
                            .contains(&format!("\x1b[48;2;{};{};{}m", color.r, color.g, color.b))
                    );
                }
            }
            assert!(!swatches.contains("\x1b]"));
            assert!(!swatches.contains("\x1bP"));
            for line in swatches.lines() {
                let mut visible = 0;
                let mut rest = line;
                while !rest.is_empty() {
                    if let Some(sgr) = rest.strip_prefix("\x1b[") {
                        let (_, tail) = sgr.split_once('m').expect("SGR terminator");
                        rest = tail;
                    } else {
                        visible += 1;
                        rest = &rest[1..];
                    }
                }
                assert!(visible <= width);
                assert!(line.ends_with("\x1b[0m"));
            }
        }
        fs::remove_dir_all(&cfg.kitty_dir).unwrap();
    }

    #[test]
    fn transparency_advice_is_plain_and_separate_from_preview() {
        let cfg = config("contrast-warning");
        let path = cfg.kitty_dir.join("mixed.png");
        image::save_buffer(
            &path,
            &[0, 0, 0, 255, 255, 255],
            2,
            1,
            image::ColorType::Rgb8,
        )
        .unwrap();
        let raw = pigment::derive(&path, &derive_options()).unwrap();
        let low = prepare(raw.clone(), 0.1, 7.0).unwrap();
        assert!(low.opacity_limited);
        let warning = low.readability_warning().unwrap();
        assert!(warning.contains("10% opacity"));
        assert!(warning.contains("increase your terminal's background opacity"));
        assert!(!warning.contains("sample") && !warning.contains('\x1b'));
        assert!(!low.swatches(80).contains("opacity"));
        assert!(
            prepare(raw, 1.0, 7.0)
                .unwrap()
                .readability_warning()
                .is_none()
        );
        assert!(!cfg.current.exists());
        fs::remove_dir_all(&cfg.kitty_dir).unwrap();
    }
}

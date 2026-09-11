//! One final-palette preparation path for preview and apply. Preview performs
//! bounded reads and in-memory derivation only; specimens use SGR, never OSC.

use crate::apply::{derive_options, schemes_dir};
use crate::config::Config;
use pigment::{Floored, ImageProfile, Palette, Readability, Rgb, effective_background};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

pub struct PreparedPalette {
    pub palette: Floored,
    pub opacity: f64,
    pub contrast: f64,
    pub readability: Readability,
    pub profile: ImageProfile,
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

/// Local literal includes only. Dynamic configuration is never executed and
/// unsupported expansion is reported instead of pretending it was resolved.
fn read_opacity(
    path: &Path,
    value: &mut f64,
    stack: &mut Vec<PathBuf>,
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
    if !file
        .metadata()
        .map_err(|_| opacity_error("config metadata"))?
        .is_file()
    {
        return Err(opacity_error("config is not a regular file"));
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| opacity_error("config path"))?;
    if stack.contains(&canonical) {
        return Err(opacity_error("include cycle"));
    }
    let mut text = String::new();
    file.take(65_537)
        .read_to_string(&mut text)
        .map_err(|_| opacity_error("config text"))?;
    *files += 1;
    *bytes += text.len();
    if text.len() > 65_536 || *bytes > 262_144 {
        return Err(opacity_error("config size limit"));
    }
    stack.push(canonical);
    let mut logical: Vec<String> = Vec::new();
    for physical in text.lines() {
        let line = physical.trim_start();
        if let Some(continued) = line.strip_prefix('\\') {
            let previous = logical
                .last_mut()
                .ok_or_else(|| opacity_error("orphan continuation"))?;
            previous.push_str(continued.trim_start());
        } else {
            logical.push(line.to_string());
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
    let background = effective_background(palette.background(), opacity, palette.wallpaper_average);
    let palette = palette.floor_against(background, contrast);
    let readability = profile.readability(&palette, opacity, contrast);
    Ok(PreparedPalette {
        palette,
        opacity,
        contrast,
        readability,
        profile,
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

fn paint(fg: Rgb, bg: Rgb, text: &str, width: usize) -> String {
    // Specimen strings are fixed ASCII. No path, metadata or shell input can
    // supply escape sequences, and each painted span resets its own SGR.
    let text: String = text.chars().take(width).collect();
    format!(
        "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m{text:width$}\x1b[0m",
        fg.r, fg.g, fg.b, bg.r, bg.g, bg.b
    )
}

impl PreparedPalette {
    /// A terminal specimen with all 16 colors and interface/text samples.
    /// Width is bounded; every line ends in SGR reset, with no OSC or cursor moves.
    pub fn specimen(&self, width: usize) -> String {
        let width = width.min(120);
        if width == 0 {
            return String::new();
        }
        let p = &self.palette;
        let ui = p.interface();
        let background = effective_background(p.background(), self.opacity, p.wallpaper_average);
        let mut lines = vec![
            paint(
                ui.active_tab_foreground,
                ui.active_tab_background,
                "  editor.go  ",
                width,
            ),
            paint(
                ui.inactive_tab_foreground,
                ui.inactive_tab_background,
                "  logs  docs  ",
                width,
            ),
            paint(
                p.foreground,
                background,
                "sam@local ~/project $ go test ./...",
                width,
            ),
            paint(p.colors[2], background, "PASS  all checks passed", width),
            paint(p.colors[1], background, "error: example diagnostic", width),
            paint(
                p.colors[3],
                background,
                "warning: example diagnostic",
                width,
            ),
            paint(
                ui.selection_foreground,
                ui.selection_background,
                "selected text  Aa 0123456789",
                width,
            ),
            paint(ui.cursor_text_color, p.cursor, "cursor", width),
        ];
        let per_row = (width / 4).clamp(1, 16);
        for chunk in p.colors.chunks(per_row).enumerate() {
            let mut line = String::new();
            for (index, &color) in chunk.1.iter().enumerate() {
                let fg = if Rgb::WHITE.contrast(color) >= Rgb::BLACK.contrast(color) {
                    Rgb::WHITE
                } else {
                    Rgb::BLACK
                };
                line.push_str(&paint(
                    fg,
                    color,
                    &format!(" {:02} ", chunk.0 * per_row + index),
                    width.min(4),
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
            let specimen = shown.specimen(width);
            assert!(!specimen.contains("\x1b]"));
            assert!(!specimen.contains("\x1bP"));
            for line in specimen.lines() {
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
}

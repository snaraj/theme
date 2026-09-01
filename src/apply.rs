//! Applying a wallpaper: the desktop, the palette (pigment, replacing
//! pywal), and every terminal behind one trait — adding a terminal is one
//! more impl, nothing else changes. The kitty socket remains the ONE
//! sanctioned path to a live kitty (never SIGUSR1: a config reload resets
//! runtime state, and a theme change may touch colors only).

use crate::config::Config;
use crate::imaging::img_size;
use crate::ui::{die, note};
use pigment::{Options, Palette, effective_background};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The desktop's own answer for the current wallpaper, when it has one.
pub fn wallpaper_get() -> Option<PathBuf> {
    let out = Command::new("wallpaper").arg("get").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let mut lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| {
            let l = l.trim();
            l.strip_prefix("//")
                .map(|r| format!("/{r}"))
                .unwrap_or_else(|| l.to_string())
        })
        .filter(|l| !l.is_empty())
        .collect();
    lines.sort();
    lines.dedup();
    lines.into_iter().next().map(PathBuf::from)
}

fn have(cmd: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|d| d.join(cmd).is_file())
}

pub fn set_desktop(cfg: &Config, img: &Path) {
    if cfg.no_apply {
        note(&format!(
            "[no-apply] would set the desktop wallpaper to {}",
            img.display()
        ));
        return;
    }
    if have("wallpaper") {
        // fill = cover the screen and crop the overflow — never letterbox.
        let filled = Command::new("wallpaper")
            .args(["set"])
            .arg(img)
            .args(["--scale", "fill"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !filled {
            let plain = Command::new("wallpaper")
                .arg("set")
                .arg(img)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !plain {
                die(&format!("wallpaper set failed for {}", img.display()));
            }
        }
    } else if std::env::var("XDG_CURRENT_DESKTOP")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
        && have("gsettings")
    {
        for key in ["picture-uri", "picture-uri-dark"] {
            let _ = Command::new("gsettings")
                .args(["set", "org.gnome.desktop.background", key])
                .arg(format!("file://{}", img.display()))
                .status();
        }
    } else if have("feh") {
        let _ = Command::new("feh").arg("--bg-fill").arg(img).status();
    } else {
        note(
            "desktop wallpaper not supported here (install the 'wallpaper' brew formula, feh, or GNOME)",
        );
    }
}

/// kitty's configured background opacity — the LAST `background_opacity`
/// line wins, exactly like the shell's awk read; unreadable means opaque.
fn kitty_opacity(cfg: &Config) -> f64 {
    let conf = fs::read_to_string(cfg.kitty_dir.join("kitty.conf")).unwrap_or_default();
    let mut op = 1.0f64;
    for line in conf.lines() {
        if let Some(rest) = line.strip_prefix("background_opacity")
            && rest.starts_with([' ', '\t'])
            && let Ok(v) = rest.trim().parse::<f64>()
        {
            op = v;
        }
    }
    op
}

/// One terminal emitter. The palette files are already on disk when these
/// run; an impl pushes colors to its terminal, best-effort.
trait Terminal {
    fn apply(&self, cfg: &Config);
}

/// Recolor every RUNNING kitty over its capability-scoped remote-control
/// socket (`remote_control_password "" set-colors`: a passwordless client
/// may call set-colors and nothing else). `--configured` updates the stored
/// config too, so future windows inherit the palette. An instance no socket
/// reaches keeps its old palette; its next window reads the include.
struct Kitty;
impl Terminal for Kitty {
    fn apply(&self, cfg: &Config) {
        let colors = cfg.cache_dir.join("colors-kitty.conf");
        let Ok(rd) = fs::read_dir("/tmp") else { return };
        for e in rd.flatten() {
            let name = e.file_name();
            let Some(n) = name.to_str() else { continue };
            if !n.starts_with("kitty-samuel-") {
                continue;
            }
            let p = e.path();
            let is_sock = fs::metadata(&p)
                .map(|m| {
                    use std::os::unix::fs::FileTypeExt;
                    m.file_type().is_socket()
                })
                .unwrap_or(false);
            if !is_sock {
                continue;
            }
            let _ = Command::new("kitten")
                .arg("@")
                .arg("--to")
                .arg(format!("unix:{}", p.display()))
                .args(["set-colors", "--all", "--configured"])
                .arg(&colors)
                .output();
        }
    }
}

/// Alacritty has no runtime socket; it live-reloads its config imports. We
/// own one managed colors file under alacritty's config dir — written only
/// when that dir exists — and the user imports it once from alacritty.toml.
struct Alacritty;
impl Terminal for Alacritty {
    fn apply(&self, cfg: &Config) {
        let dir = cfg
            .kitty_dir
            .parent()
            .map(|p| p.join("alacritty"))
            .unwrap_or_else(|| PathBuf::from("/nonexistent"));
        if dir.is_dir() {
            let _ = fs::copy(
                cfg.cache_dir.join("colors-alacritty.toml"),
                dir.join("theme-colors.toml"),
            );
        }
    }
}

/// Plain unix terminals: OSC 4/10/11/12 to the CALLER'S OWN tty only —
/// never a spray across /dev/pts, never stdout (tables and pipes stay
/// clean). Skipped inside kitty, whose socket path already applied.
struct OscTty;
impl Terminal for OscTty {
    fn apply(&self, cfg: &Config) {
        if std::env::var("KITTY_WINDOW_ID")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            return;
        }
        let Ok(pal) = fs::read_to_string(cfg.cache_dir.join("colors")) else {
            return;
        };
        let seq = osc_sequences(&pal);
        if let Ok(mut tty) = fs::OpenOptions::new().write(true).open("/dev/tty") {
            let _ = tty.write_all(seq.as_bytes());
        }
    }
}

fn osc_sequences(colors_file: &str) -> String {
    let mut out = String::new();
    let lines: Vec<&str> = colors_file.lines().collect();
    for (i, hex) in lines.iter().take(16).enumerate() {
        out.push_str(&format!("\x1b]4;{i};{hex}\x1b\\"));
    }
    if let Some(bg) = lines.first() {
        out.push_str(&format!("\x1b]11;{bg}\x1b\\"));
    }
    if let Some(fg) = lines.get(16).or_else(|| lines.get(7)) {
        out.push_str(&format!("\x1b]10;{fg}\x1b\\\x1b]12;{fg}\x1b\\"));
    }
    out
}

/// pigment derivation options — one place, so list/preview/set all derive
/// the identical palette.
pub fn derive_options() -> Options {
    Options::default()
}

/// The pigment cache directory for per-wallpaper schemes.
pub fn schemes_dir(cfg: &Config) -> PathBuf {
    cfg.cache_dir.join("schemes")
}

/// Derive (cached), floor against the EFFECTIVE background, export the
/// palette files, repoint kitty's include, and push to every terminal.
pub fn set_palette(cfg: &Config, img: &Path) {
    if cfg.no_apply {
        note(&format!(
            "[no-apply] would derive a palette from {}",
            img.display()
        ));
        return;
    }
    let pal: Palette = match pigment::cached_derive(img, &derive_options(), &schemes_dir(cfg)) {
        Ok(p) => p,
        Err(e) => die(&format!(
            "palette derivation failed on {}: {e}",
            img.display()
        )),
    };
    // wal's floor was measured against the OPAQUE background; a translucent
    // kitty draws over the background BLENDED with the wallpaper. Re-floor
    // every text color against that blend — hue kept, moved just far enough.
    // The emitters only exist on the Floored proof type.
    let eff = effective_background(pal.background(), kitty_opacity(cfg), pal.wallpaper_average);
    let pal = pal.floor_against(eff, cfg.contrast);

    if fs::create_dir_all(&cfg.cache_dir).is_err() {
        die(&format!("cannot write {}", cfg.cache_dir.display()));
    }
    let colors: String = pal
        .colors
        .iter()
        .map(|c| format!("{}\n", c.hex()))
        .collect();
    let writes = [
        (cfg.cache_dir.join("colors"), colors),
        (cfg.cache_dir.join("colors-kitty.conf"), pal.to_kitty()),
        (
            cfg.cache_dir.join("colors-alacritty.toml"),
            pal.to_alacritty(),
        ),
        (cfg.cache_dir.join("wal"), img.display().to_string()),
    ];
    for (path, content) in writes {
        if fs::write(&path, content).is_err() {
            die(&format!("cannot write {}", path.display()));
        }
    }
    if let Some(parent) = cfg.current.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let include = format!("include {}/colors-kitty.conf\n", cfg.cache_dir.display());
    if fs::write(&cfg.current, include).is_err() {
        die(&format!("cannot write {}", cfg.current.display()));
    }
    let terminals: [&dyn Terminal; 3] = [&Kitty, &Alacritty, &OscTty];
    for t in &terminals {
        t.apply(cfg);
    }
}

/// Apply `img` everywhere (or desktop-only), then say what is now current.
pub fn use_image(cfg: &Config, img: &Path, desktop_only: bool) {
    set_desktop(cfg, img);
    if !desktop_only {
        set_palette(cfg, img);
    }
    let size = img_size(img);
    let name = img.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let suffix = if size.is_empty() {
        String::new()
    } else {
        format!(" ({size})")
    };
    note(&format!("now: {name}{suffix}"));
}

#[cfg(test)]
mod tests {
    use pigment::{Mode, Palette, Rgb, effective_background};

    /// The measured shell regression: a mid-tone background reaches the
    /// floor on ONE side only, and the old lightness heuristic shipped
    /// 2.32:1 under a 4.5 request. The floor must reach 4.5 by moving to
    /// the DARKER (reachable) side.
    #[test]
    fn mid_tone_background_reaches_the_floor_on_the_dark_side() {
        let bg = Rgb::parse("#aaaaaa").unwrap();
        let accent = Rgb::parse("#777777").unwrap();
        let mut pal = Palette {
            colors: [accent; 16],
            foreground: accent,
            cursor: accent,
            wallpaper_average: bg,
            mode: Mode::Dark,
        };
        pal.colors[0] = bg;
        let eff = effective_background(pal.background(), 1.0, pal.wallpaper_average);
        assert_eq!(eff, bg);
        let pal = pal.floor_against(eff, 4.5);
        let out = pal.colors[1];
        assert!(out.contrast(bg) >= 4.5, "got {:.2}:1", out.contrast(bg));
        assert!(
            out.luminance() < bg.luminance(),
            "moved toward the endpoint that cannot reach the floor"
        );
    }
}

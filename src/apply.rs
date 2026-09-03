//! Applying a wallpaper: the desktop, the palette (pigment, replacing
//! pywal), and every terminal behind one trait — adding a terminal is one
//! more impl, nothing else changes. The kitty socket remains the ONE
//! sanctioned path to a live kitty (never SIGUSR1: a config reload resets
//! runtime state, and a theme change may touch colors only).

use crate::config::Config;
use crate::imaging::img_size;
use crate::ui::{die, note, parse_hex6};
use pigment::{Options, Palette, effective_background};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

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

/// Set the desktop. `Err` means the picture IS on the active Space but not
/// on the others — a partial apply, which the caller owes an exit status
/// (see [`settle`]) once it has finished the palette work.
pub fn set_desktop(cfg: &Config, img: &Path) -> Result<(), String> {
    if cfg.no_apply {
        note(&format!(
            "[no-apply] would set the desktop wallpaper to {}",
            img.display()
        ));
        return Ok(());
    }
    if have("wallpaper") {
        // The helper reaches only the ACTIVE Space of each screen, so note
        // when it started: spaces::sync_all_spaces tells the record it wrote
        // from an older one by that instant.
        #[cfg(target_os = "macos")]
        let started = std::time::SystemTime::now();
        // fill = cover the screen and crop the overflow — never letterbox.
        let filled = Command::new("wallpaper")
            .args(["set"])
            .arg(img)
            .args(["--scale", "fill", "--screen", "all"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !filled {
            let plain = Command::new("wallpaper")
                .arg("set")
                .arg(img)
                .args(["--screen", "all"])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !plain {
                die(&format!("wallpaper set failed for {}", img.display()));
            }
        }
        // Not fatal HERE — the palette must still apply, so the failure
        // travels back as an Err instead of dying mid-apply — but it does
        // reach the exit status.
        #[cfg(target_os = "macos")]
        crate::spaces::sync_all_spaces(img, started)?;
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
    Ok(())
}

/// Speak [`set_desktop`]'s verdict LAST, once the palette and the `now:`
/// line are done: a wallpaper that reached only the Space you happen to be
/// looking at is a partial apply, and a partial apply may not hand back a
/// zero exit status.
pub fn settle(desktop: Result<(), String>) {
    if let Err(e) = desktop {
        die(&format!("desktop set on the active Space only: {e}"));
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
        let me = rustix::process::getuid().as_raw();
        for e in rd.flatten() {
            let name = e.file_name();
            let Some(n) = name.to_str() else { continue };
            if !n.starts_with("kitty-samuel-") {
                continue;
            }
            let p = e.path();
            if !own_socket(&p, me) {
                continue;
            }
            let child = Command::new("kitten")
                .arg("@")
                .arg("--to")
                .arg(format!("unix:{}", p.display()))
                .args(["set-colors", "--all", "--configured"])
                .arg(&colors)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
            if let Ok(child) = child {
                // A rogue same-uid socket can accept the request and never
                // answer; a theme apply must not hang on a terminal. Cap each
                // child, kill and reap on expiry — best-effort, never fatal.
                wait_capped(child, Duration::from_secs(3));
            }
        }
    }
}

/// A `/tmp` entry we may hand the palette to: a socket (checked without
/// following symlinks) that WE own. A symlink, a non-socket, or another
/// principal's socket is skipped — no disclosure, no connection.
fn own_socket(p: &Path, me: u32) -> bool {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    fs::symlink_metadata(p)
        .map(|m| m.file_type().is_socket() && m.uid() == me)
        .unwrap_or(false)
}

/// Wait for a child up to `limit`, then kill and reap it. Polls rather than
/// blocking so a stalled peer cannot pin the call open.
fn wait_capped(mut child: std::process::Child, limit: Duration) {
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return,
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

/// Exactly `#rrggbb` — the only line shape the colors file legitimately
/// holds, and the only shape allowed into an escape sequence.
fn osc_color(l: &str) -> bool {
    l.strip_prefix('#').is_some_and(|h| parse_hex6(h).is_some())
}

fn osc_sequences(colors_file: &str) -> String {
    // Only lines that parse as a hex color reach the terminal — the gate the
    // sibling reader of this file (scheme_colors → swatch_row) already
    // applies. set_palette wrote the file itself moments earlier, but OSC is
    // a sink the shell never had, so it inherits no parity cover: validate
    // at the sink.
    let mut out = String::new();
    let lines: Vec<&str> = colors_file.lines().collect();
    for (i, hex) in lines.iter().take(16).enumerate() {
        if osc_color(hex) {
            out.push_str(&format!("\x1b]4;{i};{hex}\x1b\\"));
        }
    }
    if let Some(bg) = lines.first()
        && osc_color(bg)
    {
        out.push_str(&format!("\x1b]11;{bg}\x1b\\"));
    }
    // The writer emits exactly 16 lines (the derived foreground never lands
    // in this file), so foreground/cursor take color7 — the conventional
    // foreground slot.
    if let Some(fg) = lines.get(7)
        && osc_color(fg)
    {
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
    let desktop = set_desktop(cfg, img);
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
    settle(desktop);
}

#[cfg(test)]
mod tests {
    use super::{osc_color, osc_sequences, own_socket, set_desktop, wait_capped};
    use crate::config::Config;
    use pigment::{Mode, Palette, Rgb, effective_background};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    /// A wallpaper that reached only the active Space is a partial apply, so
    /// set_desktop answers with a Result the caller must settle. Under
    /// no-apply there is nothing to sync and nothing to touch: Ok, and the
    /// desktop is never approached.
    #[test]
    fn no_apply_set_desktop_answers_ok() {
        let nowhere = PathBuf::from("/nonexistent/theme-apply-test");
        let cfg = Config {
            wallpaper_dirs: vec![nowhere.clone()],
            wallpaper_dirs_display: String::new(),
            cache_dir: nowhere.clone(),
            kitty_dir: nowhere.clone(),
            current: nowhere.join("current-theme.conf"),
            formats: vec!["jpg".into()],
            contrast: 4.5,
            no_apply: true,
        };
        assert!(set_desktop(&cfg, &nowhere.join("x.jpg")).is_ok());
    }

    /// A theme apply must never hang on a terminal: a child that outlives its
    /// deadline is killed and reaped, and the call returns near the deadline.
    #[test]
    fn wait_capped_kills_a_slow_child() {
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let start = Instant::now();
        wait_capped(child, Duration::from_millis(300));
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "wait_capped did not bound the child"
        );
    }

    /// The socket predicate: our own socket qualifies; a regular file and a
    /// symlink-to-socket do not — no palette goes to a non-socket or through
    /// a symlink. (A foreign-uid socket also fails, but forging one needs
    /// root, so the uid arm is exercised by passing a bogus uid here.)
    #[test]
    fn own_socket_requires_a_real_socket_we_own() {
        use std::os::unix::net::UnixListener;
        let d = std::env::temp_dir().join(format!("theme-sock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let me = rustix::process::getuid().as_raw();

        let sock = d.join("s.sock");
        let _l = UnixListener::bind(&sock).unwrap();
        assert!(own_socket(&sock, me), "our own socket should qualify");
        assert!(!own_socket(&sock, me + 1), "a foreign uid must not qualify");

        let plain = d.join("plain");
        std::fs::write(&plain, b"x").unwrap();
        assert!(!own_socket(&plain, me), "a regular file is not a socket");

        let link = d.join("link.sock");
        std::os::unix::fs::symlink(&sock, &link).unwrap();
        assert!(!own_socket(&link, me), "a symlink must not be followed");

        let _ = std::fs::remove_dir_all(&d);
    }

    /// Poisoned colors-file lines must never reach the terminal: only exact
    /// `#rrggbb` is emitted, everything else — an ESC-laden line, a bad hex,
    /// a bare word, a `#` without six digits — is dropped. A mutant that
    /// evaluated the gate and then let the line through fails here.
    #[test]
    fn osc_sequences_emits_only_validated_hex() {
        let poisoned = concat!(
            "#123abc\n",           // valid slot 0 (also bg)
            "not-a-color\n",       // bare word
            "#zzzzzz\n",           // # but not hex
            "#12 \x1b]4;9;evil\n", // embedded escape
            "#abcdef\n",           // valid
            "123456\n",            // hex but no leading #
            "#7f7f7f\n"            // valid — reaches slot 6
        );
        let out = osc_sequences(poisoned);
        // The three well-formed colors appear; nothing else does.
        assert!(out.contains("#123abc"));
        assert!(out.contains("#abcdef"));
        assert!(out.contains("#7f7f7f"));
        for bad in ["not-a-color", "zzzzzz", "evil", "]4;9;", "123456\x1b"] {
            assert!(!out.contains(bad), "poisoned fragment leaked: {bad}");
        }
        // Only ESC bytes we ourselves framed (the OSC introducers) are present.
        assert_eq!(out.matches("\x1b]4;").count(), 3);
    }

    #[test]
    fn osc_color_is_exactly_hash_plus_six_hex() {
        assert!(osc_color("#0a0b0c"));
        assert!(osc_color("#ABCDEF"));
        for bad in [
            "0a0b0c", "#0a0b0", "#0a0b0cc", "#0a0b0g", "", "#", "##00000",
        ] {
            assert!(!osc_color(bad), "accepted a bad line: {bad:?}");
        }
    }

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

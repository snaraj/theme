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
/// Every request names the tool honestly (#95): no host is told this is a
/// browser.
pub const UA: &str = concat!(
    "theme/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/snaraj/theme)"
);
const FORMATS_ALL: &str = "jpg jpeg png webp gif bmp tif tiff";

/// What the terminal on stdout can show. Kitty graphics, the window a
/// palette must reach, OSC color writes and the TERMINAL label all match on
/// this one value, decided once by [`Term::detect`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TerminalClass {
    /// stdout is not a terminal: nothing here is drawn for a person.
    #[default]
    Pipe,
    /// A local kitty window, addressable over its own socket.
    Kitty,
    /// kitty's protocol on the tty, but no local window to address: over
    /// ssh, or behind a hop such as sudo that dropped KITTY_WINDOW_ID.
    KittyRemote,
    /// tmux, screen or zellij owns the screen. Kitty graphics are not
    /// attempted, even when kitty is the outer terminal.
    Multiplexed,
    /// Any other terminal: OSC colors, no inline pictures.
    Osc,
}

/// How color swatches are written: off under NO_COLOR (any non-empty value,
/// no-color.org), 24-bit where the terminal is kitty or says so in
/// COLORTERM, the 256-color cube elsewhere.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Colors {
    Off,
    Ansi256,
    #[default]
    True,
}

/// The terminal, resolved once and carried in [`Config`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Term {
    pub class: TerminalClass,
    pub colors: Colors,
    /// KITTY_LISTEN_ON's unix socket, wherever the variable is set.
    pub socket: Option<PathBuf>,
}

/// Everything detection reads, in one place, so [`Term::detect`] is a pure
/// function a test drives without a terminal. The tty answer comes from the
/// descriptor itself and outranks every variable: a pipe is a pipe whatever
/// the environment claims.
#[derive(Default)]
pub struct TermFacts {
    pub stdout_tty: bool,
    pub term: String,
    pub kitty_window: bool,
    pub listen_on: String,
    pub multiplexer: bool,
    pub ssh: bool,
    pub no_color: bool,
    pub colorterm: String,
}

impl TermFacts {
    fn read() -> TermFacts {
        use std::io::IsTerminal;
        let var = |k: &str| env::var(k).unwrap_or_default();
        let set = |keys: &[&str]| keys.iter().any(|k| !var(k).is_empty());
        TermFacts {
            stdout_tty: std::io::stdout().is_terminal(),
            term: var("TERM"),
            kitty_window: set(&["KITTY_WINDOW_ID"]),
            listen_on: var("KITTY_LISTEN_ON"),
            multiplexer: set(&["TMUX", "STY", "ZELLIJ"]),
            ssh: set(&["SSH_CONNECTION", "SSH_TTY"]),
            no_color: set(&["NO_COLOR"]),
            colorterm: var("COLORTERM"),
        }
    }
}

impl Term {
    pub fn detect(f: &TermFacts) -> Term {
        use TerminalClass::*;
        let class = if !f.stdout_tty {
            Pipe
        } else if f.multiplexer || f.term.starts_with("screen") || f.term.starts_with("tmux") {
            Multiplexed
        } else if f.kitty_window && !f.ssh {
            Kitty
        } else if f.kitty_window || f.term == "xterm-kitty" {
            KittyRemote
        } else {
            Osc
        };
        let colors = if f.no_color {
            Colors::Off
        } else if matches!(class, Pipe | Kitty | KittyRemote)
            || matches!(f.colorterm.as_str(), "truecolor" | "24bit")
        {
            Colors::True
        } else {
            Colors::Ansi256
        };
        let socket = f.listen_on.strip_prefix("unix:").map(PathBuf::from);
        Term {
            class,
            colors,
            socket,
        }
    }

    /// kitty's own protocol reaches the person reading stdout.
    pub fn kitty(&self) -> bool {
        matches!(
            self.class,
            TerminalClass::Kitty | TerminalClass::KittyRemote
        )
    }
}

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
    pub term: Term,
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
        let contrast: f64 = contrast_raw.parse().unwrap_or(f64::NAN);
        if !contrast.is_finite() || !(1.0..=21.0).contains(&contrast) {
            die("THEME_CONTRAST must be a finite number from 1 to 21");
        }

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
            term: Term::detect(&TermFacts::read()),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// One row per terminal class. The facts are the whole input, so each
    /// class is pinned without a real terminal.
    #[test]
    fn every_terminal_class_is_decided_by_its_facts() {
        use Colors::*;
        use TerminalClass::*;
        let tty = |term: &str| TermFacts {
            stdout_tty: true,
            term: term.into(),
            ..Default::default()
        };
        let kitty = |f: TermFacts| TermFacts {
            kitty_window: true,
            ..f
        };
        let rows = [
            (
                "pipe inside kitty",
                TermFacts {
                    stdout_tty: false,
                    ..kitty(tty("xterm-kitty"))
                },
                Pipe,
                True,
            ),
            ("plain kitty", kitty(tty("xterm-kitty")), Kitty, True),
            (
                "kitty over ssh",
                TermFacts {
                    ssh: true,
                    ..tty("xterm-kitty")
                },
                KittyRemote,
                True,
            ),
            (
                "kitty window id over ssh",
                TermFacts {
                    ssh: true,
                    ..kitty(tty("xterm-kitty"))
                },
                KittyRemote,
                True,
            ),
            (
                "tmux inside kitty",
                TermFacts {
                    multiplexer: true,
                    ..kitty(tty("tmux-256color"))
                },
                Multiplexed,
                Ansi256,
            ),
            (
                "screen inside kitty, TERM only",
                kitty(tty("screen-256color")),
                Multiplexed,
                Ansi256,
            ),
            (
                "zellij keeps kitty's TERM",
                TermFacts {
                    multiplexer: true,
                    colorterm: "truecolor".into(),
                    ..kitty(tty("xterm-kitty"))
                },
                Multiplexed,
                True,
            ),
            (
                "OSC terminal with truecolor",
                TermFacts {
                    colorterm: "24bit".into(),
                    ..tty("xterm-256color")
                },
                Osc,
                True,
            ),
            (
                "OSC terminal without COLORTERM",
                tty("xterm-256color"),
                Osc,
                Ansi256,
            ),
            (
                "TERM=xterm-kitty without KITTY_WINDOW_ID",
                tty("xterm-kitty"),
                KittyRemote,
                True,
            ),
            (
                "NO_COLOR in kitty",
                TermFacts {
                    no_color: true,
                    ..kitty(tty("xterm-kitty"))
                },
                Kitty,
                Off,
            ),
            (
                "NO_COLOR into a pipe",
                TermFacts {
                    no_color: true,
                    ..Default::default()
                },
                Pipe,
                Off,
            ),
        ];
        for (name, facts, class, colors) in rows {
            let t = Term::detect(&facts);
            assert_eq!((t.class, t.colors), (class, colors), "{name}");
        }
    }

    #[test]
    fn only_a_unix_listen_on_names_a_socket() {
        let socket = |v: &str| {
            Term::detect(&TermFacts {
                listen_on: v.into(),
                ..Default::default()
            })
            .socket
        };
        assert_eq!(
            socket("unix:/tmp/kitty-1"),
            Some(PathBuf::from("/tmp/kitty-1"))
        );
        assert_eq!(socket("tcp:localhost:1"), None);
        assert_eq!(socket(""), None);
    }

    /// #95: the header names this tool and its version, never a browser.
    #[test]
    fn the_user_agent_names_this_tool_and_its_version() {
        let ours = format!("theme/{} ", env!("CARGO_PKG_VERSION"));
        assert!(UA.starts_with(&ours), "{UA}");
        assert!(!UA.contains("Mozilla") && UA.is_ascii() && !UA.contains(char::is_control));
    }
}

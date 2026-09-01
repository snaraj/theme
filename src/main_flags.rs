//! Global flags, extracted from any argv position so the subcommands stay
//! flag-free — the shell dispatch's loop, typed.

use crate::imaging::{extend_image, rotate_image};
use crate::ui::die;
use std::path::Path;

#[derive(Default)]
pub struct Flags {
    pub rotate: String,
    pub extend: String,
    pub verbose: bool,
    pub list_n: usize,
    pub desktop_only: bool,
    pub wallpaper: String,
    pub source_url: String,
    /// `update --version <v>`: the requested release, empty = latest.
    pub version_sel: String,
    pub args: Vec<String>,
}

pub fn parse(argv: &[String]) -> Flags {
    let mut f = Flags {
        list_n: 10,
        ..Default::default()
    };
    let mut want_rotate = false;
    let mut want_n = false;
    let mut want_w = false;
    let mut want_version = false;
    let mut list_n_raw = String::from("10");
    for a in argv {
        if want_rotate {
            f.rotate = a.clone();
            want_rotate = false;
            continue;
        }
        if want_n {
            list_n_raw = a.clone();
            want_n = false;
            continue;
        }
        if want_w {
            f.wallpaper = a.clone();
            want_w = false;
            continue;
        }
        if want_version {
            f.version_sel = a.clone();
            want_version = false;
            continue;
        }
        match a.as_str() {
            "--rotate" => want_rotate = true,
            s if s.starts_with("--rotate=") => f.rotate = s["--rotate=".len()..].to_string(),
            "--extend" => f.extend = "000000".into(),
            s if s.starts_with("--extend=") => {
                f.extend = s["--extend=".len()..].trim_start_matches('#').to_string();
            }
            "-v" | "--verbose" => f.verbose = true,
            "-n" => want_n = true,
            s if s.starts_with("-n=") => list_n_raw = s[3..].to_string(),
            s if s.starts_with("--limit=") => list_n_raw = s["--limit=".len()..].to_string(),
            "--all" => list_n_raw = "0".into(),
            "--desktop-only" => f.desktop_only = true,
            "-w" | "--wallpaper" => want_w = true,
            s if s.starts_with("--wallpaper=") => {
                f.wallpaper = s["--wallpaper=".len()..].to_string();
            }
            // Leading `--version` is the version-command alias (falls
            // through to args); AFTER a command token it is `update`'s
            // release selector.
            "--version" if !f.args.is_empty() => want_version = true,
            s if s.starts_with("--version=") && !f.args.is_empty() => {
                f.version_sel = s["--version=".len()..].to_string();
            }
            _ => f.args.push(a.clone()),
        }
    }
    if want_rotate {
        die("--rotate takes left or right");
    }
    if want_n {
        die("-n takes a row count");
    }
    if want_w {
        die("--wallpaper takes a wallpaper name");
    }
    if want_version {
        die("--version takes a release version like v0.1.0");
    }
    if !matches!(f.rotate.as_str(), "" | "left" | "right") {
        die("--rotate takes left or right");
    }
    if list_n_raw.is_empty() || !list_n_raw.bytes().all(|b| b.is_ascii_digit()) {
        die("-n takes a row count (0 or --all = everything)");
    }
    f.list_n = list_n_raw.parse().unwrap_or(10);
    if !f.extend.is_empty()
        && (f.extend.len() != 6 || !f.extend.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        die("--extend takes a 6-digit hex color (default 000000)");
    }
    f
}

impl Flags {
    /// Apply --rotate / --extend to a scratch file about to be saved.
    pub fn apply_transforms(&self, path: &Path) {
        if !self.rotate.is_empty() {
            rotate_image(path, &self.rotate);
        }
        if !self.extend.is_empty() {
            extend_image(path, &self.extend);
        }
    }

    /// The " rotated left" / " extended" tail a transformed save carries.
    pub fn hint_suffix(&self) -> String {
        format!(
            "{}{}",
            if self.rotate.is_empty() {
                String::new()
            } else {
                format!(" rotated {}", self.rotate)
            },
            if self.extend.is_empty() {
                ""
            } else {
                " extended"
            }
        )
    }

    pub fn transforms_requested(&self) -> bool {
        !self.rotate.is_empty() || !self.extend.is_empty()
    }
}

//! Global flags, extracted from any argv position so the subcommands stay
//! flag-free — the shell dispatch's loop, typed.

use crate::imaging::{extend_image, rotate_image};
use crate::ui::{die, display_text};
use std::path::Path;

#[derive(Default)]
pub struct Flags {
    pub rotate: String,
    pub extend: String,
    pub verbose: bool,
    pub list_n: usize,
    pub desktop_only: bool,
    pub wallpaper: String,
    pub mkdir: String,
    pub source_url: String,
    pub args: Vec<String>,
}

/// `theme update`'s OWN argv grammar, parsed from the RAW argv — the global
/// flag pass never runs for update, so nothing can swallow a typo before
/// refusal, and `--version` exists for NO other command (a destructive verb
/// handed `--version` refuses it like any unknown flag, before any side
/// effect — Codex round 3). Accepted: zero or one `--version <v>` /
/// `--version=<v>`, `-h`/`--help`. Anything else — positional or flag —
/// refuses HERE, before any network call. Returns (want_help, version_sel).
pub fn parse_update(rest: &[String]) -> (bool, String) {
    let mut help = false;
    let mut version = String::new();
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        let value = match a.as_str() {
            "-h" | "--help" => {
                help = true;
                continue;
            }
            "--version" => match it.next().filter(|v| !v.starts_with('-')) {
                Some(v) => v.clone(),
                None => die("--version takes a release version like v0.1.0"),
            },
            s if s.starts_with("--version=") => s["--version=".len()..].to_string(),
            s => die(&format!(
                "unknown argument '{}' for 'theme update' — try: theme update --help",
                display_text(s)
            )),
        };
        if value.is_empty() {
            die("--version takes a release version like v0.1.0");
        }
        if !version.is_empty() {
            die("theme update takes one --version at most");
        }
        version = value;
    }
    (help, version)
}

pub fn parse(argv: &[String]) -> Flags {
    let mut f = Flags {
        list_n: 10,
        ..Default::default()
    };
    let mut want_rotate = false;
    let mut want_n = false;
    let mut want_w = false;
    let mut want_mkdir = false;
    let mut saw_mkdir = false;
    let mut list_n_raw = String::from("10");
    for a in argv {
        if want_mkdir {
            f.mkdir = a.clone();
            want_mkdir = false;
            continue;
        }
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
            "--mkdir" => {
                want_mkdir = true;
                saw_mkdir = true;
            }
            s if s.starts_with("--mkdir=") => {
                f.mkdir = s["--mkdir=".len()..].to_string();
                saw_mkdir = true;
            }
            "-w" | "--wallpaper" => want_w = true,
            s if s.starts_with("--wallpaper=") => {
                f.wallpaper = s["--wallpaper=".len()..].to_string();
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
    if want_mkdir {
        die("--mkdir takes one folder name (no slashes, no leading dot or dash)");
    }
    // --mkdir is the ONE caller-chosen component of a save path: one library
    // folder name, fenced here — before dispatch, so no traversal reaches the
    // saver's own guard, and nothing is downloaded first. A dangling
    // `--mkdir` leaves the name empty and refuses on the same line, and an
    // option-shaped value refuses rather than silently swallowing the flag
    // that followed it.
    if saw_mkdir
        && (f.mkdir.is_empty()
            || f.mkdir.starts_with(['.', '-'])
            || f.mkdir.contains('/')
            || f.mkdir.chars().count() > 64
            || f.mkdir.chars().any(|c| c.is_ascii_control()))
    {
        die("--mkdir takes one folder name (no slashes, no leading dot or dash)");
    }
    if !matches!(f.rotate.as_str(), "" | "left" | "right") {
        die("--rotate takes left or right");
    }
    f.list_n = row_count(&list_n_raw)
        .unwrap_or_else(|| die("-n takes a row count (0 or --all = everything)"));
    if !f.extend.is_empty()
        && (f.extend.len() != 6 || !f.extend.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        die("--extend takes a 6-digit hex color (default 000000)");
    }
    f
}

/// A row count, or nothing at all. Digits are not the same thing as a
/// number: a value larger than this machine can count rows with is not a
/// row count, and turning it into the default silently answered a question
/// nobody asked — `-n <past usize>` printed ten rows and exited 0.
fn row_count(raw: &str) -> Option<usize> {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    raw.parse().ok()
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

#[cfg(test)]
mod tests {
    use super::row_count;

    /// Every value `-n` is documented to take, and every one it is not.
    /// The overflow case is the one that used to pass: the bytes are all
    /// digits, so the shape check said yes, and the number that did not fit
    /// quietly became the default ten rows.
    #[test]
    fn a_row_count_is_a_number_this_machine_can_count_to() {
        assert_eq!(row_count("10"), Some(10));
        assert_eq!(row_count("0"), Some(0));
        assert_eq!(row_count(&usize::MAX.to_string()), Some(usize::MAX));
        for bad in [
            "",
            " ",
            "-1",
            "1.5",
            "1e3",
            "ten",
            "10 ",
            "0x10",
            "184467440737095516160",
            // usize::MAX + 1, exactly.
            "18446744073709551616",
        ] {
            assert_eq!(row_count(bad), None, "{bad} was accepted");
        }
    }
}

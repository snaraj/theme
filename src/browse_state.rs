//! Browser preferences are data, never commands or paths to open. A restored
//! path is useful only when it also belongs to the current library snapshot.

use crate::{config::Config, store};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

const NAME: &str = "browser-v1";
const MAX_BYTES: usize = 1_048_576;
const MAX_FAVORITES: usize = 512;
const MAX_HISTORY: usize = 128;

#[derive(Default, Debug, PartialEq)]
pub struct State {
    pub query: Vec<String>,
    pub favorites: BTreeSet<PathBuf>,
    pub history: Vec<PathBuf>,
}

impl State {
    pub fn load(cfg: &Config) -> Self {
        store::read(cfg, NAME, MAX_BYTES)
            .and_then(|b| Self::decode(&b))
            .unwrap_or_default()
    }

    pub fn save(&self, cfg: &Config) -> Result<(), String> {
        let bytes = self.encode();
        if bytes.len() > MAX_BYTES {
            return Err("browser preferences exceed the storage limit".into());
        }
        store::write(cfg, NAME, &bytes)
    }

    pub fn visit(&mut self, path: &Path) {
        if self.history.last().is_some_and(|p| p == path) {
            return;
        }
        self.history.push(path.to_owned());
        if self.history.len() > MAX_HISTORY {
            self.history.remove(0);
        }
    }

    pub fn favorite(&mut self, path: &Path) -> Result<bool, &'static str> {
        if self.favorites.remove(path) {
            return Ok(false);
        }
        if self.favorites.len() >= MAX_FAVORITES {
            return Err("favorite limit reached; remove a favorite first");
        }
        self.favorites.insert(path.to_owned());
        Ok(true)
    }

    fn encode(&self) -> Vec<u8> {
        let mut s = String::from("theme-browser-v1\n");
        for q in &self.query {
            s.push_str(&format!("Q {}\n", hex(q.as_bytes())));
        }
        for p in &self.favorites {
            s.push_str(&format!("F {}\n", hex(p.as_os_str().as_bytes())));
        }
        for p in &self.history {
            s.push_str(&format!("H {}\n", hex(p.as_os_str().as_bytes())));
        }
        s.into_bytes()
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_BYTES {
            return None;
        }
        let text = std::str::from_utf8(bytes).ok()?;
        let mut lines = text.lines();
        if lines.next()? != "theme-browser-v1" {
            return None;
        }
        let mut state = Self::default();
        for line in lines {
            let (kind, value) = line.split_once(' ')?;
            let bytes = unhex(value)?;
            match kind {
                "Q" if state.query.len() < 32 => {
                    let q = String::from_utf8(bytes).ok()?;
                    if q.is_empty() || q.chars().any(char::is_control) {
                        return None;
                    }
                    state.query.push(q);
                }
                "F" | "H" => {
                    let path = PathBuf::from(OsString::from_vec(bytes));
                    if !path.is_absolute() {
                        return None;
                    }
                    if kind == "F" && state.favorites.len() < MAX_FAVORITES {
                        state.favorites.insert(path);
                    } else if kind == "H" && state.history.len() < MAX_HISTORY {
                        state.history.push(path);
                    } else {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        Some(state)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.is_empty() || s.len() > 8192 || !s.len().is_multiple_of(2) {
        return None;
    }
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            Some(((pair[0] as char).to_digit(16)? * 16 + (pair[1] as char).to_digit(16)?) as u8)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_roundtrip_paths_without_shell_interpretation() {
        let mut s = State {
            query: vec!["blue".into(), "landscape".into()],
            ..State::default()
        };
        let p = PathBuf::from("/library/a calm picture #1.png");
        assert_eq!(s.favorite(&p), Ok(true));
        s.visit(&p);
        s.visit(&p);
        assert_eq!(s.history.len(), 1);
        assert_eq!(State::decode(&s.encode()), Some(s));
    }

    #[test]
    fn history_is_bounded_and_favorites_toggle() {
        let mut s = State::default();
        for i in 0..200 {
            s.visit(Path::new(&format!("/library/{i}.png")));
        }
        assert_eq!(s.history.len(), MAX_HISTORY);
        assert_eq!(s.history[0], Path::new("/library/72.png"));
        assert_eq!(s.favorite(Path::new("/library/a.png")), Ok(true));
        assert_eq!(s.favorite(Path::new("/library/a.png")), Ok(false));
        assert!(s.favorites.is_empty());
    }

    #[test]
    fn incomplete_unknown_or_relative_records_are_ignored_as_a_whole() {
        for bytes in [
            b"theme-browser-v2\n".as_slice(),
            b"theme-browser-v1\nQ 6",
            b"theme-browser-v1\nF 61",
        ] {
            assert!(State::decode(bytes).is_none());
        }
    }
}

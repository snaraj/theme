//! Emitters. Strings out, nothing else — applying them (sockets, config
//! files, ttys) is the CLI's job and jurisdiction.
//!
//! The terminal emitters live on [`Floored`], not [`Palette`]: forgetting
//! the contrast floor ships an unreadable terminal silently, so the type
//! system makes it unforgettable. Only the cache format — which stores the
//! pre-floor palette on purpose — stays on [`Palette`].

use crate::{Floored, Mode, Palette, Rgb};

impl Floored {
    /// kitty color file, shaped like the `colors-kitty.conf` the include
    /// chain already reads (foreground/background/cursor + color0-15).
    pub fn to_kitty(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str(&format!("foreground {}\n", self.foreground.hex()));
        out.push_str(&format!("background {}\n", self.background().hex()));
        out.push_str(&format!("cursor {}\n\n", self.cursor.hex()));
        for (i, c) in self.colors.iter().enumerate() {
            out.push_str(&format!("color{i} {}\n", c.hex()));
        }
        out
    }

    /// Alacritty TOML fragment (`[colors.*]` tables), importable from
    /// `alacritty.toml`.
    pub fn to_alacritty(&self) -> String {
        let named = |from: usize| {
            [
                "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
            ]
            .iter()
            .enumerate()
            .map(|(i, name)| format!("{name} = \"{}\"\n", self.colors[from + i].hex()))
            .collect::<String>()
        };
        format!(
            "[colors.primary]\nbackground = \"{}\"\nforeground = \"{}\"\n\n\
             [colors.cursor]\ncursor = \"{}\"\n\n\
             [colors.normal]\n{}\n[colors.bright]\n{}",
            self.background().hex(),
            self.foreground.hex(),
            self.cursor.hex(),
            named(0),
            named(8),
        )
    }

    /// OSC escape sequences recoloring any xterm-compatible terminal:
    /// OSC 4 per slot, then OSC 10/11/12 (foreground/background/cursor).
    pub fn to_osc(&self) -> String {
        fn spec(c: Rgb) -> String {
            format!("rgb:{:02x}/{:02x}/{:02x}", c.r, c.g, c.b)
        }
        let mut out = String::with_capacity(1024);
        for (i, c) in self.colors.iter().enumerate() {
            out.push_str(&format!("\x1b]4;{i};{}\x1b\\", spec(*c)));
        }
        out.push_str(&format!("\x1b]10;{}\x1b\\", spec(self.foreground)));
        out.push_str(&format!("\x1b]11;{}\x1b\\", spec(self.background())));
        out.push_str(&format!("\x1b]12;{}\x1b\\", spec(self.cursor)));
        out
    }
}

impl Palette {
    /// The plain-text cache/interchange format: a version tag, 16 color
    /// lines, then foreground, cursor, wallpaper average, and mode. Line
    /// oriented and greppable on purpose — no serializer dependency.
    pub fn to_cache_format(&self) -> String {
        let mut out = String::with_capacity(256);
        out.push_str("pigment1\n");
        for c in &self.colors {
            out.push_str(&c.hex());
            out.push('\n');
        }
        out.push_str(&format!(
            "{}\n{}\n{}\n",
            self.foreground.hex(),
            self.cursor.hex(),
            self.wallpaper_average.hex()
        ));
        out.push_str(match self.mode {
            Mode::Dark => "dark\n",
            Mode::Light => "light\n",
        });
        out
    }

    /// Parse [`Palette::to_cache_format`] output.
    pub fn from_cache_format(s: &str) -> Option<Palette> {
        let mut lines = s.lines();
        if lines.next()? != "pigment1" {
            return None;
        }
        let mut colors = [Rgb::BLACK; 16];
        for slot in &mut colors {
            *slot = Rgb::parse(lines.next()?)?;
        }
        let foreground = Rgb::parse(lines.next()?)?;
        let cursor = Rgb::parse(lines.next()?)?;
        let wallpaper_average = Rgb::parse(lines.next()?)?;
        let mode = match lines.next()? {
            "dark" => Mode::Dark,
            "light" => Mode::Light,
            _ => return None,
        };
        Some(Palette {
            colors,
            foreground,
            cursor,
            wallpaper_average,
            mode,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Palette {
        let mut colors = [Rgb::BLACK; 16];
        for (i, c) in colors.iter_mut().enumerate() {
            *c = Rgb {
                r: i as u8 * 10,
                g: 100,
                b: 200,
            };
        }
        Palette {
            colors,
            foreground: Rgb {
                r: 230,
                g: 230,
                b: 230,
            },
            cursor: Rgb {
                r: 230,
                g: 230,
                b: 230,
            },
            wallpaper_average: Rgb {
                r: 90,
                g: 80,
                b: 70,
            },
            mode: Mode::Dark,
        }
    }

    /// Wrap without mutation: a floor of 1.0 is an identity, since a
    /// contrast ratio is >= 1 by definition.
    fn floored() -> Floored {
        sample().floor_against(Rgb::BLACK, 1.0)
    }

    #[test]
    fn kitty_has_all_lines() {
        let k = floored().to_kitty();
        assert!(k.contains("foreground #e6e6e6"));
        assert!(k.contains("background #0064c8"));
        assert!(k.contains("cursor #e6e6e6"));
        for i in 0..16 {
            assert!(k.contains(&format!("color{i} #")), "missing color{i}");
        }
    }

    #[test]
    fn alacritty_has_all_tables() {
        let a = floored().to_alacritty();
        for t in [
            "[colors.primary]",
            "[colors.cursor]",
            "[colors.normal]",
            "[colors.bright]",
        ] {
            assert!(a.contains(t), "missing {t}");
        }
        for name in [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ] {
            assert_eq!(a.matches(&format!("{name} = ")).count(), 2, "{name}");
        }
    }

    #[test]
    fn osc_is_wellformed() {
        let o = floored().to_osc();
        assert_eq!(o.matches("\x1b]4;").count(), 16);
        assert!(o.contains("\x1b]10;rgb:e6/e6/e6\x1b\\"));
        assert!(o.contains("\x1b]11;rgb:00/64/c8\x1b\\"));
        assert!(o.contains("\x1b]12;rgb:e6/e6/e6\x1b\\"));
    }

    #[test]
    fn cache_roundtrip_is_lossless() {
        let p = sample();
        assert_eq!(Palette::from_cache_format(&p.to_cache_format()).unwrap(), p);
    }

    #[test]
    fn cache_rejects_garbage() {
        assert!(Palette::from_cache_format("").is_none());
        assert!(Palette::from_cache_format("pigment1\nnot-a-color\n").is_none());
        assert!(Palette::from_cache_format("wal\n#000000\n").is_none());
    }
}

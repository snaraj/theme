//! Emitters. Strings out, nothing else — applying them (sockets, config
//! files, ttys) is the CLI's job and jurisdiction.
//!
//! The terminal emitters live on [`Floored`], not [`Palette`]: forgetting
//! the contrast floor ships an unreadable terminal silently, so the type
//! system makes it unforgettable. Only the cache format — which stores the
//! pre-floor palette on purpose — stays on [`Palette`].

use crate::{Floored, ImageProfile, Mode, Palette, Rgb};

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
        let ui = self.interface();
        for (name, color) in [
            ("selection_foreground", ui.selection_foreground),
            ("selection_background", ui.selection_background),
            ("active_tab_foreground", ui.active_tab_foreground),
            ("active_tab_background", ui.active_tab_background),
            ("inactive_tab_foreground", ui.inactive_tab_foreground),
            ("inactive_tab_background", ui.inactive_tab_background),
            ("active_border_color", ui.active_border_color),
            ("inactive_border_color", ui.inactive_border_color),
            ("cursor_text_color", ui.cursor_text_color),
        ] {
            out.push_str(&format!("{name} {}\n", color.hex()));
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
        use std::fmt::Write;
        let mut out = String::with_capacity(256 + self.profile.colors.len() * 8);
        out.push_str("pigment3\n");
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
        out.push_str(&format!(
            "profile {} {} {} {}\n",
            self.profile.width, self.profile.height, self.profile.columns, self.profile.rows
        ));
        for color in &self.profile.colors {
            writeln!(out, "#{:02x}{:02x}{:02x}", color.r, color.g, color.b).unwrap();
        }
        for color in self.profile.bounds {
            writeln!(out, "{}", color.hex()).unwrap();
        }
        out
    }

    /// Parse [`Palette::to_cache_format`] output.
    pub fn from_cache_format(s: &str) -> Option<Palette> {
        CacheRecord::parse(s)?.into_palette()
    }
}

/// Validated cache data, before computing profile features. Color-only readers
/// still validate the entire record, but need no luminance/Oklab calculations.
pub(crate) struct CacheRecord {
    pub colors: [Rgb; 16],
    foreground: Rgb,
    cursor: Rgb,
    wallpaper_average: Rgb,
    mode: Mode,
    width: u32,
    height: u32,
    columns: usize,
    rows: usize,
    samples: Vec<Rgb>,
    bounds: [Rgb; 2],
}

impl CacheRecord {
    pub(crate) fn parse(s: &str) -> Option<Self> {
        Self::parse_samples::<true>(s)
    }

    pub(crate) fn parse_colors(s: &str) -> Option<[Rgb; 16]> {
        Self::parse_samples::<false>(s).map(|record| record.colors)
    }

    fn parse_samples<const KEEP: bool>(s: &str) -> Option<Self> {
        let mut lines = s.lines();
        if lines.next()? != "pigment3" {
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
        let mut shape = lines.next()?.split_whitespace();
        if shape.next()? != "profile" {
            return None;
        }
        let width = shape.next()?.parse().ok()?;
        let height = shape.next()?.parse().ok()?;
        let columns: usize = shape.next()?.parse().ok()?;
        let rows: usize = shape.next()?.parse().ok()?;
        if shape.next().is_some() || !ImageProfile::valid_shape(width, height, columns, rows) {
            return None;
        }
        let mut samples = Vec::with_capacity(if KEEP { columns * rows } else { 0 });
        let mut sampled_bounds = [Rgb::WHITE, Rgb::BLACK];
        for _ in 0..columns * rows {
            let sample = Rgb::parse(lines.next()?)?;
            let [low, high] = sampled_bounds;
            sampled_bounds = [
                Rgb {
                    r: low.r.min(sample.r),
                    g: low.g.min(sample.g),
                    b: low.b.min(sample.b),
                },
                Rgb {
                    r: high.r.max(sample.r),
                    g: high.g.max(sample.g),
                    b: high.b.max(sample.b),
                },
            ];
            if KEEP {
                samples.push(sample);
            }
        }
        let bounds = [Rgb::parse(lines.next()?)?, Rgb::parse(lines.next()?)?];
        if lines.next().is_some()
            || sampled_bounds
                .iter()
                .any(|&c| !ImageProfile::bounds_cover(bounds, c))
        {
            return None;
        }
        Some(Self {
            colors,
            foreground,
            cursor,
            wallpaper_average,
            mode,
            width,
            height,
            columns,
            rows,
            samples,
            bounds,
        })
    }

    pub(crate) fn into_palette(self) -> Option<Palette> {
        let Self {
            colors,
            foreground,
            cursor,
            wallpaper_average,
            mode,
            width,
            height,
            columns,
            rows,
            samples,
            bounds,
        } = self;
        let profile =
            ImageProfile::new(width, height, columns, rows, samples)?.with_bounds(bounds)?;
        Some(Palette {
            colors,
            foreground,
            cursor,
            wallpaper_average,
            profile,
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
            profile: ImageProfile::uniform(Rgb {
                r: 90,
                g: 80,
                b: 70,
            }),
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
    fn colors_only_preserves_full_record_acceptance() {
        let mut palette = sample();
        palette.profile = ImageProfile::new(
            512,
            384,
            16,
            16,
            (0..256)
                .map(|r| Rgb {
                    r: r as u8,
                    g: 40,
                    b: 190,
                })
                .collect(),
        )
        .unwrap();
        let valid = palette.to_cache_format();
        let check = |text: &str, expected| {
            assert_eq!(CacheRecord::parse_colors(text), expected);
            assert_eq!(Palette::from_cache_format(text).map(|p| p.colors), expected);
        };
        for text in [
            valid.clone(),
            valid.trim_end().into(),
            valid.replace('\n', "\r\n"),
            valid.replace('#', ""),
            valid.replace("profile 512 384 16 16", "profile\t+512\u{2003}0384 +16 016"),
        ] {
            check(&text, Some(palette.colors));
        }
        for end in 0..valid.len() - 1 {
            check(&valid[..end], None);
        }
        let lines: Vec<_> = valid.lines().collect();
        for i in 0..lines.len() {
            let mut changed = lines.clone();
            changed[i] = "invalid";
            check(&changed.join("\n"), None);
        }
        for shape in [
            "profile 0 384 16 16",
            "profile 512 0 16 16",
            "profile 512 384 0 16",
            "profile 512 384 17 16",
            "profile 512 384 16 17",
            "profile 15 384 16 16",
            "profile 512 15 16 16",
            "profile 4294967296 384 16 16",
            "profile 512 384 16 16 extra",
        ] {
            check(&valid.replace("profile 512 384 16 16", shape), None);
        }
        for suffix in ["\n", "extra\n", "#123456\n", "\r"] {
            check(&(valid.clone() + suffix), None);
        }
        // A narrowed or inverted bound would understate the required floor.
        for (position, color) in [(lines.len() - 2, "#ff28be"), (lines.len() - 1, "#0028be")] {
            let mut changed = lines.clone();
            changed[position] = color;
            check(&changed.join("\n"), None);
        }
    }

    #[test]
    fn cache_rejects_garbage() {
        assert!(Palette::from_cache_format("").is_none());
        assert!(Palette::from_cache_format("pigment1\nnot-a-color\n").is_none());
        assert!(Palette::from_cache_format("wal\n#000000\n").is_none());
        let valid = sample().to_cache_format();
        for invalid in [
            valid.replace("pigment3", "pigment2"),
            valid.replace("profile 1 1 1 1", "profile 1 1 17 1"),
            valid.replace("profile 1 1 1 1", "profile 0 1 1 1"),
            valid.clone() + "#123456\n",
        ] {
            assert!(Palette::from_cache_format(&invalid).is_none());
        }
    }
}

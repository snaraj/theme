//! Color types and the exact WCAG math the contrast floor depends on.

/// Both sRGB thresholds used below choose the same branch for 8-bit channels:
/// 0..=10 are linear, 11..=255 use the power curve. Compute those exact values
/// once, preserving the existing arithmetic while sharing it across profiles
/// and contrast calculations.
fn linear_channel(value: u8) -> f64 {
    static VALUES: std::sync::LazyLock<[f64; 256]> = std::sync::LazyLock::new(|| {
        std::array::from_fn(|value| {
            let value = value as f64 / 255.0;
            if value <= 0.03928 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        })
    });
    VALUES[usize::from(value)]
}

/// An sRGB color, 8 bits per channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgb {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
}

/// Hue (degrees), saturation, lightness — all HSL, used only inside derivation.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Hsl {
    pub h: f64,
    pub s: f64,
    pub l: f64,
}

impl Rgb {
    /// White.
    pub const WHITE: Rgb = Rgb {
        r: 255,
        g: 255,
        b: 255,
    };
    /// Black.
    pub const BLACK: Rgb = Rgb { r: 0, g: 0, b: 0 };

    /// Parse `RRGGBB` or `#RRGGBB`.
    pub fn parse(s: &str) -> Option<Rgb> {
        fn nibble(byte: u8) -> Option<u8> {
            match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                b'A'..=b'F' => Some(byte - b'A' + 10),
                _ => None,
            }
        }
        let &[r0, r1, g0, g1, b0, b1] = s.strip_prefix('#').unwrap_or(s).as_bytes() else {
            return None;
        };
        Some(Rgb {
            r: nibble(r0)? << 4 | nibble(r1)?,
            g: nibble(g0)? << 4 | nibble(g1)?,
            b: nibble(b0)? << 4 | nibble(b1)?,
        })
    }

    /// Format as `#rrggbb`.
    pub fn hex(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// WCAG relative luminance, with the exact constants the shell floor
    /// used (threshold 0.03928): changing them would move floor decisions.
    pub fn luminance(&self) -> f64 {
        0.2126 * linear_channel(self.r)
            + 0.7152 * linear_channel(self.g)
            + 0.0722 * linear_channel(self.b)
    }

    /// WCAG contrast ratio against `other`, in `1.0..=21.0`.
    pub fn contrast(&self, other: Rgb) -> f64 {
        let (a, b) = (self.luminance(), other.luminance());
        let (hi, lo) = if a >= b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// Oklab coordinates for perceptual color comparisons, from linear sRGB.
    pub fn oklab(&self) -> [f64; 3] {
        let (r, g, b) = (
            linear_channel(self.r),
            linear_channel(self.g),
            linear_channel(self.b),
        );
        let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
        let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
        let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
        [
            0.210_454_255_3 * l + 0.793_617_785 * m - 0.004_072_046_8 * s,
            1.977_998_495_1 * l - 2.428_592_205 * m + 0.450_593_709_9 * s,
            0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766 * s,
        ]
    }

    /// Per-channel mix toward `target` by `t` in `0.0..=1.0`.
    pub fn mix(&self, target: Rgb, t: f64) -> Rgb {
        fn m(c: u8, t8: u8, t: f64) -> u8 {
            (f64::from(c) + (f64::from(t8) - f64::from(c)) * t).round() as u8
        }
        Rgb {
            r: m(self.r, target.r, t),
            g: m(self.g, target.g, t),
            b: m(self.b, target.b, t),
        }
    }

    pub(crate) fn to_hsl(self) -> Hsl {
        let (r, g, b) = (
            f64::from(self.r) / 255.0,
            f64::from(self.g) / 255.0,
            f64::from(self.b) / 255.0,
        );
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let l = (max + min) / 2.0;
        let d = max - min;
        if d == 0.0 {
            return Hsl { h: 0.0, s: 0.0, l };
        }
        let s = d / (1.0 - (2.0 * l - 1.0).abs());
        let h = 60.0
            * if max == r {
                ((g - b) / d).rem_euclid(6.0)
            } else if max == g {
                (b - r) / d + 2.0
            } else {
                (r - g) / d + 4.0
            };
        Hsl { h, s, l }
    }
}

impl Hsl {
    pub(crate) fn to_rgb(self) -> Rgb {
        let c = (1.0 - (2.0 * self.l - 1.0).abs()) * self.s;
        let x = c * (1.0 - ((self.h / 60.0).rem_euclid(2.0) - 1.0).abs());
        let m = self.l - c / 2.0;
        let (r, g, b) = match self.h.rem_euclid(360.0) {
            h if h < 60.0 => (c, x, 0.0),
            h if h < 120.0 => (x, c, 0.0),
            h if h < 180.0 => (0.0, c, x),
            h if h < 240.0 => (0.0, x, c),
            h if h < 300.0 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        let q = |v: f64| ((v + m) * 255.0).round() as u8;
        Rgb {
            r: q(r),
            g: q(g),
            b: q(b),
        }
    }
}

/// Circular hue distance in degrees, `0.0..=180.0`.
pub(crate) fn hue_dist(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_table_preserves_both_reference_curves_exactly() {
        for value in 0..=255u8 {
            let normalized = f64::from(value) / 255.0;
            for threshold in [0.03928, 0.04045] {
                let reference = if normalized <= threshold {
                    normalized / 12.92
                } else {
                    ((normalized + 0.055) / 1.055).powf(2.4)
                };
                assert_eq!(linear_channel(value).to_bits(), reference.to_bits());
            }
        }
    }

    #[test]
    fn oklab_matches_reference_primaries() {
        let red = Rgb { r: 255, g: 0, b: 0 }.oklab();
        for (value, expected) in red
            .into_iter()
            .zip([0.627_955_36, 0.224_863_06, 0.125_846_30])
        {
            assert!((value - expected).abs() < 1e-8);
        }
        assert_eq!(Rgb::BLACK.oklab(), [0.0; 3]);
        let white = Rgb::WHITE.oklab();
        assert!((white[0] - 1.0).abs() < 1e-7);
        assert!(white[1].hypot(white[2]) < 1e-7);
    }

    #[test]
    fn hex_roundtrip() {
        for s in ["#000000", "#ffffff", "#12ab9f"] {
            assert_eq!(Rgb::parse(s).unwrap().hex(), s);
        }
        assert_eq!(Rgb::parse("12ab9f"), Rgb::parse("#12ab9f"));
        assert!(Rgb::parse("#12ab9").is_none());
        assert!(Rgb::parse("#12ab9x").is_none());
    }

    #[test]
    fn hex_parser_matches_reference_for_all_single_byte_changes() {
        let reference = |s: &str| {
            let s = s.strip_prefix('#').unwrap_or(s);
            if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let value = u32::from_str_radix(s, 16).ok()?;
            Some(Rgb {
                r: (value >> 16) as u8,
                g: (value >> 8) as u8,
                b: value as u8,
            })
        };
        for original in ["123456", "#ABCdef"] {
            for position in 0..original.len() {
                for byte in 0..=127u8 {
                    let mut changed = original.as_bytes().to_vec();
                    changed[position] = byte;
                    let text = std::str::from_utf8(&changed).unwrap();
                    assert_eq!(Rgb::parse(text), reference(text), "{text:?}");
                }
            }
        }
        for text in [
            "",
            "#",
            "##123456",
            "#1234567",
            "ééé",
            "éabcd",
            "１２３",
            "123456\n",
        ] {
            assert_eq!(Rgb::parse(text), reference(text), "{text:?}");
        }
    }

    #[test]
    fn wcag_known_values() {
        assert_eq!(Rgb::BLACK.luminance(), 0.0);
        assert!((Rgb::WHITE.luminance() - 1.0).abs() < 1e-9);
        assert!((Rgb::WHITE.contrast(Rgb::BLACK) - 21.0).abs() < 1e-9);
        assert!((Rgb::WHITE.contrast(Rgb::WHITE) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn mix_endpoints() {
        let c = Rgb {
            r: 10,
            g: 200,
            b: 90,
        };
        assert_eq!(c.mix(Rgb::WHITE, 0.0), c);
        assert_eq!(c.mix(Rgb::WHITE, 1.0), Rgb::WHITE);
        assert_eq!(c.mix(Rgb::BLACK, 1.0), Rgb::BLACK);
    }

    #[test]
    fn hsl_roundtrip_stays_close() {
        for c in [
            Rgb {
                r: 200,
                g: 30,
                b: 30,
            },
            Rgb {
                r: 10,
                g: 240,
                b: 120,
            },
            Rgb {
                r: 128,
                g: 128,
                b: 128,
            },
        ] {
            let back = c.to_hsl().to_rgb();
            assert!(
                i32::from(back.r).abs_diff(i32::from(c.r)) <= 1,
                "{c:?} -> {back:?}"
            );
            assert!(i32::from(back.g).abs_diff(i32::from(c.g)) <= 1);
            assert!(i32::from(back.b).abs_diff(i32::from(c.b)) <= 1);
        }
    }

    #[test]
    fn hue_distance_wraps() {
        assert_eq!(hue_dist(10.0, 350.0), 20.0);
        assert_eq!(hue_dist(0.0, 180.0), 180.0);
    }
}

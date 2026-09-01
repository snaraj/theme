//! Color types and the exact WCAG math the contrast floor depends on.

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
        let s = s.strip_prefix('#').unwrap_or(s);
        if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(s, 16).ok()?;
        Some(Rgb {
            r: (v >> 16) as u8,
            g: (v >> 8) as u8,
            b: v as u8,
        })
    }

    /// Format as `#rrggbb`.
    pub fn hex(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// WCAG relative luminance, with the exact constants the shell floor
    /// used (threshold 0.03928): changing them would move floor decisions.
    pub fn luminance(&self) -> f64 {
        fn ch(x: u8) -> f64 {
            let x = f64::from(x) / 255.0;
            if x <= 0.03928 {
                x / 12.92
            } else {
                ((x + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * ch(self.r) + 0.7152 * ch(self.g) + 0.0722 * ch(self.b)
    }

    /// WCAG contrast ratio against `other`, in `1.0..=21.0`.
    pub fn contrast(&self, other: Rgb) -> f64 {
        let (a, b) = (self.luminance(), other.luminance());
        let (hi, lo) = if a >= b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
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
    fn hex_roundtrip() {
        for s in ["#000000", "#ffffff", "#12ab9f"] {
            assert_eq!(Rgb::parse(s).unwrap().hex(), s);
        }
        assert_eq!(Rgb::parse("12ab9f"), Rgb::parse("#12ab9f"));
        assert!(Rgb::parse("#12ab9").is_none());
        assert!(Rgb::parse("#12ab9x").is_none());
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

//! The blend-aware contrast floor, ported 1:1 from theme.sh's
//! PALETTE_FLOOR_PY (reviewer-hardened; see theme/theme.sh lines 591-663 in
//! snaraj/dotfiles). Semantics preserved exactly:
//!
//! - effective background = opacity * palette bg + (1 - opacity) * wallpaper
//!   average, per channel, rounded;
//! - a color below the floor takes the SMALLEST binary-search mix toward
//!   whichever of white/black can ACHIEVABLY reach the floor (choosing by
//!   background lightness alone picked the wrong side on mid-tones and
//!   shipped 2.3:1 under a 4.5 request — the proven bug);
//! - when neither endpoint reaches it, the strongest endpoint wins rather
//!   than silently keeping an unreadable color;
//! - slots 1-15 and the foreground are floored; slot 0 (the background)
//!   never is. The cursor is not floored, matching the shell.
//!
//! One deliberate difference: rounding uses Rust's round-half-away-from-zero
//! where Python used round-half-even. Off-by-one on exact .5 midpoints only;
//! the floor invariant (documented above, tested below) is unaffected.

use crate::{Palette, Rgb};

const ITERATIONS: u32 = 24;

/// The background a translucent terminal actually draws text over:
/// `opacity * palette_bg + (1 - opacity) * wallpaper_avg`. At opacity 1.0
/// this reduces to the palette background itself.
pub fn effective_background(palette_bg: Rgb, opacity: f64, wallpaper_avg: Rgb) -> Rgb {
    let blend =
        |b: u8, w: u8| (opacity * f64::from(b) + (1.0 - opacity) * f64::from(w)).round() as u8;
    Rgb {
        r: blend(palette_bg.r, wallpaper_avg.r),
        g: blend(palette_bg.g, wallpaper_avg.g),
        b: blend(palette_bg.b, wallpaper_avg.b),
    }
}

/// Smallest mix toward `target` that reaches `floor` against `eff`, or None
/// when even the endpoint itself cannot.
fn solve(c: Rgb, target: Rgb, eff: Rgb, floor: f64) -> Option<(f64, Rgb)> {
    if target.contrast(eff) < floor {
        return None;
    }
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..ITERATIONS {
        let m = (lo + hi) / 2.0;
        if c.mix(target, m).contrast(eff) >= floor {
            hi = m;
        } else {
            lo = m;
        }
    }
    Some((hi, c.mix(target, hi)))
}

pub(crate) fn floor_color(c: Rgb, eff: Rgb, floor: f64) -> Rgb {
    if c.contrast(eff) >= floor {
        return c;
    }
    let best = [Rgb::WHITE, Rgb::BLACK]
        .into_iter()
        .filter_map(|t| solve(c, t, eff, floor))
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    match best {
        Some((_, mixed)) => mixed,
        None => {
            if Rgb::WHITE.contrast(eff) >= Rgb::BLACK.contrast(eff) {
                Rgb::WHITE
            } else {
                Rgb::BLACK
            }
        }
    }
}

/// A palette whose text colors have passed the contrast floor — the only
/// type the terminal emitters accept. Constructing one IS the proof the
/// floor ran; a palette that skipped it cannot reach a terminal. Reads
/// deref to [`Palette`].
pub struct Floored(pub(crate) Palette);

impl std::ops::Deref for Floored {
    type Target = Palette;
    fn deref(&self) -> &Palette {
        &self.0
    }
}

impl Palette {
    /// Floor every text color (slots 1-15 and the foreground) to at least
    /// `floor` contrast against `eff` — normally
    /// [`effective_background`]`(self.background(), opacity, self.wallpaper_average)`
    /// — and return the proof-of-floor wrapper the emitters require.
    pub fn floor_against(mut self, eff: Rgb, floor: f64) -> Floored {
        for i in 1..16 {
            self.colors[i] = floor_color(self.colors[i], eff, floor);
        }
        self.foreground = floor_color(self.foreground, eff, floor);
        Floored(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Mode, Palette};

    fn prng_color(state: &mut u64) -> Rgb {
        *state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let v = *state;
        Rgb {
            r: (v >> 16) as u8,
            g: (v >> 32) as u8,
            b: (v >> 48) as u8,
        }
    }

    /// The floor invariant over 500 deterministic pseudo-random cases:
    /// post-floor contrast reaches the target, or the target is proven
    /// unreachable from both endpoints and the strongest endpoint was taken.
    #[test]
    fn floor_invariant_holds() {
        let mut state = 7u64;
        for floor in [1.5, 3.0, 4.5, 7.0, 15.0] {
            for _ in 0..100 {
                let c = prng_color(&mut state);
                let eff = prng_color(&mut state);
                let out = floor_color(c, eff, floor);
                let reachable =
                    Rgb::WHITE.contrast(eff) >= floor || Rgb::BLACK.contrast(eff) >= floor;
                if reachable {
                    assert!(
                        out.contrast(eff) >= floor,
                        "{c:?} vs {eff:?} floored to {out:?}: {} < {floor}",
                        out.contrast(eff)
                    );
                } else {
                    let strongest = if Rgb::WHITE.contrast(eff) >= Rgb::BLACK.contrast(eff) {
                        Rgb::WHITE
                    } else {
                        Rgb::BLACK
                    };
                    assert_eq!(out, strongest);
                }
            }
        }
    }

    #[test]
    fn passing_colors_are_untouched() {
        let eff = Rgb::BLACK;
        let c = Rgb::WHITE;
        assert_eq!(floor_color(c, eff, 4.5), c);
    }

    #[test]
    fn effective_background_blends_and_reduces_when_opaque() {
        let bg = Rgb {
            r: 20,
            g: 20,
            b: 30,
        };
        let wp = Rgb {
            r: 200,
            g: 150,
            b: 180,
        };
        assert_eq!(effective_background(bg, 1.0, wp), bg);
        assert_eq!(effective_background(bg, 0.0, wp), wp);
        let half = effective_background(bg, 0.5, wp);
        assert_eq!(
            half,
            Rgb {
                r: 110,
                g: 85,
                b: 105
            }
        );
    }

    #[test]
    fn background_slot_never_floored() {
        let p = Palette {
            colors: [Rgb {
                r: 18,
                g: 18,
                b: 24,
            }; 16],
            foreground: Rgb {
                r: 30,
                g: 30,
                b: 30,
            },
            cursor: Rgb {
                r: 30,
                g: 30,
                b: 30,
            },
            wallpaper_average: Rgb {
                r: 18,
                g: 18,
                b: 24,
            },
            mode: Mode::Dark,
        };
        let eff = effective_background(p.background(), 1.0, p.wallpaper_average);
        let f = p.floor_against(eff, 4.5);
        assert_eq!(
            f.colors[0],
            Rgb {
                r: 18,
                g: 18,
                b: 24
            }
        );
        assert!(f.foreground.contrast(eff) >= 4.5);
        for i in 1..16 {
            assert!(f.colors[i].contrast(eff) >= 4.5);
        }
        // Cursor untouched, matching the shell implementation.
        assert_eq!(
            f.cursor,
            Rgb {
                r: 30,
                g: 30,
                b: 30
            }
        );
    }

    /// The measured shell scenario: light art under 0.60 opacity — mid-tone
    /// accents must be pushed until they read.
    #[test]
    fn translucent_light_wallpaper_case() {
        let bg = Rgb {
            r: 40,
            g: 30,
            b: 45,
        };
        let wp = Rgb {
            r: 235,
            g: 180,
            b: 200,
        }; // pink sky
        let eff = effective_background(bg, 0.60, wp);
        let accent = Rgb {
            r: 150,
            g: 120,
            b: 140,
        }; // mid-tone, unreadable
        let out = floor_color(accent, eff, 4.5);
        assert!(out.contrast(eff) >= 4.5);
    }
}

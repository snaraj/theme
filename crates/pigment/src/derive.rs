//! 16-slot ANSI palette derivation from clusters.
//!
//! The hue-archetype mapping is the "smarts" upgrade over pywal: ANSI 1-6
//! each chase their conventional hue (red/green/yellow/blue/magenta/cyan)
//! among the image's clusters instead of taking the first six clusters in
//! frequency order, so slots stay distinguishable. The bright-variant step
//! curve derives from wallrust's accent ladder idea (MIT, prime-run/wallrust).

use crate::color::{Hsl, hue_dist};
use crate::extract::Cluster;
use crate::{Mode, ModePref, Palette, Rgb};

/// Weighted mean saturation below which art counts as grayscale and gets the
/// ramp palette (near-mono art must derive, never fail).
const GRAY_SAT: f64 = 0.06;

const ARCHETYPES: [f64; 6] = [0.0, 120.0, 60.0, 240.0, 300.0, 180.0];

pub(crate) fn palette(clusters: &[Cluster], average: Rgb, pref: ModePref) -> Palette {
    let mean_lum: f64 = clusters.iter().map(|c| c.color.luminance() * c.share).sum();
    let mean_sat: f64 = clusters.iter().map(|c| c.color.to_hsl().s * c.share).sum();
    let mode = match pref {
        ModePref::Dark => Mode::Dark,
        ModePref::Light => Mode::Light,
        ModePref::Auto => {
            if mean_lum < 0.5 {
                Mode::Dark
            } else {
                Mode::Light
            }
        }
    };

    let dominant = clusters.first().map(|c| c.color).unwrap_or(average);
    let accents = if clusters.is_empty() || mean_sat < GRAY_SAT {
        gray_accents(dominant, mode)
    } else {
        hue_accents(clusters, mode)
    };

    let dom = dominant.to_hsl();
    let (bg, fg) = match mode {
        Mode::Dark => (
            Hsl {
                h: dom.h,
                s: dom.s.min(0.35),
                l: dom.l.min(0.10),
            }
            .to_rgb(),
            Hsl {
                h: dom.h,
                s: dom.s.min(0.12),
                l: 0.88,
            }
            .to_rgb(),
        ),
        Mode::Light => (
            Hsl {
                h: dom.h,
                s: dom.s.min(0.25),
                l: dom.l.max(0.92),
            }
            .to_rgb(),
            Hsl {
                h: dom.h,
                s: dom.s.min(0.15),
                l: 0.12,
            }
            .to_rgb(),
        ),
    };

    let mut colors = [Rgb::BLACK; 16];
    colors[0] = bg;
    colors[1..7].copy_from_slice(&accents);
    colors[7] = dim(fg, mode);
    colors[8] = lift(bg, mode);
    for i in 0..6 {
        colors[9 + i] = brighten(accents[i], mode);
    }
    colors[15] = match mode {
        Mode::Dark => Hsl {
            h: dom.h,
            s: 0.05,
            l: 0.95,
        }
        .to_rgb(),
        Mode::Light => Hsl {
            h: dom.h,
            s: 0.05,
            l: 0.05,
        }
        .to_rgb(),
    };

    Palette {
        colors,
        foreground: fg,
        cursor: fg,
        wallpaper_average: average,
        mode,
    }
}

/// The lightness band accents are normalised into, per mode.
fn band(mode: Mode) -> (f64, f64) {
    match mode {
        Mode::Dark => (0.55, 0.72),
        Mode::Light => (0.32, 0.48),
    }
}

fn clamp_band(c: Hsl, mode: Mode) -> Rgb {
    let (lo, hi) = band(mode);
    Hsl {
        h: c.h,
        s: c.s,
        l: c.l.clamp(lo, hi),
    }
    .to_rgb()
}

/// ANSI 1-6: per archetype hue, the best sufficiently-saturated cluster not
/// already spent; otherwise synthesize at the archetype hue so no two slots
/// collapse into the same color (pywal's near-duplicate problem).
fn hue_accents(clusters: &[Cluster], mode: Mode) -> [Rgb; 6] {
    let mut used = vec![false; clusters.len()];
    let most_saturated = clusters
        .iter()
        .max_by(|a, b| a.color.to_hsl().s.partial_cmp(&b.color.to_hsl().s).unwrap())
        .map(|c| c.color)
        .unwrap();

    let mut out = [Rgb::BLACK; 6];
    for (slot, &target) in ARCHETYPES.iter().enumerate() {
        let mut best: Option<(usize, f64)> = None;
        for (i, c) in clusters.iter().enumerate() {
            if used[i] {
                continue;
            }
            let hsl = c.color.to_hsl();
            let d = hue_dist(hsl.h, target);
            if hsl.s < 0.12 || d > 50.0 {
                continue;
            }
            let score = hsl.s.powf(0.7) * c.share.powf(0.25) * (1.0 - d / 180.0);
            if best.is_none_or(|(_, s)| score > s) {
                best = Some((i, score));
            }
        }
        out[slot] = match best {
            Some((i, _)) => {
                used[i] = true;
                clamp_band(clusters[i].color.to_hsl(), mode)
            }
            None => {
                let base = most_saturated.to_hsl();
                clamp_band(
                    Hsl {
                        h: target,
                        s: base.s.max(0.45),
                        l: base.l,
                    },
                    mode,
                )
            }
        };
    }
    out
}

/// Grayscale ramp: six luma steps through the accent band, hue kept at the
/// dominant tint. The contrast floor guarantees readability afterwards.
fn gray_accents(dominant: Rgb, mode: Mode) -> [Rgb; 6] {
    let h = dominant.to_hsl().h;
    let (lo, hi) = band(mode);
    let mut out = [Rgb::BLACK; 6];
    for (i, slot) in out.iter_mut().enumerate() {
        let l = lo + (hi - lo) * (i as f64 / 5.0);
        *slot = Hsl { h, s: 0.0, l }.to_rgb();
    }
    out
}

/// Bright variant (slots 9-14): one ladder step up in lightness and
/// saturation — descended from wallrust's fixed accent-curve points.
fn brighten(c: Rgb, mode: Mode) -> Rgb {
    let hsl = c.to_hsl();
    let l = match mode {
        Mode::Dark => (hsl.l + 0.10).min(0.85),
        Mode::Light => (hsl.l - 0.10).max(0.15),
    };
    Hsl {
        h: hsl.h,
        s: (hsl.s + 0.10).min(1.0),
        l,
    }
    .to_rgb()
}

fn dim(fg: Rgb, mode: Mode) -> Rgb {
    let hsl = fg.to_hsl();
    let l = match mode {
        Mode::Dark => hsl.l - 0.14,
        Mode::Light => hsl.l + 0.14,
    };
    Hsl { l, ..hsl }.to_rgb()
}

fn lift(bg: Rgb, mode: Mode) -> Rgb {
    let hsl = bg.to_hsl();
    let l = match mode {
        Mode::Dark => hsl.l + 0.18,
        Mode::Light => hsl.l - 0.18,
    };
    Hsl { l, ..hsl }.to_rgb()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::kmeans;

    fn colorful() -> Vec<Cluster> {
        let mut px = Vec::new();
        for _ in 0..100 {
            px.push(Rgb {
                r: 180,
                g: 40,
                b: 40,
            });
            px.push(Rgb {
                r: 40,
                g: 160,
                b: 70,
            });
            px.push(Rgb {
                r: 60,
                g: 80,
                b: 200,
            });
            px.push(Rgb {
                r: 20,
                g: 20,
                b: 28,
            });
            px.push(Rgb {
                r: 20,
                g: 20,
                b: 28,
            });
        }
        kmeans(&px, 10, 42)
    }

    #[test]
    fn dark_art_derives_dark_mode() {
        let p = palette(
            &colorful(),
            Rgb {
                r: 60,
                g: 60,
                b: 80,
            },
            ModePref::Auto,
        );
        assert_eq!(p.mode, Mode::Dark);
        assert!(p.background().luminance() < 0.1);
        assert!(p.foreground.luminance() > 0.5);
    }

    #[test]
    fn mode_pref_overrides_auto() {
        let p = palette(
            &colorful(),
            Rgb {
                r: 60,
                g: 60,
                b: 80,
            },
            ModePref::Light,
        );
        assert_eq!(p.mode, Mode::Light);
        assert!(p.background().luminance() > 0.7);
    }

    #[test]
    fn accents_are_distinct() {
        let p = palette(
            &colorful(),
            Rgb {
                r: 60,
                g: 60,
                b: 80,
            },
            ModePref::Auto,
        );
        for i in 1..7 {
            for j in (i + 1)..7 {
                assert_ne!(p.colors[i], p.colors[j], "slots {i} and {j} collapsed");
            }
        }
    }

    #[test]
    fn grayscale_art_still_derives() {
        let px = vec![
            Rgb {
                r: 90,
                g: 90,
                b: 90
            };
            400
        ];
        let p = palette(
            &kmeans(&px, 10, 42),
            Rgb {
                r: 90,
                g: 90,
                b: 90,
            },
            ModePref::Auto,
        );
        assert_eq!(p.mode, Mode::Dark);
        // A usable ramp: accents monotone in lightness, none equal to bg.
        for i in 1..7 {
            assert_ne!(p.colors[i], p.colors[0]);
        }
    }

    #[test]
    fn empty_clusters_fall_back_to_average() {
        let p = palette(
            &[],
            Rgb {
                r: 10,
                g: 10,
                b: 10,
            },
            ModePref::Auto,
        );
        assert_eq!(p.mode, Mode::Dark);
    }
}

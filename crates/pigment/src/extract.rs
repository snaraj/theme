//! Deterministic k-means++ over the downsampled grid. In-house on purpose:
//! a seeded PRNG plus Lloyd iterations is ~100 lines, and owning it is what
//! makes palettes reproducible enough for gold-file tests.

use crate::Rgb;

/// A color cluster: centroid plus its share of the sampled pixels.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Cluster {
    pub color: Rgb,
    pub share: f64,
}

/// splitmix64 — tiny, seedable, and good enough for center initialisation.
struct Prng(u64);

impl Prng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0.0..1.0`.
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn dist2(a: [f64; 3], b: [f64; 3]) -> f64 {
    let d0 = a[0] - b[0];
    let d1 = a[1] - b[1];
    let d2 = a[2] - b[2];
    d0 * d0 + d1 * d1 + d2 * d2
}

fn to_f(c: Rgb) -> [f64; 3] {
    [f64::from(c.r), f64::from(c.g), f64::from(c.b)]
}

/// Cluster `pixels` into at most `k` colors, largest share first.
pub(crate) fn kmeans(pixels: &[Rgb], k: usize, seed: u64) -> Vec<Cluster> {
    if pixels.is_empty() {
        return Vec::new();
    }
    let points: Vec<[f64; 3]> = pixels.iter().map(|&c| to_f(c)).collect();

    // k-means++ initialisation, seeded.
    let mut rng = Prng(seed);
    let mut centers: Vec<[f64; 3]> = vec![points[(rng.next() % points.len() as u64) as usize]];
    let mut d2: Vec<f64> = points.iter().map(|&p| dist2(p, centers[0])).collect();
    while centers.len() < k {
        let sum: f64 = d2.iter().sum();
        if sum <= f64::EPSILON {
            break; // fewer distinct colors than k
        }
        let mut pick = rng.unit() * sum;
        let mut idx = points.len() - 1;
        for (i, &d) in d2.iter().enumerate() {
            if pick <= d {
                idx = i;
                break;
            }
            pick -= d;
        }
        let c = points[idx];
        centers.push(c);
        for (di, p) in d2.iter_mut().zip(&points) {
            *di = di.min(dist2(*p, c));
        }
    }

    // Lloyd iterations; the assignment pass is the hot loop of the crate.
    let mut assign = vec![0usize; points.len()];
    for _ in 0..12 {
        let mut moved = false;
        for (a, p) in assign.iter_mut().zip(&points) {
            let mut best = 0;
            let mut bd = f64::MAX;
            for (ci, &c) in centers.iter().enumerate() {
                let d = dist2(*p, c);
                if d < bd {
                    bd = d;
                    best = ci;
                }
            }
            if *a != best {
                *a = best;
                moved = true;
            }
        }
        if !moved {
            break;
        }
        let mut sums = vec![[0.0f64; 3]; centers.len()];
        let mut counts = vec![0usize; centers.len()];
        for (&a, p) in assign.iter().zip(&points) {
            sums[a][0] += p[0];
            sums[a][1] += p[1];
            sums[a][2] += p[2];
            counts[a] += 1;
        }
        for (c, (s, &n)) in centers.iter_mut().zip(sums.iter().zip(&counts)) {
            if n > 0 {
                *c = [s[0] / n as f64, s[1] / n as f64, s[2] / n as f64];
            }
        }
    }

    let mut counts = vec![0usize; centers.len()];
    for &a in &assign {
        counts[a] += 1;
    }
    let total = points.len() as f64;
    let mut out: Vec<Cluster> = centers
        .iter()
        .zip(&counts)
        .filter(|&(_, &n)| n > 0)
        .map(|(&c, &n)| Cluster {
            color: Rgb {
                r: c[0].round() as u8,
                g: c[1].round() as u8,
                b: c[2].round() as u8,
            },
            share: n as f64 / total,
        })
        .collect();
    // Sort by share desc, then color for a total, deterministic order.
    out.sort_by(|a, b| {
        b.share
            .partial_cmp(&a.share)
            .unwrap()
            .then_with(|| (a.color.r, a.color.g, a.color.b).cmp(&(b.color.r, b.color.g, b.color.b)))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quadrants() -> Vec<Rgb> {
        let mut px = Vec::new();
        for y in 0..40 {
            for x in 0..40 {
                px.push(match (x < 20, y < 20) {
                    (true, true) => Rgb {
                        r: 200,
                        g: 30,
                        b: 30,
                    },
                    (false, true) => Rgb {
                        r: 30,
                        g: 180,
                        b: 60,
                    },
                    (true, false) => Rgb {
                        r: 40,
                        g: 90,
                        b: 220,
                    },
                    (false, false) => Rgb {
                        r: 230,
                        g: 210,
                        b: 80,
                    },
                });
            }
        }
        px
    }

    #[test]
    fn deterministic_across_runs() {
        let px = quadrants();
        let a = kmeans(&px, 10, 42);
        let b = kmeans(&px, 10, 42);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.color, y.color);
            assert_eq!(x.share, y.share);
        }
    }

    #[test]
    fn finds_the_four_quadrant_colors() {
        let clusters = kmeans(&quadrants(), 10, 42);
        for want in [
            Rgb {
                r: 200,
                g: 30,
                b: 30,
            },
            Rgb {
                r: 30,
                g: 180,
                b: 60,
            },
            Rgb {
                r: 40,
                g: 90,
                b: 220,
            },
            Rgb {
                r: 230,
                g: 210,
                b: 80,
            },
        ] {
            assert!(
                clusters.iter().any(|c| c.color == want),
                "missing {want:?} in {clusters:?}"
            );
        }
    }

    #[test]
    fn monochrome_collapses_below_k() {
        let px = vec![
            Rgb {
                r: 77,
                g: 77,
                b: 77
            };
            500
        ];
        let clusters = kmeans(&px, 10, 42);
        assert_eq!(clusters.len(), 1);
        assert_eq!(
            clusters[0].color,
            Rgb {
                r: 77,
                g: 77,
                b: 77
            }
        );
        assert_eq!(clusters[0].share, 1.0);
    }

    #[test]
    fn empty_input_is_empty() {
        assert!(kmeans(&[], 10, 42).is_empty());
    }
}

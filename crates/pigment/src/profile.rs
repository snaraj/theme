//! Small spatial summaries. These describe sampled regions, never every pixel
//! behind a terminal, its crop, blur, or the contents of other windows.

use crate::{Floored, Rgb, effective_background};

/// Maximum number of rows or columns in the persisted spatial profile.
pub const PROFILE_EDGE: usize = 16;

/// A bounded image summary, independent of opacity and terminal colors.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageProfile {
    /// Original image width in pixels.
    pub width: u32,
    /// Original image height in pixels.
    pub height: u32,
    /// Mean relative luminance of the sampled regions, in 0..=1.
    pub mean_luminance: f64,
    /// Mean Oklab chroma of the sampled regions.
    pub chroma: f64,
    /// Mean luminance difference between adjacent regions; lower is calmer.
    pub texture: f64,
    /// Mean Oklab coordinates, useful for comparing overall image colors.
    pub signature: [f64; 3],
    pub(crate) columns: usize,
    pub(crate) rows: usize,
    pub(crate) colors: Vec<Rgb>,
}

/// Readability across every sampled region and all default/ANSI text colors.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Readability {
    /// Lowest contrast ratio among all sampled regions and text colors.
    pub worst: f64,
    /// Fraction of sampled regions where every text color meets the target.
    pub coverage: f64,
    /// Number of sampled regions; this is not a full-resolution pixel count.
    pub samples: usize,
}

impl ImageProfile {
    /// A one-pixel uniform profile, useful for manually constructed palettes.
    pub fn uniform(color: Rgb) -> Self {
        Self::new(1, 1, 1, 1, vec![color]).unwrap()
    }

    pub(crate) fn valid_shape(width: u32, height: u32, columns: usize, rows: usize) -> bool {
        width > 0
            && height > 0
            && (1..=PROFILE_EDGE).contains(&columns)
            && (1..=PROFILE_EDGE).contains(&rows)
            && columns <= width as usize
            && rows <= height as usize
    }

    pub(crate) fn new(
        width: u32,
        height: u32,
        columns: usize,
        rows: usize,
        colors: Vec<Rgb>,
    ) -> Option<Self> {
        if !Self::valid_shape(width, height, columns, rows) || colors.len() != columns * rows {
            return None;
        }
        let count = colors.len() as f64;
        let luminance: Vec<_> = colors.iter().map(Rgb::luminance).collect();
        let mean_luminance = luminance.iter().sum::<f64>() / count;
        let (mut signature, mut chroma, mut texture, mut edges) = ([0.0; 3], 0.0, 0.0, 0);
        for (i, color) in colors.iter().enumerate() {
            let lab = color.oklab();
            for (total, value) in signature.iter_mut().zip(lab) {
                *total += value / count;
            }
            chroma += lab[1].hypot(lab[2]) / count;
            if i % columns != 0 {
                texture += (luminance[i] - luminance[i - 1]).abs();
                edges += 1;
            }
            if i >= columns {
                texture += (luminance[i] - luminance[i - columns]).abs();
                edges += 1;
            }
        }
        Some(Self {
            width,
            height,
            mean_luminance,
            chroma,
            texture: if edges == 0 {
                0.0
            } else {
                texture / f64::from(edges)
            },
            signature,
            columns,
            rows,
            colors,
        })
    }

    /// Summarize the existing analysis grid without decoding the image again.
    pub(crate) fn from_grid(
        width: u32,
        height: u32,
        grid_width: usize,
        grid_height: usize,
        pixels: &[Rgb],
    ) -> Self {
        let columns = grid_width.min(PROFILE_EDGE);
        let rows = grid_height.min(PROFILE_EDGE);
        let mut sums = vec![[0u64; 3]; columns * rows];
        let mut counts = vec![0u64; columns * rows];
        let x_cells: Vec<_> = (0..grid_width).map(|x| x * columns / grid_width).collect();
        for (y, row) in pixels.chunks(grid_width).enumerate() {
            let row_cell = (y * rows / grid_height) * columns;
            for (color, &x) in row.iter().zip(&x_cells) {
                let cell = row_cell + x;
                for (total, value) in sums[cell].iter_mut().zip([color.r, color.g, color.b]) {
                    *total += u64::from(value);
                }
                counts[cell] += 1;
            }
        }
        let colors = sums
            .into_iter()
            .zip(counts)
            .map(|(sum, count)| {
                let channel = |i| ((sum[i] + count / 2) / count) as u8;
                Rgb {
                    r: channel(0),
                    g: channel(1),
                    b: channel(2),
                }
            })
            .collect();
        Self::new(width, height, columns, rows, colors).unwrap()
    }

    /// Oklab distance between overall image colors. Texture remains a separate
    /// dimension so a caller can prefer calmer images without changing hue.
    pub fn distance(&self, other: &Self) -> f64 {
        self.signature
            .iter()
            .zip(other.signature)
            .map(|(a, b)| (a - b).powi(2))
            .sum::<f64>()
            .sqrt()
    }

    /// Evaluate the final palette over each sampled region at this opacity.
    /// Invalid opacity/target yields no passing regions instead of a false pass.
    pub fn readability(&self, palette: &Floored, opacity: f64, target: f64) -> Readability {
        let mut result = Readability {
            worst: 21.0,
            coverage: 0.0,
            samples: self.colors.len(),
        };
        if !opacity.is_finite()
            || !(0.0..=1.0).contains(&opacity)
            || !target.is_finite()
            || !(1.0..=21.0).contains(&target)
        {
            result.worst = 1.0;
            return result;
        }
        let text_luminance: Vec<_> = palette.colors[1..]
            .iter()
            .chain([&palette.foreground])
            .map(Rgb::luminance)
            .collect();
        let mut passing = 0;
        for &region in &self.colors {
            let background =
                effective_background(palette.background(), opacity, region).luminance();
            let worst = text_luminance
                .iter()
                .map(|&foreground| {
                    (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05)
                })
                .fold(21.0, f64::min);
            result.worst = result.worst.min(worst);
            passing += usize::from(worst >= target);
        }
        result.coverage = passing as f64 / result.samples as f64;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModePref, derive::palette};

    #[test]
    fn dark_and_light_regions_are_not_hidden_by_the_average() {
        let profile = ImageProfile::new(2, 1, 2, 1, vec![Rgb::BLACK, Rgb::WHITE]).unwrap();
        let average = Rgb {
            r: 128,
            g: 128,
            b: 128,
        };
        let palette = palette(&[], average, ModePref::Dark);
        let eff = effective_background(palette.background(), 0.35, average);
        let palette = palette.floor_against(eff, 4.5);
        let stats = profile.readability(&palette, 0.35, 4.5);
        assert_eq!(stats.samples, 2);
        assert!(stats.worst < 4.5);
        assert!(stats.coverage < 1.0);
        let opaque = profile.readability(&palette, 1.0, 4.5);
        assert_eq!(opaque.coverage, 1.0);
        assert_eq!(profile.texture, 1.0);
    }

    #[test]
    fn profile_preserves_shape_and_bounds_work() {
        let pixels = vec![Rgb::WHITE; 128 * 64];
        let profile = ImageProfile::from_grid(3840, 1920, 128, 64, &pixels);
        assert_eq!((profile.width, profile.height), (3840, 1920));
        assert_eq!(profile.colors.len(), PROFILE_EDGE * PROFILE_EDGE);
        assert_eq!(profile.texture, 0.0);
        assert!((profile.mean_luminance - 1.0).abs() < 1e-10);
        assert_eq!(profile.distance(&profile), 0.0);
        assert!(ImageProfile::new(1, 1, 17, 1, vec![Rgb::BLACK; 17]).is_none());
    }

    #[test]
    fn profile_matches_independent_region_rectangles() {
        for (w, h) in [
            (1, 1),
            (1, 17),
            (3, 7),
            (7, 3),
            (15, 16),
            (16, 15),
            (17, 31),
            (31, 17),
            (127, 128),
            (128, 127),
            (128, 128),
        ] {
            let pixels: Vec<_> = (0..w * h)
                .map(|i| Rgb {
                    r: ((i * 73 + i / w * 19) % 256) as u8,
                    g: ((i * 29 + i / w * 101) % 256) as u8,
                    b: ((i * 131 + i / w * 7) % 256) as u8,
                })
                .collect();
            let actual = ImageProfile::from_grid(w as u32, h as u32, w, h, &pixels);
            let (columns, rows) = (w.min(PROFILE_EDGE), h.min(PROFILE_EDGE));
            let mut expected = Vec::new();
            for row in 0..rows {
                for column in 0..columns {
                    let mut sum = [0u64; 3];
                    let mut count = 0;
                    for y in (row * h).div_ceil(rows)..((row + 1) * h).div_ceil(rows) {
                        for x in
                            (column * w).div_ceil(columns)..((column + 1) * w).div_ceil(columns)
                        {
                            let color = pixels[y * w + x];
                            for (channel, value) in sum.iter_mut().zip([color.r, color.g, color.b])
                            {
                                *channel += u64::from(value);
                            }
                            count += 1;
                        }
                    }
                    let [r, g, b] = sum.map(|channel| ((channel + count / 2) / count) as u8);
                    expected.push(Rgb { r, g, b });
                }
            }
            let reference = ImageProfile::new(w as u32, h as u32, columns, rows, expected).unwrap();
            assert_eq!(actual, reference, "profile {w}x{h}");
        }
    }
}

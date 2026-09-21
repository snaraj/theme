//! Image loading: one decode, one pass, one in-memory downsample.

use crate::{Error, ImageProfile, Rgb};
use std::path::Path;

/// Longest edge of the analysis grid. 128x128 = at most 16,384 samples for
/// k-means, which keeps derivation fast regardless of source resolution.
const GRID: u32 = 128;

/// Longest edge accepted from a decoder. Far beyond any wallpaper (8K is
/// 7680x4320) while capping the post-decode RGB buffer, which is allocated
/// outside the decoder's own `max_alloc` accounting.
const MAX_EDGE: u32 = 16_384;

pub(crate) struct Decoded {
    /// Block-mean downsample, row-major, at most GRID x GRID.
    pub pixels: Vec<Rgb>,
    /// Mean color of every full-resolution pixel (what `magick -resize 1x1`
    /// approximated in the shell floor).
    pub average: Rgb,
    pub profile: ImageProfile,
}

pub(crate) fn load(path: &Path) -> Result<Decoded, Error> {
    // The format is sniffed from the CONTENT — `with_guessed_format` reads
    // the magic bytes — matching the content-over-extension doctrine the
    // downloader enforces. The path's extension is only the fallback when
    // the magic is unrecognized. (`image::open` alone dispatches on the
    // extension; PR #8 review proved that regresses extensionless and
    // mislabeled files that decode fine under the shell CLI.)
    let mut reader = image::ImageReader::open(path)
        .and_then(image::ImageReader::with_guessed_format)
        .map_err(|e| Error::Decode(format!("{}: {e}", path.display())))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_EDGE);
    limits.max_image_height = Some(MAX_EDGE);
    reader.limits(limits);
    let img = reader
        .decode()
        .map_err(|e| Error::Decode(format!("{}: {e}", path.display())))?;
    let rgb = img.into_rgb8();
    let (w, h) = rgb.dimensions();
    if w == 0 || h == 0 {
        return Err(Error::Decode(format!("{}: empty image", path.display())));
    }

    let gw = w.min(GRID);
    let gh = h.min(GRID);
    let cells = (gw * gh) as usize;
    let mut pixels = Vec::with_capacity(cells);
    let mut total = [0u64; 3];
    let mut bounds = [[255u8; 3], [0u8; 3]];
    // Sum one rectangle at a time: each pixel contributes three additions,
    // without updating a cell count and the whole-image totals per pixel.
    // Ceil boundaries exactly invert floor(x * grid_width / image_width).
    let edges: Vec<usize> = (0..=gw)
        .map(|x| (x * w).div_ceil(gw) as usize * 3)
        .collect();
    let stride = w as usize * 3;
    for by in 0..gh {
        let top = (by * h).div_ceil(gh) as usize;
        let bottom = ((by + 1) * h).div_ceil(gh) as usize;
        for edge in edges.windows(2) {
            let (left, right) = (edge[0], edge[1]);
            let mut sum = [0u64; 3];
            let (mut low, mut high) = ([255u8; 3], [0u8; 3]);
            for y in top..bottom {
                for pixel in rgb.as_raw()[y * stride + left..y * stride + right]
                    .as_chunks::<3>()
                    .0
                {
                    sum[0] += u64::from(pixel[0]);
                    sum[1] += u64::from(pixel[1]);
                    sum[2] += u64::from(pixel[2]);
                    for channel in 0..3 {
                        low[channel] = low[channel].min(pixel[channel]);
                        high[channel] = high[channel].max(pixel[channel]);
                    }
                }
            }
            for (all, part) in total.iter_mut().zip(sum) {
                *all += part;
            }
            for channel in 0..3 {
                bounds[0][channel] = bounds[0][channel].min(low[channel]);
                bounds[1][channel] = bounds[1][channel].max(high[channel]);
            }
            let count = ((right - left) / 3 * (bottom - top)) as u64;
            let [r, g, b] = sum.map(|v| ((v + count / 2) / count) as u8);
            pixels.push(Rgb { r, g, b });
        }
    }

    let n = u64::from(w) * u64::from(h);
    let avg = |t: u64| ((t + n / 2) / n) as u8;
    Ok(Decoded {
        profile: ImageProfile::from_grid(w, h, gw as usize, gh as usize, &pixels)
            .with_bounds(bounds.map(|[r, g, b]| Rgb { r, g, b }))
            .unwrap(),
        pixels,
        average: Rgb {
            r: avg(total[0]),
            g: avg(total[1]),
            b: avg(total[2]),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn write_png(name: &str, w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> PathBuf {
        let mut buf = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                buf.extend_from_slice(&f(x, y));
            }
        }
        let path =
            std::env::temp_dir().join(format!("pigment-test-{}-{name}.png", std::process::id()));
        image::save_buffer(&path, &buf, w, h, image::ColorType::Rgb8).unwrap();
        path
    }

    #[test]
    fn solid_image_average_and_grid() {
        let p = write_png("solid", 300, 200, |_, _| [10, 200, 90]);
        let d = load(&p).unwrap();
        std::fs::remove_file(&p).ok();
        assert_eq!(
            d.average,
            Rgb {
                r: 10,
                g: 200,
                b: 90
            }
        );
        assert!(d.pixels.len() <= (GRID * GRID) as usize);
        assert!(d.pixels.iter().all(|&c| c
            == Rgb {
                r: 10,
                g: 200,
                b: 90
            }));
    }

    #[test]
    fn tiny_image_smaller_than_grid() {
        let p = write_png(
            "tiny",
            3,
            2,
            |x, _| if x == 0 { [255, 0, 0] } else { [0, 0, 255] },
        );
        let d = load(&p).unwrap();
        std::fs::remove_file(&p).ok();
        assert_eq!(d.pixels.len(), 6);
    }

    #[test]
    fn text_floor_covers_small_details_hidden_by_region_means() {
        let color = |x: u32, y: u32| match (x + y * 257) % 997 {
            0 => [255, 255, 255],
            1 => [0, 0, 0],
            2 => [255, 0, 190],
            _ => [18, 40, 65],
        };
        let path = write_png("sparse-highlights", 257, 193, color);
        let raw = crate::derive(&path, &crate::Options::default()).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(raw.profile.bounds, [Rgb::BLACK, Rgb::WHITE]);
        let cached = crate::Palette::from_cache_format(&raw.to_cache_format()).unwrap();
        for palette in [raw, cached] {
            for opacity in [0.6, 0.8, 0.9, 1.0] {
                for target in [4.5, 7.0] {
                    let (floored, achievable) = palette.clone().floor_for_image(opacity, target);
                    if !achievable {
                        assert!(opacity < 0.8, "unexpectedly lost a feasible palette");
                        continue;
                    }
                    // Independent original-pixel oracle, not the analysis grid
                    // or the persisted bounds used by the implementation.
                    for y in 0..193 {
                        for x in 0..257 {
                            let [r, g, b] = color(x, y);
                            let background = crate::effective_background(
                                floored.background(),
                                opacity,
                                Rgb { r, g, b },
                            );
                            for text in floored.colors[1..]
                                .iter()
                                .chain([&floored.foreground, &floored.cursor])
                            {
                                assert!(
                                    text.contrast(background) >= target,
                                    "hidden detail {x},{y}, opacity {opacity}, target {target}: {}",
                                    text.contrast(background)
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn downsample_matches_independent_cell_rectangle_means() {
        let color = |x: u32, y: u32| {
            [
                ((73 * x + 19 * y + 11 * x * y) % 256) as u8,
                ((29 * x + 101 * y + x * x) % 256) as u8,
                ((131 * x + 7 * y + y * y) % 256) as u8,
            ]
        };
        for (w, h) in [
            (1, 1),
            (1, 17),
            (7, 3),
            (15, 33),
            (16, 16),
            (17, 127),
            (127, 17),
            (128, 127),
            (129, 131),
            (257, 65),
            (65, 257),
            (511, 19),
        ] {
            let path = write_png(&format!("grid-reference-{w}-{h}"), w, h, color);
            let actual = load(&path).unwrap();
            std::fs::remove_file(path).unwrap();
            let (gw, gh) = (w.min(GRID), h.min(GRID));
            let mut expected = Vec::new();
            let mut total = [0u64; 3];
            // Invert grid membership into half-open source rectangles. This
            // independent reference never computes a cell from a pixel index.
            for row in 0..gh {
                for column in 0..gw {
                    let mut sum = [0u64; 3];
                    let mut count = 0;
                    for y in (row * h).div_ceil(gh)..((row + 1) * h).div_ceil(gh) {
                        for x in (column * w).div_ceil(gw)..((column + 1) * w).div_ceil(gw) {
                            for (channel, value) in sum.iter_mut().zip(color(x, y)) {
                                *channel += u64::from(value);
                            }
                            count += 1;
                        }
                    }
                    for (all, region) in total.iter_mut().zip(sum) {
                        *all += region;
                    }
                    let [r, g, b] = sum.map(|channel| ((channel + count / 2) / count) as u8);
                    expected.push(Rgb { r, g, b });
                }
            }
            let count = u64::from(w) * u64::from(h);
            let [r, g, b] = total.map(|channel| ((channel + count / 2) / count) as u8);
            assert_eq!(actual.pixels, expected, "grid {w}x{h}");
            assert_eq!(actual.average, Rgb { r, g, b }, "average {w}x{h}");
        }
    }

    #[test]
    fn decodes_extensionless_path_by_content() {
        let p = write_png("for-noext", 4, 4, |_, _| [1, 2, 3]);
        let noext = p.with_extension("");
        std::fs::rename(&p, &noext).unwrap();
        let d = load(&noext);
        std::fs::remove_file(&noext).ok();
        if let Err(e) = &d {
            panic!("extensionless valid PNG must decode by content: {e}");
        }
    }

    #[test]
    fn decodes_mislabeled_extension_by_content() {
        let p = write_png("for-mislabel", 4, 4, |_, _| [9, 8, 7]);
        let jpg = p.with_extension("jpg");
        std::fs::rename(&p, &jpg).unwrap();
        let d = load(&jpg);
        std::fs::remove_file(&jpg).ok();
        if let Err(e) = &d {
            panic!("PNG bytes at a .jpg path must decode by content: {e}");
        }
    }

    #[test]
    fn refuses_non_image_bytes() {
        let path = std::env::temp_dir().join(format!("pigment-test-{}.png", std::process::id()));
        std::fs::write(&path, b"not an image at all").unwrap();
        let err = load(&path);
        std::fs::remove_file(&path).ok();
        assert!(err.is_err());
    }
}

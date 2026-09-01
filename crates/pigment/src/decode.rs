//! Image loading: one decode, one pass, one in-memory downsample.

use crate::{Error, Rgb};
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
    let mut sums = vec![[0u64; 3]; cells];
    let mut counts = vec![0u64; cells];
    let mut total = [0u64; 3];

    for (y, row) in rgb.rows().enumerate() {
        let by = (y as u64 * u64::from(gh) / u64::from(h)) as usize;
        for (x, p) in row.enumerate() {
            let bx = (x as u64 * u64::from(gw) / u64::from(w)) as usize;
            let cell = by * gw as usize + bx;
            let s = &mut sums[cell];
            s[0] += u64::from(p[0]);
            s[1] += u64::from(p[1]);
            s[2] += u64::from(p[2]);
            counts[cell] += 1;
            total[0] += u64::from(p[0]);
            total[1] += u64::from(p[1]);
            total[2] += u64::from(p[2]);
        }
    }

    let n = u64::from(w) * u64::from(h);
    let avg = |t: u64| ((t + n / 2) / n) as u8;
    let pixels = sums
        .iter()
        .zip(&counts)
        .filter(|&(_, &c)| c > 0)
        .map(|(s, &c)| Rgb {
            r: ((s[0] + c / 2) / c) as u8,
            g: ((s[1] + c / 2) / c) as u8,
            b: ((s[2] + c / 2) / c) as u8,
        })
        .collect();

    Ok(Decoded {
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

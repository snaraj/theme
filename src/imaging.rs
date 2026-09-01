//! In-process image transforms and measurements (the shell shelled out to
//! sips/ImageMagick for these). Rotation and canvas extension re-encode in
//! the source format where the codec can encode; webp encodes are not
//! supported by the decoder set, so a transformed webp re-encodes as PNG —
//! the save step names the file by its actual content type either way.

use crate::ui::die;
use image::{DynamicImage, GenericImageView, ImageFormat, Rgba, RgbaImage};
use std::path::Path;
use std::process::Command;

/// "3840x2160", or empty when the dimensions cannot be read. Shape-asserted:
/// digits, an x, digits — or nothing at all.
pub fn img_size(path: &Path) -> String {
    match image::image_dimensions(path) {
        Ok((w, h)) => format!("{w}x{h}"),
        Err(_) => String::new(),
    }
}

pub fn width_of(path: &Path) -> Option<u32> {
    image::image_dimensions(path).ok().map(|(w, _)| w)
}

fn load(path: &Path) -> Option<(DynamicImage, Option<ImageFormat>)> {
    let reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let fmt = reader.format();
    Some((reader.decode().ok()?, fmt))
}

fn store(img: &DynamicImage, path: &Path, fmt: Option<ImageFormat>) -> bool {
    let fmt = match fmt {
        Some(ImageFormat::WebP) | None => ImageFormat::Png,
        Some(f) => f,
    };
    // JPEG cannot carry an alpha channel; flatten before encoding.
    let out = if fmt == ImageFormat::Jpeg {
        DynamicImage::ImageRgb8(img.to_rgb8())
    } else {
        img.clone()
    };
    out.save_with_format(path, fmt).is_ok()
}

/// Rotate the file in place 90° (`right`) or 270° (`left`).
pub fn rotate_image(path: &Path, dir: &str) {
    let Some((img, fmt)) = load(path) else {
        die(&format!("rotation failed on {}", path.display()));
    };
    let rotated = match dir {
        "right" => img.rotate90(),
        "left" => img.rotate270(),
        _ => die("--rotate takes left or right"),
    };
    if !store(&rotated, path, fmt) {
        die(&format!("rotation failed on {}", path.display()));
    }
}

/// The primary display's aspect ratio (width/height), or 1.6 when it cannot
/// be read. Finder's desktop bounds are logical points; the RATIO matches
/// the pixels.
fn screen_aspect() -> f64 {
    let out = Command::new("osascript")
        .args([
            "-e",
            "tell application \"Finder\" to get bounds of window of desktop",
        ])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let parts: Vec<f64> = out
        .trim()
        .split(", ")
        .filter_map(|p| p.trim().parse().ok())
        .collect();
    match parts.as_slice() {
        [_, _, w, h] if *h > 0.0 => w / h,
        _ => 1.6,
    }
}

/// Extend the canvas to the screen's aspect ratio, design centred, padding
/// in a solid color — for art on a flat background: no crop, no zoom.
pub fn extend_image(path: &Path, hex: &str) {
    let Some((r, g, b)) = crate::ui::parse_hex6(hex) else {
        die("--extend takes a 6-digit hex color (default 000000)");
    };
    let Some((img, fmt)) = load(path) else {
        die(&format!("cannot read image size of {}", path.display()));
    };
    let (w, h) = img.dimensions();
    let aspect = screen_aspect();
    let (tw, th) = {
        let tw = (f64::from(h) * aspect) as u32;
        if tw >= w {
            (tw, h)
        } else {
            (w, (f64::from(w) / aspect) as u32)
        }
    };
    let mut canvas = RgbaImage::from_pixel(tw.max(w), th.max(h), Rgba([r, g, b, 255]));
    let ox = (canvas.width() - w) / 2;
    let oy = (canvas.height() - h) / 2;
    image::imageops::overlay(&mut canvas, &img.to_rgba8(), i64::from(ox), i64::from(oy));
    if !store(&DynamicImage::ImageRgba8(canvas), path, fmt) {
        die(&format!("canvas extension failed on {}", path.display()));
    }
}

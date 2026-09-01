//! Per-stage and end-to-end benches on a synthetic 4K wallpaper.
//! Run: `cargo bench -p pigment`.

use criterion::{Criterion, criterion_group, criterion_main};
use pigment::{Options, cached_derive, derive, effective_background};
use std::path::PathBuf;

/// A 3840x2160 gradient-plus-blobs image: non-trivial for k-means, cheap to
/// generate, and deterministic.
fn synthetic_4k() -> PathBuf {
    let path = std::env::temp_dir().join("pigment-bench-4k.png");
    if !path.exists() {
        let (w, h) = (3840u32, 2160u32);
        let mut buf = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                let r = (x * 255 / w) as u8;
                let g = (y * 255 / h) as u8;
                let b = if (x / 400 + y / 400) % 2 == 0 {
                    200
                } else {
                    40
                };
                buf.extend_from_slice(&[r, g, b]);
            }
        }
        image::save_buffer(&path, &buf, w, h, image::ColorType::Rgb8).unwrap();
    }
    path
}

fn benches(c: &mut Criterion) {
    let img = synthetic_4k();
    let opts = Options::default();
    let cache = std::env::temp_dir().join("pigment-bench-cache");

    c.bench_function("derive_uncached_4k", |b| {
        b.iter(|| derive(&img, &opts).unwrap())
    });

    cached_derive(&img, &opts, &cache).unwrap();
    c.bench_function("derive_cached_4k", |b| {
        b.iter(|| cached_derive(&img, &opts, &cache).unwrap())
    });

    let p = derive(&img, &opts).unwrap();
    let eff = effective_background(p.background(), 0.65, p.wallpaper_average);
    c.bench_function("floor_16_colors", |b| {
        b.iter(|| p.clone().floor_against(eff, 4.5))
    });

    let f = p.clone().floor_against(eff, 4.5);
    c.bench_function("emit_all_formats", |b| {
        b.iter(|| {
            (
                f.to_kitty(),
                f.to_alacritty(),
                f.to_osc(),
                p.to_cache_format(),
            )
        })
    });
}

criterion_group!(stages, benches);
criterion_main!(stages);

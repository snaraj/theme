//! End-to-end CLI-shaped harness for hyperfine comparisons:
//! derive + floor + emit kitty conf to stdout.
//!
//! `cargo run --release -p pigment --example derive -- <image> [opacity] [floor]`

use pigment::{Options, derive, effective_background};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: derive <image> [opacity] [floor]");
        std::process::exit(2);
    };
    let opacity: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let floor: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(4.5);

    let palette = match derive(std::path::Path::new(&path), &Options::default()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("derive: {e}");
            std::process::exit(1);
        }
    };
    let eff = effective_background(palette.background(), opacity, palette.wallpaper_average);
    print!("{}", palette.floor_against(eff, floor).to_kitty());
}

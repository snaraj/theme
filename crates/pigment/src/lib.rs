//! Pigment: the in-house wallpaper-to-palette color engine for `theme`.
//!
//! One decode, one in-memory downsample, deterministic k-means++, a 16-slot
//! ANSI derivation with hue-archetype mapping, and the blend-aware contrast
//! floor ported 1:1 from `theme.sh`. The crate emits strings and files only —
//! it never touches a terminal, a socket, or the desktop.
//!
//! Determinism is a feature: the same image bytes and [`Options`] always
//! produce the same [`Palette`], so gold-file tests are sound and caches are
//! stable across machines.

// unsafe_code is denied by the workspace lint table (root Cargo.toml) — the
// single, gate-enforced source of truth per AGENTS.md.
#![warn(missing_docs)]

mod cache;
mod color;
mod decode;
mod derive;
mod emit;
mod extract;
mod floor;

pub use cache::{cache_key, cached_derive};
pub use color::Rgb;
pub use floor::{Floored, effective_background};

use std::path::Path;

/// Light/dark decision for a derived palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Dark background, light text.
    Dark,
    /// Light background, dark text.
    Light,
}

/// Caller preference for [`Mode`]; `Auto` decides from the image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModePref {
    /// Decide from the image's weighted luminance.
    Auto,
    /// Force a dark palette.
    Dark,
    /// Force a light palette.
    Light,
}

/// Derivation parameters. `Default` is the supported configuration.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Light/dark preference.
    pub mode: ModePref,
    /// k-means cluster count (clamped to the number of distinct colors).
    pub clusters: usize,
    /// PRNG seed for k-means++ initialisation. Fixed by default so palettes
    /// are reproducible; change it only to explore alternatives.
    pub seed: u64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            mode: ModePref::Auto,
            clusters: 10,
            seed: 0x5EED_1E57,
        }
    }
}

/// A derived 16-color terminal palette plus the metadata the contrast floor
/// and the cache need.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette {
    /// ANSI slots 0-15. Slot 0 is the background.
    pub colors: [Rgb; 16],
    /// Default text color.
    pub foreground: Rgb,
    /// Cursor color.
    pub cursor: Rgb,
    /// Mean color of the full-resolution image (the floor blends against it).
    pub wallpaper_average: Rgb,
    /// The light/dark decision that shaped the palette.
    pub mode: Mode,
}

impl Palette {
    /// The background color (ANSI slot 0).
    pub fn background(&self) -> Rgb {
        self.colors[0]
    }
}

/// Errors a derivation can produce.
#[derive(Debug)]
pub enum Error {
    /// The image could not be read or decoded.
    Decode(String),
    /// A cache file could not be read, written, or parsed.
    Cache(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Decode(m) => write!(f, "decode: {m}"),
            Error::Cache(m) => write!(f, "cache: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// Derive a palette from the image at `path`.
///
/// Grayscale and near-monochrome art is first-class: it produces a usable
/// ramp palette rather than an error (the shell CLI's colorz backend refused
/// such images; pigment must not).
pub fn derive(path: &Path, opts: &Options) -> Result<Palette, Error> {
    let img = decode::load(path)?;
    let clusters = extract::kmeans(&img.pixels, opts.clusters, opts.seed);
    Ok(derive::palette(&clusters, img.average, opts.mode))
}

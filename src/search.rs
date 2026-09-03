//! `theme search` — one ranked answer over every fact the library already
//! holds: the title on disk, the folder it sits in, its shape, its
//! provenance xattrs, and the colors its cached scheme derived. Matching is
//! in-house (exact beats word start beats substring beats a scattered
//! subsequence), EVERY term must land somewhere, and the table obeys the
//! same width discipline as `list` — three columns while they fit, stacked
//! below that, never torn.

use crate::config::Config;
use crate::imaging::img_size;
use crate::library::all_images;
use crate::report::{
    added_date, backfill_schemes, birth_key, human_bytes, swatch_cells, wall_meta, wall_scheme,
    wall_source,
};
use crate::ui::{display_text, parse_hex6, term_cols, truncate_ellipsis, wrap_prefixed};
use std::path::Path;

/// The `theme.*` provenance xattrs `preview` reads, as fields a term can hit.
const META: [&str; 5] = ["artist", "published", "camera", "place", "license"];
/// Hue wedges: each upper bound in degrees and the word below it. Red owns
/// both ends of the circle, so anything past the last bound wraps back to it.
const HUE_MAX: [f64; 8] = [15.0, 45.0, 70.0, 170.0, 200.0, 260.0, 290.0, 345.0];
const HUE_WORD: [&str; 8] = [
    "red", "orange", "yellow", "green", "cyan", "blue", "purple", "pink",
];
/// Width tiers, largest first: the word people search a resolution by.
const TIER_PX: [u32; 5] = [7680, 5120, 3840, 2560, 1920];
const TIER_WORD: [&str; 5] = ["8k", "5k", "4k", "1440p", "1080p"];
/// Column floors; below their sum the three columns cannot fit and rows stack.
const TITLEW: usize = 16;
const SWATCHW: usize = 24;
const MATCHW: usize = 12;

/// The sanitized title — matched and printed as this copy, while the path we
/// open stays byte-exact.
fn title(path: &Path) -> String {
    display_text(path.file_stem().and_then(|s| s.to_str()).unwrap_or(""))
}

/// Where the file sits inside its library root ("unsplash", "gifs/anime").
/// The root itself has no folder to name, and answers empty.
fn folder(cfg: &Config, path: &Path) -> String {
    cfg.wallpaper_dirs
        .iter()
        .find_map(|d| path.strip_prefix(d).ok())
        .and_then(Path::parent)
        .map(|p| display_text(&p.to_string_lossy()))
        .unwrap_or_default()
}

/// "landscape 4k" — the words people actually search a resolution by, rather
/// than the digits they would have to remember.
fn shape_words(size: &str) -> String {
    let Some((Ok(w), Ok(h))) = size
        .split_once('x')
        .map(|(a, b)| (a.parse::<u32>(), b.parse::<u32>()))
    else {
        return String::new();
    };
    let orient = match w.cmp(&h) {
        std::cmp::Ordering::Greater => "landscape",
        std::cmp::Ordering::Less => "portrait",
        std::cmp::Ordering::Equal => "square",
    };
    let tier = TIER_PX
        .iter()
        .position(|px| w >= *px)
        .map_or("", |i| TIER_WORD[i]);
    format!("{orient} {tier}").trim_end().to_string()
}

/// A hex color as the word someone would type. HSL buckets, because hue is
/// how people name a color and lightness is what separates brown from orange
/// and black from everything.
fn color_word(hex: &str) -> Option<&'static str> {
    let (r, g, b) = parse_hex6(hex.trim_start_matches('#'))?;
    let n = |c: u8| f64::from(c) / 255.0;
    let (r, g, b) = (n(r), n(g), n(b));
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let (l, d) = ((max + min) / 2.0, max - min);
    if l < 0.12 {
        return Some("black");
    }
    if l > 0.9 {
        return Some("white");
    }
    if d == 0.0 || d / (1.0 - (2.0 * l - 1.0).abs()) < 0.12 {
        return Some("gray");
    }
    let h = 60.0
        * if max == r {
            ((g - b) / d).rem_euclid(6.0)
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
    let w = HUE_WORD[HUE_MAX.iter().position(|hi| h < *hi).unwrap_or(0)];
    // A dark orange or yellow is what everyone calls brown.
    let brown = l < 0.4 && matches!(w, "orange" | "yellow");
    Some(if brown { "brown" } else { w })
}

/// Everything one wallpaper can be searched by. An empty value is dropped
/// rather than stored, so a fact the file does not have can never match.
fn facts(cfg: &Config, path: &Path, scheme: &[String]) -> Vec<(&'static str, String)> {
    let size = img_size(path);
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mut words: Vec<&str> = Vec::new();
    for c in scheme {
        if let Some(w) = color_word(c)
            && !words.contains(&w)
        {
            words.push(w);
        }
    }
    // Color 0 is the background; its luminance is what "dark" and "light"
    // mean to anyone picking a theme.
    let mode = scheme
        .first()
        .and_then(|c| pigment::Rgb::parse(c))
        .map_or("", |c| if c.luminance() < 0.5 { "dark" } else { "light" });
    let mut f: Vec<(&'static str, String)> = vec![
        ("title", title(path)),
        ("folder", folder(cfg, path)),
        ("format", ext.to_lowercase()),
        ("shape", shape_words(&size)),
        ("size", size),
        ("bytes", human_bytes(path)),
        ("added", added_date(path)),
        ("colors", words.join(" ")),
        ("mode", mode.to_string()),
    ];
    let src = wall_source(path);
    if src != "-" {
        f.push(("source", src));
    }
    for field in META {
        f.push((field, wall_meta(path, &format!("theme.{field}"))));
    }
    f.retain(|(_, v)| !v.is_empty());
    f
}

/// One term against one value, both already lowercased. The ladder is fzf's
/// idea without fzf's crate: an exact answer beats a word start beats a
/// substring beats a scattered subsequence, and a subsequence pays one point
/// for every character it had to skip.
fn score(term: &[char], value: &[char]) -> u32 {
    if term.is_empty() || term.len() > value.len() {
        return 0;
    }
    if term == value {
        return 100;
    }
    let mut sub = 0;
    for i in 0..=value.len() - term.len() {
        if value[i..i + term.len()] == *term {
            if i == 0 || !value[i - 1].is_alphanumeric() {
                return 80;
            }
            sub = 60;
        }
    }
    if sub > 0 {
        return sub;
    }
    let (mut ti, mut gaps, mut last) = (0usize, 0usize, None);
    for (i, c) in value.iter().enumerate() {
        if term.get(ti) == Some(c) {
            if let Some(l) = last {
                gaps += i - l - 1;
            }
            (last, ti) = (Some(i), ti + 1);
        }
    }
    if ti == term.len() {
        30usize.saturating_sub(gaps).max(10) as u32
    } else {
        0
    }
}

/// A search term: its lowercased characters, plus the color it IS when the
/// user typed one — a hex term is a question about the scheme, not the text.
struct Term {
    chars: Vec<char>,
    hex: Option<(u8, u8, u8)>,
}

fn term(raw: &str) -> Term {
    let low = display_text(raw).to_lowercase();
    Term {
        hex: parse_hex6(low.trim_start_matches('#')),
        chars: low.chars().collect(),
    }
}

/// Two colors within this Euclidean RGB distance still read as the same one.
fn near(hex: &str, (r, g, b): (u8, u8, u8)) -> bool {
    let Some((cr, cg, cb)) = parse_hex6(hex.trim_start_matches('#')) else {
        return false;
    };
    let d = |a: u8, b: u8| f64::from(i32::from(a) - i32::from(b)).powi(2);
    (d(cr, r) + d(cg, g) + d(cb, b)).sqrt() <= 60.0
}

/// A wallpaper's total and the facts that earned it. EVERY term must land
/// somewhere (AND semantics): one term nobody answers eliminates the file.
fn rank(facts: &[(&str, String)], scheme: &[String], terms: &[Term]) -> Option<(u32, String)> {
    let low: Vec<Vec<char>> = facts
        .iter()
        .map(|(_, v)| v.to_lowercase().chars().collect())
        .collect();
    let (mut total, mut hits): (u32, Vec<String>) = (0, Vec::new());
    for t in terms {
        let (mut best, mut hit) = (0, String::new());
        for (i, (field, value)) in facts.iter().enumerate() {
            let s = score(&t.chars, &low[i]);
            if s > best {
                (best, hit) = (s, format!("{field}: {}", truncate_ellipsis(value, 40)));
            }
        }
        if let Some((r, g, b)) = t.hex
            && best < 60
            && scheme.iter().any(|c| near(c, (r, g, b)))
        {
            (best, hit) = (60, format!("colors: #{r:02x}{g:02x}{b:02x}"));
        }
        if best == 0 {
            return None;
        }
        total += best;
        if !hits.contains(&hit) {
            hits.push(hit);
        }
    }
    Some((total, hits.join(", ")))
}

/// One matching wallpaper, reduced to what the table prints and the keys it
/// sorts by.
struct Hit {
    score: u32,
    added: i64,
    title: String,
    scheme: Vec<String>,
    matched: String,
}

#[allow(clippy::print_literal)] // column headers: the formatter pads them, hand-counted spaces would rot
pub fn cmd_search(cfg: &Config, args: &[String], list_n: usize) {
    let terms: Vec<Term> = args.iter().map(|a| term(a)).collect();
    let files = all_images(cfg);
    let total = files.len();
    // A wallpaper with no cached scheme can answer no color question, so the
    // candidate set is indexed once first — exactly as `list` does.
    backfill_schemes(cfg, files.iter());
    let mut hits: Vec<Hit> = Vec::new();
    for f in &files {
        let scheme = wall_scheme(cfg, f).unwrap_or_default();
        if let Some((score, matched)) = rank(&facts(cfg, f, &scheme), &scheme, &terms) {
            hits.push(Hit {
                score,
                added: birth_key(f),
                title: title(f),
                scheme,
                matched,
            });
        }
    }
    // Best answer first; equally good answers newest first, like `list`.
    hits.sort_by_key(|h| (std::cmp::Reverse(h.score), std::cmp::Reverse(h.added)));

    let cols = term_cols();
    let query = display_text(&args.join(" "));
    // The query, the footer and the no-match line are prose, and wrap like
    // every other value this tool prints — never at column 0 (issue #19).
    let say = |text: &str, first: &str| {
        for l in wrap_prefixed(text, cols, first, "  ") {
            println!("{l}");
        }
    };
    say(&query, "search: ");
    println!();
    if hits.is_empty() {
        // Nothing matching is an ANSWER, not a failure: exit 0.
        say(&format!("no wallpaper matches \"{query}\""), "  ");
        return;
    }
    let wide = cols >= 2 + TITLEW + 2 + SWATCHW + 2 + MATCHW;
    let namew = cols
        .saturating_sub(2 + 2 + SWATCHW + 2 + MATCHW)
        .clamp(TITLEW, 44);
    // Too narrow for three columns: the swatch row is count-fitted to the
    // width rather than cut, and every part of the row stacks (issue #19).
    let swn = if wide {
        8
    } else {
        (cols.saturating_sub(4) / 3).clamp(1, 8)
    };
    if wide {
        println!(
            "  {:<namew$}  {:<SWATCHW$}  {}",
            "TITLE", "COLORSCHEME", "MATCHED"
        );
    }
    let shown = if list_n > 0 { list_n } else { hits.len() };
    for h in hits.iter().take(shown) {
        let (sw, n) = swatch_cells(&h.scheme[..swn.min(h.scheme.len())]);
        let w = swn * 3;
        let cells = if n == 0 {
            format!("{:<w$}", "-")
        } else {
            format!("{sw}{}", "   ".repeat(swn.saturating_sub(n)))
        };
        if wide {
            // The matched text wraps into its OWN column, so its hanging
            // indent is measured on a plain-text copy: the swatch escapes
            // ahead of it are bytes, not columns.
            let pad = " ".repeat(2 + namew + 2 + SWATCHW + 2);
            let lines = wrap_prefixed(&h.matched, cols, &pad, &pad);
            println!(
                "  {:<namew$}  {cells}  {}",
                truncate_ellipsis(&h.title, namew),
                lines[0].get(pad.len()..).unwrap_or("")
            );
            for l in &lines[1..] {
                println!("{l}");
            }
        } else {
            println!("  {}", truncate_ellipsis(&h.title, cols.saturating_sub(2)));
            println!("    {}", cells.trim_end());
            for l in wrap_prefixed(&h.matched, cols, "    ", "    ") {
                println!("{l}");
            }
        }
    }
    println!();
    // A capped table says so, the way `list` does: matches found is not rows shown.
    let (n, rows) = (hits.len(), shown.min(hits.len()));
    let footer = if rows < n {
        format!("{rows} of {n} matches shown — more: theme search … -n <count>, or --all")
    } else {
        format!("{n} of {total} wallpapers match")
    };
    say(&footer, "  ");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sc(t: &str, v: &str) -> u32 {
        let (t, v): (Vec<char>, Vec<char>) = (t.chars().collect(), v.chars().collect());
        score(&t, &v)
    }

    /// The ladder, rung by rung — the gap penalty, its floor, and a term
    /// longer than the value, which can be nothing at all.
    #[test]
    fn the_score_ladder_orders_the_way_a_reader_expects() {
        assert_eq!(sc("neon", "neon"), 100);
        assert_eq!(sc("city", "neon-city-rain"), 80); // after a non-alphanumeric
        assert_eq!(sc("eon", "neon-city"), 60); // mid-word
        assert_eq!(sc("nct", "neon-city"), 25); // subsequence, 5 gaps
        assert_eq!(sc("az", &format!("a{}z", "-".repeat(30))), 10); // the floor holds
        assert_eq!(sc("zz", "neon"), 0);
        assert_eq!(sc("neon-city", "neon"), 0);
        // Offsets count CHARACTERS: on bytes the word-start test would land
        // inside the é and score this 60.
        assert_eq!(sc("noir", "café-noir"), 80);
    }

    /// The derived vocabulary: hue and lightness for a color, orientation and
    /// tier for a size.
    #[test]
    fn derived_words_are_the_ones_people_type() {
        assert_eq!(color_word("000000"), Some("black"));
        assert_eq!(color_word("f8f8f8"), Some("white"));
        assert_eq!(color_word("7a7a7a"), Some("gray"));
        assert_eq!(color_word("c81e1e"), Some("red"));
        assert_eq!(color_word("1e3cc8"), Some("blue"));
        // Same hue, different lightness: orange in the light, brown in the dark.
        assert_eq!(color_word("ff9933"), Some("orange"));
        assert_eq!(color_word("5a3300"), Some("brown"));
        assert_eq!(color_word("nothex"), None);
        assert_eq!(shape_words("3840x2160"), "landscape 4k");
        assert_eq!(shape_words("1080x1920"), "portrait");
        assert_eq!(shape_words("2560x2560"), "square 1440p");
        assert_eq!(shape_words(""), "");
    }

    /// AND semantics and the hex-by-distance arm, over one fixed fact set.
    #[test]
    fn every_term_must_land_and_a_hex_asks_the_scheme() {
        let facts = [
            ("title", "neon-city-rain".to_string()),
            ("colors", "blue black".to_string()),
        ];
        let scheme = vec!["1e3cc8".to_string()];
        let terms = |s: &str| -> Vec<Term> { s.split(' ').map(term).collect() };
        let (total, matched) = rank(&facts, &scheme, &terms("neon blue")).unwrap();
        assert_eq!(total, 160); // two word-start hits
        assert_eq!(matched, "title: neon-city-rain, colors: blue black");
        // One term nobody answers eliminates the wallpaper outright.
        assert!(rank(&facts, &scheme, &terms("neon qqqq")).is_none());
        // A hex within 60 of a scheme color scores like a substring and is
        // shown as the color it asked for, not as the scheme's.
        let (total, matched) = rank(&facts, &scheme, &terms("#1e3ec9")).unwrap();
        assert_eq!((total, matched.as_str()), (60, "colors: #1e3ec9"));
        assert!(rank(&facts, &scheme, &terms("#c81e1e")).is_none());
    }
}

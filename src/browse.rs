//! A line-oriented wallpaper browser. Navigation only previews; the exact
//! typed command `apply` is the sole desktop transition. IDs belong to one
//! immutable library snapshot, so names never become shell words.

use crate::browse_state::State;
use crate::config::Config;
use crate::presentation::{self, PreparedPalette};
use crate::ui::{
    cell_chunks, die, display_text, pad_cells, term_cols, truncate_ellipsis, wrap_prefixed,
};
use crate::{apply, report, search};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const HELP: &str = "theme browse [terms...] [options]   (alias: theme surf)

Browse a contact sheet, then type select <ID> for a larger preview.
Only the exact command apply changes the wallpaper and terminal colors.
Existing terminal and shell keybindings are unchanged.

  --all                  start with the entire library, not the saved query
  --favorites            start with saved favorites
  --page-size N          pictures per page (1..24; default 6)
  --color NAME|#RRGGBB    filter the emitted palette, not filename words
  --min-contrast RATIO   minimum sampled text/background contrast (1..21)
  --coverage FRACTION    sampled coverage at THEME_CONTRAST (0..1)
  --min-width PIXELS     minimum original image width
  --min-height PIXELS    minimum original image height
  --aspect W:H           aspect ratio within 10 percent

Commands (type a line, then Return):
  select ID   next   prev   page N   list   shuffle
  favorite [ID]   favorites   all   history
  query TERMS   calmer   similar   different
  apply   help   quit

next/prev follow the current queue; shuffle visits each result once.
page N shows a sheet without selecting or applying anything. IDs stay
stable during the session. Query and local favorites/history are saved.
query with no terms clears the query. all leaves the favorites filter.
calmer ranks measured image texture; similar/different compare the selected
image's mean Oklab color. Readability is sampled, not an every-pixel guarantee.
EOF exits. Non-interactive input/output prints one deterministic page and
never reads commands or applies a wallpaper. THEME_NO_APPLY also skips state
writes. Escape cancels nothing globally; type quit to leave this browser.";

fn prose_lines(text: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(1);
    text.lines()
        .flat_map(|line| {
            let line = display_text(line);
            if cols >= 14 {
                wrap_prefixed(&line, cols, "", "  ")
            } else if line.is_empty() {
                vec![String::new()]
            } else {
                cell_chunks(&line, cols)
            }
        })
        .collect()
}

fn say(text: &str) {
    for line in prose_lines(text, term_cols()) {
        println!("{line}");
    }
}

fn note(text: &str) {
    say(&format!("theme: {}", display_text(text)));
}

pub fn usage() {
    say(HELP);
}

type FileIdentity = (u64, u64, u64, i64, i64, i64, i64);

#[derive(Default)]
struct Options {
    terms: Vec<String>,
    reset: bool,
    favorites: bool,
    page_size: usize,
    color: Option<String>,
    contrast: Option<f64>,
    coverage: Option<f64>,
    width: u32,
    height: u32,
    aspect: Option<f64>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Option<Self>, String> {
        let mut o = Self {
            page_size: 6,
            ..Self::default()
        };
        let mut help = false;
        let mut rest = false;
        let mut args = args.iter();
        while let Some(a) = args.next() {
            if a.len() > 2048 || a.chars().any(char::is_control) {
                return Err("browser arguments must be short printable text".into());
            }
            if rest {
                o.terms.push(a.clone());
                continue;
            }
            match a.as_str() {
                "--" => rest = true,
                "--help" | "-h" => help = true,
                "--all" => o.reset = true,
                "--favorites" => o.favorites = true,
                "--page-size" | "--color" | "--min-contrast" | "--coverage" | "--min-width"
                | "--min-height" | "--aspect" => {
                    let v = args.next().ok_or_else(|| format!("{a} needs a value"))?;
                    match a.as_str() {
                        "--page-size" => o.page_size = number(v, 1.0, 24.0)? as usize,
                        "--color" => {
                            if pigment::Rgb::parse(v).is_none() && !COLORS.contains(&v.as_str()) {
                                return Err("--color needs #RRGGBB or red/orange/yellow/green/cyan/blue/purple/pink/black/white/gray/brown".into());
                            }
                            o.color = Some(v.clone());
                        }
                        "--min-contrast" => o.contrast = Some(number(v, 1.0, 21.0)?),
                        "--coverage" => o.coverage = Some(number(v, 0.0, 1.0)?),
                        "--min-width" => o.width = pixels(v)?,
                        "--min-height" => o.height = pixels(v)?,
                        "--aspect" => {
                            let (w, h) = v
                                .split_once(':')
                                .ok_or("--aspect needs W:H, for example 16:9")?;
                            o.aspect = Some(number(w, 0.01, 10000.0)? / number(h, 0.01, 10000.0)?);
                        }
                        _ => unreachable!(),
                    }
                    if a == "--page-size" && v.parse::<usize>().is_err() {
                        return Err("--page-size needs a whole number".into());
                    }
                }
                s if s.starts_with('-') => return Err(format!("unknown browse option {s}")),
                _ => o.terms.push(a.clone()),
            }
        }
        if o.terms.len() > 32 {
            return Err("use at most 32 search terms".into());
        }
        Ok((!help).then_some(o))
    }

    fn measured(&self) -> bool {
        self.color.is_some()
            || self.contrast.is_some()
            || self.coverage.is_some()
            || self.width > 0
            || self.height > 0
            || self.aspect.is_some()
    }

    fn matches(&self, p: &PreparedPalette) -> bool {
        p.profile.width >= self.width
            && p.profile.height >= self.height
            && self.aspect.is_none_or(|r| {
                p.profile.height > 0
                    && ((p.profile.width as f64 / p.profile.height as f64) / r - 1.0).abs() <= 0.1
            })
            && self.contrast.is_none_or(|v| p.readability.worst >= v)
            && self.coverage.is_none_or(|v| p.readability.coverage >= v)
            && self
                .color
                .as_ref()
                .is_none_or(|c| p.palette.colors.iter().any(|rgb| color_matches(c, *rgb)))
    }
}

fn number(s: &str, min: f64, max: f64) -> Result<f64, String> {
    s.parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= min && *v <= max)
        .ok_or_else(|| format!("expected a number between {min} and {max}"))
}

fn pixels(s: &str) -> Result<u32, String> {
    s.parse::<u32>()
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(|| "image dimensions need a positive integer".into())
}

const COLORS: [&str; 12] = [
    "red", "orange", "yellow", "green", "cyan", "blue", "purple", "pink", "black", "white", "gray",
    "brown",
];

fn color_matches(want: &str, rgb: pigment::Rgb) -> bool {
    if let Some(want) = pigment::Rgb::parse(want) {
        let a = crate::ui::parse_hex6(rgb.hex().trim_start_matches('#')).unwrap();
        let b = crate::ui::parse_hex6(want.hex().trim_start_matches('#')).unwrap();
        let d = |a: u8, b: u8| (f64::from(a) - f64::from(b)).powi(2);
        return d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2) <= 3600.0;
    }
    search::color_word(&rgb.hex()) == Some(want)
}

/// A permutation, rather than independent random picks: moving backward then
/// forward is reversible, and reaching the end never silently starts repeats.
#[derive(Default)]
struct Queue {
    ids: Vec<usize>,
    cursor: Option<usize>,
}

impl Queue {
    fn replace(&mut self, ids: Vec<usize>) {
        self.ids = ids;
        self.cursor = None;
    }
    fn selected(&self) -> Option<usize> {
        self.cursor.and_then(|i| self.ids.get(i)).copied()
    }
    fn select(&mut self, id: usize) -> bool {
        let Some(i) = self.ids.iter().position(|v| *v == id) else {
            return false;
        };
        self.cursor = Some(i);
        true
    }
    fn step(&mut self, forward: bool) -> bool {
        let next = match (self.cursor, forward) {
            (None, _) => 0,
            (Some(i), true) => i + 1,
            (Some(i), false) => match i.checked_sub(1) {
                Some(i) => i,
                None => return false,
            },
        };
        if next >= self.ids.len() {
            return false;
        }
        self.cursor = Some(next);
        true
    }
    fn shuffle(&mut self, mut seed: u64) {
        for i in (1..self.ids.len()).rev() {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            self.ids.swap(i, ((seed >> 32) as usize) % (i + 1));
        }
        self.cursor = None;
    }
}

struct Browser<'a> {
    cfg: &'a Config,
    options: Options,
    state: State,
    paths: Vec<PathBuf>,
    measured: BTreeMap<usize, Result<PreparedPalette, String>>,
    identities: BTreeMap<usize, FileIdentity>,
    queue: Queue,
    page: usize,
    graphics: bool,
}

fn identity(path: &std::path::Path) -> Option<FileIdentity> {
    path.symlink_metadata()
        .ok()
        .filter(|m| m.file_type().is_file())
        .map(|m| {
            (
                m.dev(),
                m.ino(),
                m.len(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec(),
            )
        })
}

impl Browser<'_> {
    fn measure(&mut self, id: usize) {
        let identity = identity(&self.paths[id]);
        if identity.is_none() {
            self.measured
                .insert(id, Err("image is no longer a regular library file".into()));
            return;
        }
        let settings_match = self.measured.get(&id).is_some_and(|p| {
            p.as_ref().is_ok_and(|p| {
                presentation::opacity(self.cfg).is_ok_and(|opacity| opacity == p.opacity)
            })
        });
        if identity.as_ref() != self.identities.get(&id) || !settings_match {
            self.measured
                .insert(id, presentation::cached_preview(self.cfg, &self.paths[id]));
            if let Some(identity) = identity {
                self.identities.insert(id, identity);
            }
        }
    }

    fn refresh(&mut self) {
        let positions: BTreeMap<&PathBuf, usize> =
            self.paths.iter().enumerate().map(|(i, p)| (p, i)).collect();
        let mut seen = BTreeSet::new();
        let mut ids: Vec<usize> = search::matching_paths(self.cfg, &self.state.query)
            .into_iter()
            .filter_map(|p| p.canonicalize().ok())
            .filter(|p| !self.options.favorites || self.state.favorites.contains(p))
            .filter_map(|p| positions.get(&p).copied())
            .filter(|id| seen.insert(*id))
            .collect();
        if self.options.measured() {
            note(&format!(
                "checking sampled image/palette filters for {} candidates",
                ids.len()
            ));
            for id in &ids {
                self.measure(*id);
            }
            ids.retain(|id| {
                self.measured
                    .get(id)
                    .is_some_and(|p| p.as_ref().is_ok_and(|p| self.options.matches(p)))
            });
        }
        self.queue.replace(ids);
        self.page = 0;
    }

    fn save(&self) {
        if let Err(e) = self.state.save(self.cfg) {
            note(&format!("preferences not saved: {e}"));
        }
    }

    fn title(&self, id: usize, width: usize) -> String {
        let mark = if self.state.favorites.contains(&self.paths[id]) {
            " *"
        } else {
            ""
        };
        let name = display_text(
            &self.paths[id]
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
        );
        truncate_ellipsis(&format!("{}{}  {name}", id + 1, mark), width)
    }

    fn sheet(&mut self) {
        let cols = term_cols().max(1);
        let page_size = self.options.page_size;
        let pages = self.queue.ids.len().div_ceil(page_size).max(1);
        self.page = self.page.min(pages - 1);
        say(&format!(
            "\nWallpaper browser  |  {} matches  |  page {}/{}",
            self.queue.ids.len(),
            self.page + 1,
            pages
        ));
        say(&format!(
            "Query: {}{}",
            if self.state.query.is_empty() {
                "all wallpapers".into()
            } else {
                display_text(&self.state.query.join(" "))
            },
            if self.options.favorites {
                "  [favorites]"
            } else {
                ""
            }
        ));
        let ids = self
            .queue
            .ids
            .iter()
            .skip(self.page * page_size)
            .take(page_size)
            .copied()
            .collect::<Vec<_>>();
        if ids.is_empty() {
            say("No matches. Type query to clear the query, or all to leave favorites.");
            return;
        }
        // Redirected output is plain, stable text; it never contains graphics,
        // terminal escapes, image bytes, or a derived-palette side effect.
        if !self.graphics || cols < 8 {
            for id in ids {
                say(&format!(
                    "{}  {}",
                    id + 1,
                    display_text(&self.paths[id].to_string_lossy())
                ));
            }
            return;
        }
        let cards = (cols / 32).clamp(1, 3);
        let width = cols.saturating_sub((cards - 1) * 3) / cards;
        for row in ids.chunks(cards) {
            let mut pictures = Vec::new();
            for id in row {
                self.measure(*id);
                let preview = report::render_preview(&self.paths[*id], width, 7);
                if let Some(p) = &preview {
                    print!("{}", p.apc);
                }
                pictures.push(preview);
            }
            for line in 0..7 {
                for (i, p) in pictures.iter().enumerate() {
                    if i > 0 {
                        print!("   ");
                    }
                    if let Some(s) = p.as_ref().and_then(|p| p.rows.get(line)) {
                        print!("{s}");
                    } else {
                        print!("{}", " ".repeat(width));
                    }
                }
                println!();
            }
            for (i, id) in row.iter().enumerate() {
                if i > 0 {
                    print!("   ");
                }
                print!("{}", pad_cells(&self.title(*id, width), width));
            }
            println!();
            for (i, id) in row.iter().enumerate() {
                if i > 0 {
                    print!("   ");
                }
                if let Some(Ok(p)) = self.measured.get(id) {
                    let count = (width / 3).min(8);
                    let sw: Vec<String> = p
                        .palette
                        .colors
                        .iter()
                        .take(count)
                        .map(|c| c.hex().trim_start_matches('#').into())
                        .collect();
                    print!(
                        "{}{}",
                        report::swatch_cells(&sw).0,
                        " ".repeat(width.saturating_sub(count * 3))
                    );
                } else {
                    print!("{}", pad_cells("palette unavailable", width));
                }
            }
            println!("\n");
        }
        say("select ID · next/prev · page N · shuffle · query TERMS · favorite ID · help · quit");
    }

    fn selected(&mut self) {
        let Some(id) = self.queue.selected() else {
            note("select a wallpaper first");
            return;
        };
        self.page = self.queue.cursor.unwrap_or(0) / self.options.page_size;
        self.state.visit(&self.paths[id]);
        self.save();
        self.measure(id);
        let cols = term_cols().max(1);
        say(&format!("\n{}", self.title(id, cols)));
        say(&display_text(&self.paths[id].to_string_lossy()));
        if self.graphics
            && cols >= 8
            && let Some(p) = report::render_preview(&self.paths[id], cols.min(80), 18)
        {
            print!("{}", p.apc);
            for line in p.rows {
                println!("{line}");
            }
        }
        match &self.measured[&id] {
            Ok(p) => {
                say(&format!(
                    "{} x {}  | opacity {:.2} | target {:.2}:1",
                    p.profile.width, p.profile.height, p.opacity, p.contrast
                ));
                say(&format!(
                    "Sampled readability: worst {:.2}:1 | {:.1}% coverage | {} points",
                    p.readability.worst,
                    p.readability.coverage * 100.0,
                    p.readability.samples
                ));
                say(&format!(
                    "Measured image: brightness {:.3} | chroma {:.3} | texture {:.3}",
                    p.profile.mean_luminance, p.profile.chroma, p.profile.texture
                ));
                print!("{}", p.specimen(cols));
                say("Preview only. Type apply to set this wallpaper and its final palette.");
            }
            Err(e) => note(&format!("preview unavailable: {e}")),
        }
    }

    fn rank(&mut self, mode: &str) {
        let selected = self.queue.selected();
        if mode != "calmer" && selected.is_none() {
            note("select a reference image first");
            return;
        }
        note(&format!(
            "measuring {} candidates for {mode}",
            self.queue.ids.len()
        ));
        for id in self.queue.ids.clone() {
            self.measure(id);
        }
        let reference = selected
            .and_then(|id| self.measured.get(&id))
            .and_then(|p| p.as_ref().ok());
        if mode != "calmer" && reference.is_none() {
            note("reference image cannot be measured");
            return;
        }
        let value = |id: &usize| {
            self.measured
                .get(id)
                .and_then(|p| p.as_ref().ok())
                .map(|p| {
                    if mode == "calmer" {
                        p.profile.texture
                    } else {
                        p.profile.distance(&reference.unwrap().profile)
                            * if mode == "different" { -1.0 } else { 1.0 }
                    }
                })
                .unwrap_or(f64::INFINITY)
        };
        self.queue
            .ids
            .sort_by(|a, b| value(a).total_cmp(&value(b)).then(a.cmp(b)));
        self.queue.cursor = None;
        self.page = 0;
        self.sheet();
    }

    fn command(&mut self, line: &str) -> bool {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            [] => {}
            ["quit" | "q"] => return false,
            ["help"] => usage(),
            ["list"] => self.sheet(),
            ["next" | "prev"] => {
                if self.queue.step(words[0] == "next") {
                    self.selected();
                } else {
                    note("end of this queue; use prev, page N, or shuffle");
                }
            }
            ["select", n] => match n.parse::<usize>().ok().and_then(|n| n.checked_sub(1)) {
                Some(id) if self.queue.select(id) => self.selected(),
                _ => note("choose an ID from the current results"),
            },
            ["page", n] => {
                match n.parse::<usize>().ok().filter(|n| {
                    *n > 0 && *n <= self.queue.ids.len().div_ceil(self.options.page_size)
                }) {
                    Some(n) => {
                        self.page = n - 1;
                        self.sheet();
                    }
                    None => note("page number is outside these results"),
                }
            }
            ["shuffle"] => {
                let seed = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as u64;
                self.queue.shuffle(seed);
                self.page = 0;
                self.sheet();
            }
            ["favorite"] | ["favorite", _] => {
                let id = if words.len() == 1 {
                    self.queue.selected()
                } else {
                    words[1]
                        .parse::<usize>()
                        .ok()
                        .and_then(|n| n.checked_sub(1))
                        .filter(|id| self.queue.ids.contains(id))
                };
                if let Some(id) = id {
                    match self.state.favorite(&self.paths[id]) {
                        Ok(on) => {
                            note(if on {
                                "saved favorite"
                            } else {
                                "removed favorite"
                            });
                            self.save();
                        }
                        Err(e) => note(e),
                    }
                } else {
                    note("select a wallpaper or use favorite ID");
                }
            }
            ["favorites" | "all"] => {
                self.options.favorites = words[0] == "favorites";
                self.refresh();
                self.sheet();
            }
            ["history"] => {
                for path in self.state.history.iter().rev() {
                    if let Some(id) = self.paths.iter().position(|p| p == path) {
                        say(&self.title(id, term_cols()));
                    }
                }
            }
            ["query", terms @ ..] => {
                if terms.len() > 32 {
                    note("use at most 32 query terms");
                } else {
                    self.state.query = terms.iter().map(|s| s.to_string()).collect();
                    self.save();
                    self.refresh();
                    self.sheet();
                }
            }
            ["calmer" | "similar" | "different"] => self.rank(words[0]),
            ["apply"] => {
                if let Some(id) = self.queue.selected() {
                    // Re-prepare immediately before apply. External image or
                    // opacity changes must first be shown as a new preview.
                    let current_identity = identity(&self.paths[id]);
                    let fresh = if current_identity.is_some() {
                        presentation::preview(self.cfg, &self.paths[id])
                    } else {
                        Err("image is no longer a regular library file".into())
                    };
                    let source_same = current_identity
                        .as_ref()
                        .is_some_and(|i| self.identities.get(&id) == Some(i));
                    let same = match (self.measured.get(&id), &fresh) {
                        (Some(Ok(old)), Ok(new)) => {
                            source_same
                                && *old.palette == *new.palette
                                && old.opacity == new.opacity
                                && old.contrast == new.contrast
                        }
                        _ => false,
                    };
                    if same {
                        apply::use_image(self.cfg, &self.paths[id], false);
                    } else {
                        self.measured.insert(id, fresh);
                        note("the preview changed or is unavailable; review it before applying");
                        self.selected();
                    }
                } else {
                    note("select and preview a wallpaper first");
                }
            }
            _ => note("unknown command; type help (nothing was applied)"),
        }
        true
    }
}

pub fn run(cfg: &Config, args: &[String]) {
    let options = match Options::parse(args) {
        Ok(Some(o)) => o,
        Ok(None) => {
            usage();
            return;
        }
        Err(e) => die(&format!("{e}; try theme browse --help")),
    };
    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    let mut state = State::load(cfg);
    if options.reset || !options.terms.is_empty() {
        state.query = options.terms.clone();
    }
    let mut paths: Vec<PathBuf> = search::matching_paths(cfg, &[])
        .into_iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect();
    paths.sort();
    paths.dedup();
    let graphics = interactive && std::env::var("KITTY_WINDOW_ID").is_ok_and(|v| !v.is_empty());
    let mut browser = Browser {
        cfg,
        options,
        state,
        paths,
        measured: BTreeMap::new(),
        identities: BTreeMap::new(),
        queue: Queue::default(),
        page: 0,
        graphics,
    };
    browser.refresh();
    browser.sheet();
    if !interactive {
        return;
    }
    browser.save();
    let mut input = io::stdin().lock();
    loop {
        print!("{}", truncate_ellipsis("browse> ", term_cols().max(1)));
        let _ = io::stdout().flush();
        let mut line = String::new();
        // Bound input before allocation, not after read_line has consumed an
        // arbitrarily large paste. Drain the rest of an overlong line once.
        let read = (&mut input).take(4097).read_line(&mut line);
        match read {
            Ok(0) => break,
            Ok(_) if line.len() > 4096 => {
                if !line.ends_with('\n') {
                    while let Ok(bytes) = input.fill_buf() {
                        if bytes.is_empty() {
                            break;
                        }
                        let end = bytes.iter().position(|b| *b == b'\n');
                        let count = end.map_or(bytes.len(), |i| i + 1);
                        input.consume(count);
                        if end.is_some() {
                            break;
                        }
                    }
                }
                note("command too long; nothing was applied");
            }
            Ok(_)
                if line
                    .trim_end_matches(['\r', '\n'])
                    .chars()
                    .any(char::is_control) =>
            {
                note("command contains control characters; nothing was applied")
            }
            Ok(_) => {
                if !browser.command(line.trim()) {
                    break;
                }
            }
            Err(_) => {
                note("input unavailable; leaving browser");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn strict_grammar_rejects_invalid_values_before_browsing() {
        for a in [
            vec!["--unknown"],
            vec!["--page-size", "2.5"],
            vec!["--page-size", "0"],
            vec!["--min-contrast", "NaN"],
            vec!["--coverage", "1.1"],
            vec!["--color", "invisible"],
            vec!["--aspect", "16:0"],
            vec!["--min-width", "-1"],
        ] {
            assert!(Options::parse(&args(&a)).is_err(), "{a:?}");
        }
        let o = Options::parse(&args(&[
            "blue",
            "--page-size",
            "8",
            "--min-contrast",
            "7",
            "--aspect",
            "16:9",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(o.terms, vec!["blue"]);
        assert_eq!(o.page_size, 8);
        assert_eq!(o.contrast, Some(7.0));
    }

    #[test]
    fn shuffled_queue_has_no_repeats_and_previous_reverses_navigation() {
        let mut q = Queue::default();
        q.replace((0..20).collect());
        q.shuffle(19);
        assert_ne!(q.ids, (0..20).collect::<Vec<_>>());
        let mut seen = BTreeSet::new();
        while q.step(true) {
            assert!(seen.insert(q.selected().unwrap()));
        }
        assert_eq!(seen.len(), 20);
        let last = q.selected();
        assert!(q.step(false));
        assert!(q.step(true));
        assert_eq!(q.selected(), last);
        assert!(!q.step(true));
        assert!(!q.select(100));
        assert_eq!(q.selected(), last);
    }

    #[test]
    fn select_keeps_snapshot_ids_when_results_are_filtered() {
        let mut q = Queue::default();
        q.replace(vec![3, 17, 2]);
        assert!(q.select(17));
        assert_eq!(q.cursor, Some(1));
        assert!(q.step(false));
        assert_eq!(q.selected(), Some(3));
    }

    #[test]
    fn palette_color_filter_uses_colors_not_search_words() {
        let blue = pigment::Rgb::parse("#2244dd").unwrap();
        assert!(color_matches("blue", blue));
        assert!(color_matches("#2244dd", blue));
        assert!(!color_matches("red", blue));
    }

    #[test]
    fn help_paths_metrics_and_footer_fit_narrow_widths() {
        for cols in [1, 8, 13, 25, 60] {
            for text in [
                HELP,
                "/library/a-very-long-unbroken-wallpaper-name.png",
                "/library/中国山水中国山水中国山水中国山水中国山水.png",
                "/library/e\u{301}toile-👩‍💻-👍🏽-🇺🇸-\u{f120}.png",
                "Sampled readability: worst 7.02:1 | 99.7% coverage | 256 points",
                "select ID · next/prev · page N · shuffle · favorite ID · help · quit",
            ] {
                assert!(
                    prose_lines(text, cols)
                        .iter()
                        .all(|line| crate::ui::display_width(line) <= cols),
                    "width {cols}: {text}"
                );
            }
        }
    }

    #[test]
    fn no_apply_navigation_and_preferences_leave_settings_untouched() {
        let dir = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("browser-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a calm blue picture.png");
        image::RgbImage::from_pixel(32, 24, image::Rgb([24, 45, 120]))
            .save(&path)
            .unwrap();
        let cfg = Config {
            wallpaper_dirs: vec![dir.clone()],
            wallpaper_dirs_display: dir.display().to_string(),
            cache_dir: dir.join("cache"),
            kitty_dir: dir.join("kitty"),
            current: dir.join("kitty/current-theme.conf"),
            formats: vec!["png".into()],
            contrast: 7.0,
            no_apply: true,
        };
        let mut browser = Browser {
            cfg: &cfg,
            options: Options {
                page_size: 6,
                ..Options::default()
            },
            state: State::default(),
            paths: vec![path.clone()],
            measured: BTreeMap::new(),
            identities: BTreeMap::new(),
            queue: Queue::default(),
            page: 0,
            graphics: false,
        };
        browser.queue.replace(vec![0]);
        assert!(browser.command("apply"));
        assert!(browser.queue.selected().is_none());
        assert!(browser.command("select 1"));
        assert_eq!(browser.queue.selected(), Some(0));
        assert!(browser.command("favorite"));
        assert!(browser.state.favorites.contains(&path));
        assert_eq!(browser.state.history, vec![path]);
        assert!(browser.command("apply"));
        assert!(!cfg.cache_dir.exists());
        assert!(!cfg.kitty_dir.exists());
        assert!(!browser.command("quit"));
        // Ordinary metadata-preserving edits still invalidate a preview:
        // ctime is independent of a restored file modification timestamp.
        let record = dir.join("identity.txt");
        std::fs::write(&record, b"first").unwrap();
        let original = std::fs::metadata(&record).unwrap().modified().unwrap();
        let before = identity(&record);
        std::fs::write(&record, b"later").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&record)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(original))
            .unwrap();
        let after = identity(&record);
        assert_eq!(before.unwrap().2, after.unwrap().2);
        assert_eq!(before.unwrap().3, after.unwrap().3);
        assert_eq!(before.unwrap().4, after.unwrap().4);
        assert_ne!(before, after);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

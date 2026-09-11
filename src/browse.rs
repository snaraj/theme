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
use rustix::event::{PollFd, PollFlags, Timespec};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, IsTerminal, Write};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

Keys (pressed on an empty line, no Return):
  Right/Left   next/previous picture, with its preview
  Down/Up, PageDown/PageUp, Space   one page, redrawn
  Home/End   first/last page

Commands (type a line, then Return):
  select ID   next (n)   prev (p)   page N   list   shuffle
  favorite [ID]   favorites   all   history
  query TERMS   calmer   similar   different
  apply   help (?)   quit (q)

Backspace, Ctrl-U and Ctrl-W edit the typed line and Escape clears it;
with text typed, keys insert or edit instead of moving. Ctrl-C or Ctrl-D on
an empty line leaves the browser, and the terminal mode is given back on
every exit.

next/prev follow the current queue; shuffle visits each result once.
page N shows a sheet without selecting or applying anything. IDs stay
stable during the session. Query and local favorites/history are saved.
query with no terms clears the query. all leaves the favorites filter.
calmer ranks measured image texture; similar/different compare the selected
image's mean Oklab color. Readability is sampled, not an every-pixel guarantee.
EOF exits. Non-interactive input/output prints one deterministic page, reads
no keys and applies no wallpaper. THEME_NO_APPLY also skips state writes.";

/// The sheet's own reminder. It names the keys first, because the keys are
/// what a person reaches for; every word wraps, so it survives 25 columns.
const FOOTER: &str = "arrows move · Space/PgDn page · Home/End ends · \
                      select ID · query TERMS · favorite ID · help · quit";

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
        let pages = self.pages();
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
        say(FOOTER);
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

    /// One picture forward or back — what `next`/`prev` and the Right/Left
    /// keys both do. The preview moves the page under it (see `selected`).
    fn step(&mut self, forward: bool) {
        if self.queue.step(forward) {
            self.selected();
        } else {
            note("end of this queue; use prev, page N, or shuffle");
        }
    }

    fn pages(&self) -> usize {
        self.queue.ids.len().div_ceil(self.options.page_size).max(1)
    }

    /// One page forward or back, redrawn. The ends are ends: an edge prints
    /// the queue's own note rather than wrapping around to the far side.
    fn turn(&mut self, forward: bool) {
        let next = if forward {
            (self.page + 1 < self.pages()).then_some(self.page + 1)
        } else {
            self.page.checked_sub(1)
        };
        match next {
            Some(page) => self.show(page),
            None if forward => note("end of these pages; use Home or page N"),
            None => note("start of these pages; use End or page N"),
        }
    }

    /// A page by number, clamped to the last one, redrawn.
    fn show(&mut self, page: usize) {
        self.page = page.min(self.pages() - 1);
        self.sheet();
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
            ["help" | "?"] => usage(),
            ["list"] => self.sheet(),
            ["next" | "n" | "prev" | "p"] => self.step(matches!(words[0], "next" | "n")),
            ["select", n] => match n.parse::<usize>().ok().and_then(|n| n.checked_sub(1)) {
                Some(id) if self.queue.select(id) => self.selected(),
                _ => note("choose an ID from the current results"),
            },
            ["page", n] => match n
                .parse::<usize>()
                .ok()
                .filter(|n| *n > 0 && *n <= self.pages())
            {
                Some(n) => self.show(n - 1),
                None => note("page number is outside these results"),
            },
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

/// What one keypress means here. The parser below is a pure function over
/// bytes, so hostile input is a table of cases instead of a live terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    /// A byte of typed text — never a control byte.
    Byte(u8),
    Space,
    Enter,
    Backspace,
    KillLine,
    KillWord,
    Interrupt,
    EndOfFile,
    Escape,
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    /// A complete sequence with no meaning here: consumed and dropped,
    /// never echoed and never executed.
    Discard,
    /// The bytes so far are the beginning of a longer sequence.
    Incomplete,
}

/// The longest sequence the reader will hold before dropping it. Real keys
/// are at most six bytes; anything longer is a paste or a hostile stream,
/// and it is consumed in bounded chunks rather than buffered without end.
const KEY_LIMIT: usize = 16;
/// Typed-line bound, unchanged from the line-reading browser.
const LINE_LIMIT: usize = 4096;
/// How long a lone Escape waits for the rest of a sequence: a terminal's
/// own arrow bytes arrive in one burst, a person's Escape does not.
const ESCAPE_WAIT: Duration = Duration::from_millis(50);
const PROMPT: &str = "browse> ";

/// The key at the front of `bytes`, and how many bytes it consumed.
/// `Incomplete` consumes nothing; every other answer consumes at least one
/// byte, so a stream of garbage always makes progress.
fn key(bytes: &[u8]) -> (Key, usize) {
    let Some(&first) = bytes.first() else {
        return (Key::Incomplete, 0);
    };
    match first {
        0x1b => escape(bytes),
        b'\r' | b'\n' => (Key::Enter, 1),
        0x7f | 0x08 => (Key::Backspace, 1),
        0x15 => (Key::KillLine, 1),
        0x17 => (Key::KillWord, 1),
        0x03 => (Key::Interrupt, 1),
        0x04 => (Key::EndOfFile, 1),
        b' ' => (Key::Space, 1),
        b if b < 0x20 => (Key::Discard, 1),
        b => (Key::Byte(b), 1),
    }
}

/// An Escape-introduced sequence: CSI (`ESC [ … final`), SS3 (`ESC O x`),
/// or Escape with any other byte after it, which is dropped whole.
fn escape(bytes: &[u8]) -> (Key, usize) {
    match bytes.get(1) {
        None => (Key::Incomplete, 0),
        Some(b'[') => csi(bytes),
        Some(b'O') => match bytes.get(2) {
            None => (Key::Incomplete, 0),
            Some(&last) => (named(last, 0), 3),
        },
        Some(_) => (Key::Discard, 2),
    }
}

/// `ESC [` parameters (0x30..0x40) and intermediates (0x20..0x30), then one
/// final byte (0x40..0x7f). A sequence that never finishes inside
/// [`KEY_LIMIT`], or whose "final" byte is not one, is dropped — the bytes
/// it had are consumed, so `ESC [ ESC [ ESC [ …` cannot accumulate.
fn csi(bytes: &[u8]) -> (Key, usize) {
    let mut end = 2;
    while end < KEY_LIMIT && bytes.get(end).is_some_and(|b| (0x20..0x40).contains(b)) {
        end += 1;
    }
    if end == KEY_LIMIT {
        return (Key::Discard, KEY_LIMIT);
    }
    let Some(&last) = bytes.get(end) else {
        return (Key::Incomplete, 0);
    };
    if !(0x40..0x7f).contains(&last) {
        return (Key::Discard, end);
    }
    let number = bytes[2..end]
        .split(|b| *b == b';')
        .next()
        .and_then(|p| std::str::from_utf8(p).ok())
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(0);
    (named(last, number), end + 1)
}

/// The key a final byte names, with the sequence's first parameter for the
/// `~` family. Terminals disagree about Home/End, so both spellings of each
/// are accepted; everything else is a sequence we have no meaning for.
fn named(last: u8, number: u16) -> Key {
    match (last, number) {
        (b'A', _) => Key::Up,
        (b'B', _) => Key::Down,
        (b'C', _) => Key::Right,
        (b'D', _) => Key::Left,
        (b'H', _) => Key::Home,
        (b'F', _) => Key::End,
        (b'~', 1 | 7) => Key::Home,
        (b'~', 4 | 8) => Key::End,
        (b'~', 5) => Key::PageUp,
        (b'~', 6) => Key::PageDown,
        _ => Key::Discard,
    }
}

/// The typed line. Bytes accumulate raw and are decoded lossily once, at
/// Return: a paste of invalid UTF-8 is a bad command, never a panic.
#[derive(Default)]
struct Line {
    bytes: Vec<u8>,
    echoed: usize,
    refused: Option<&'static str>,
}

impl Line {
    fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    fn push(&mut self, byte: u8) {
        if self.bytes.len() >= LINE_LIMIT {
            self.refused
                .get_or_insert("command too long; nothing was applied");
            return;
        }
        self.bytes.push(byte);
        self.echo();
    }

    /// A dropped sequence. On an empty line it was a keypress with no
    /// meaning here; inside a typed line it is a pasted control byte, and
    /// the line it arrived in is refused whole at Return rather than run
    /// with the byte quietly removed.
    fn refuse_control(&mut self) {
        if !self.bytes.is_empty() {
            self.refused
                .get_or_insert("command contains control characters; nothing was applied");
        }
    }

    fn clear(&mut self) {
        self.bytes.clear();
        self.refused = None;
        self.redraw();
    }

    /// Delete the last character, not the last byte: a multi-byte glyph
    /// leaves no half behind.
    fn backspace(&mut self) {
        while self.bytes.pop().is_some_and(|b| (0x80..0xc0).contains(&b)) {}
        self.redraw();
    }

    fn kill_word(&mut self) {
        while self.bytes.last() == Some(&b' ') {
            self.bytes.pop();
        }
        while matches!(self.bytes.last(), Some(b) if *b != b' ') {
            self.bytes.pop();
        }
        self.redraw();
    }

    /// The command to run, or the one refusal that poisoned this line.
    fn take(&mut self) -> Result<String, &'static str> {
        let refused = self.refused.take();
        let line = String::from_utf8_lossy(&self.bytes).into_owned();
        self.bytes.clear();
        self.echoed = 0;
        refused.map_or(Ok(line), Err)
    }

    /// Prompt and line from the start of the row. After a deletion the tail
    /// of the old line is still on screen, which is what `\x1b[K` erases.
    fn redraw(&mut self) {
        print!("\r{}\x1b[K", truncate_ellipsis(PROMPT, term_cols().max(1)));
        self.echoed = 0;
        self.echo();
    }

    /// Echo what raw mode stopped the terminal from echoing, one COMPLETE
    /// character at a time: a multi-byte glyph arrives byte by byte and
    /// half of one must not reach the screen.
    fn echo(&mut self) {
        while self.echoed < self.bytes.len() {
            let tail = &self.bytes[self.echoed..];
            match std::str::from_utf8(tail) {
                Ok(text) => {
                    print!("{text}");
                    self.echoed = self.bytes.len();
                }
                Err(e) if e.valid_up_to() > 0 => {
                    print!("{}", String::from_utf8_lossy(&tail[..e.valid_up_to()]));
                    self.echoed += e.valid_up_to();
                }
                Err(e) => match e.error_len() {
                    Some(bad) => {
                        print!("\u{fffd}");
                        self.echoed += bad;
                    }
                    // Truncated, not invalid: the rest is still in flight.
                    None => break,
                },
            }
        }
        let _ = io::stdout().flush();
    }
}

/// The next key, waiting for it. `None` when the terminal's input ended or
/// failed, which leaves the browser exactly as EOF always has.
fn next_key(pending: &mut Vec<u8>) -> Option<Key> {
    loop {
        let (key, used) = key(pending);
        if key != Key::Incomplete {
            pending.drain(..used);
            return Some(key);
        }
        // Escape is a key of its own once nothing follows it promptly, and a
        // sequence cut off mid-flight is dropped rather than held forever.
        if !pending.is_empty() && !readable(ESCAPE_WAIT) {
            let lone = pending.len() == 1;
            pending.clear();
            return Some(if lone { Key::Escape } else { Key::Discard });
        }
        let mut buffer = [0u8; 512];
        match rustix::io::read(io::stdin(), &mut buffer[..]) {
            Ok(0) => return None,
            Ok(read) => pending.extend_from_slice(&buffer[..read]),
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => {
                note("input unavailable; leaving browser");
                return None;
            }
        }
    }
}

/// Whether the terminal has bytes for us within `wait`. A signal — a window
/// resize, say — is not an answer: poll again rather than report a lone
/// Escape that nobody pressed.
fn readable(wait: Duration) -> bool {
    let terminal = io::stdin();
    let mut fds = [PollFd::new(&terminal, PollFlags::IN)];
    let wait = Timespec {
        tv_sec: 0,
        tv_nsec: wait.subsec_nanos() as _,
    };
    loop {
        match rustix::event::poll(&mut fds, Some(&wait)) {
            Ok(ready) => return ready > 0,
            Err(rustix::io::Errno::INTR) => {}
            // Let the read that follows report the real failure.
            Err(_) => return true,
        }
    }
}

/// The interactive session. Raw mode is entered only here — `run` has
/// already proved both ends are terminals — and the guard gives the
/// terminal back on every exit from this function, panic included.
///
/// Keys move only on an empty line. Mid-command they are text or nothing at
/// all, so neither a paste nor a stray sequence can navigate or apply.
fn interact(browser: &mut Browser) {
    let _cooked = crate::ui::raw_mode();
    let mut line = Line::default();
    let mut pending = Vec::new();
    let mut prompt = true;
    loop {
        if prompt {
            line.redraw();
            prompt = false;
        }
        let Some(key) = next_key(&mut pending) else {
            break;
        };
        let empty = line.is_empty();
        match key {
            Key::Enter => {
                println!();
                match line.take() {
                    Err(refusal) => note(refusal),
                    Ok(command) => {
                        if !browser.command(command.trim()) {
                            break;
                        }
                    }
                }
                prompt = true;
            }
            Key::Byte(byte) => line.push(byte),
            Key::Space if !empty => line.push(b' '),
            Key::Backspace => line.backspace(),
            Key::KillWord => line.kill_word(),
            Key::KillLine | Key::Escape => line.clear(),
            Key::Interrupt if empty => break,
            Key::Interrupt => line.clear(),
            Key::EndOfFile if empty => break,
            Key::Discard => line.refuse_control(),
            // A movement key with text typed: ignored, and the line stands.
            _ if !empty => {}
            Key::Right
            | Key::Left
            | Key::Up
            | Key::Down
            | Key::PageUp
            | Key::PageDown
            | Key::Space
            | Key::Home
            | Key::End => {
                // Leave the prompt's row before the key's output lands.
                println!();
                match key {
                    Key::Right | Key::Left => browser.step(key == Key::Right),
                    Key::Up | Key::PageUp => browser.turn(false),
                    Key::Home => browser.show(0),
                    Key::End => browser.show(usize::MAX),
                    _ => browser.turn(true),
                }
                prompt = true;
            }
            Key::EndOfFile | Key::Incomplete => {}
        }
    }
    println!();
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
    interact(&mut browser);
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
    fn keys_parse_the_sequences_real_terminals_send() {
        let cases: &[(&[u8], Key, usize)] = &[
            (b"\x1b[A", Key::Up, 3),
            (b"\x1b[B", Key::Down, 3),
            (b"\x1b[C", Key::Right, 3),
            (b"\x1b[D", Key::Left, 3),
            (b"\x1bOA", Key::Up, 3),
            (b"\x1bOB", Key::Down, 3),
            (b"\x1bOC", Key::Right, 3),
            (b"\x1bOD", Key::Left, 3),
            (b"\x1b[H", Key::Home, 3),
            (b"\x1b[F", Key::End, 3),
            (b"\x1bOH", Key::Home, 3),
            (b"\x1bOF", Key::End, 3),
            (b"\x1b[1~", Key::Home, 4),
            (b"\x1b[7~", Key::Home, 4),
            (b"\x1b[4~", Key::End, 4),
            (b"\x1b[8~", Key::End, 4),
            (b"\x1b[5~", Key::PageUp, 4),
            (b"\x1b[6~", Key::PageDown, 4),
            // A modified arrow is still that arrow, not a stray sequence.
            (b"\x1b[1;5C", Key::Right, 6),
            (b"\r", Key::Enter, 1),
            (b"\n", Key::Enter, 1),
            (b"\x7f", Key::Backspace, 1),
            (b"\x08", Key::Backspace, 1),
            (b"\x15", Key::KillLine, 1),
            (b"\x17", Key::KillWord, 1),
            (b"\x03", Key::Interrupt, 1),
            (b"\x04", Key::EndOfFile, 1),
            (b" ", Key::Space, 1),
            (b"q", Key::Byte(b'q'), 1),
            // One key at a time out of a burst that holds several.
            (b"\x1b[C\x1b[D\n", Key::Right, 3),
        ];
        for (bytes, want, used) in cases {
            assert_eq!(key(bytes), (*want, *used), "{bytes:?}");
        }
    }

    #[test]
    fn truncated_and_hostile_sequences_stay_bounded() {
        for partial in [
            b"\x1b".as_slice(),
            b"\x1b[",
            b"\x1b[5",
            b"\x1bO",
            b"\x1b[1;5",
        ] {
            assert_eq!(key(partial), (Key::Incomplete, 0), "{partial:?}");
        }
        // A stream that never finishes a sequence still makes progress: no
        // unbounded buffer, no repeat of the same byte, no panic.
        let garbage = b"\x1b[".repeat(1000);
        let (mut rest, mut dropped) = (garbage.as_slice(), 0);
        loop {
            let (k, used) = key(rest);
            if k == Key::Incomplete {
                break;
            }
            assert_eq!((k, used > 0), (Key::Discard, true));
            rest = &rest[used..];
            dropped += 1;
        }
        assert_eq!((dropped, rest), (999, b"\x1b[".as_slice()));
        // An absurd parameter run is consumed at the limit, never held.
        let long = [b"\x1b[".as_slice(), &b"1".repeat(64)].concat();
        assert_eq!(key(&long), (Key::Discard, KEY_LIMIT));
        // Well-formed sequences with no meaning here, and bare control
        // bytes, are consumed and dropped.
        let unknown: &[(&[u8], usize)] = &[
            (b"\x1b[200~", 6),
            (b"\x1b[Z", 3),
            (b"\x1bZ", 2),
            (b"\x1bOZ", 3),
            (b"\x1b[3~", 4),
            (b"\x01", 1),
        ];
        for (bytes, used) in unknown {
            assert_eq!(key(bytes), (Key::Discard, *used), "{bytes:?}");
        }
    }

    #[test]
    fn typed_line_edits_bounds_and_refuses_a_poisoned_paste() {
        let mut line = Line::default();
        for b in b"select 12" {
            line.push(*b);
        }
        line.backspace();
        assert_eq!(line.bytes, b"select 1");
        // Ctrl-W takes the word, and the next one takes the space with it.
        line.kill_word();
        assert_eq!(line.bytes, b"select ");
        line.kill_word();
        assert!(line.is_empty());
        for b in b"select" {
            line.push(*b);
        }
        // A dropped control byte poisons the line it arrived in — that line
        // is refused whole rather than run with the byte quietly removed.
        line.refuse_control();
        assert_eq!(
            line.take(),
            Err("command contains control characters; nothing was applied")
        );
        assert!(line.is_empty());
        // The refusal belongs to that line only, and on an empty line a
        // dropped key says nothing at all.
        line.refuse_control();
        assert_eq!(line.take(), Ok(String::new()));
        // The bound refuses the line instead of silently truncating it.
        for _ in 0..LINE_LIMIT + 10 {
            line.push(b'x');
        }
        assert_eq!(line.bytes.len(), LINE_LIMIT);
        assert_eq!(line.take(), Err("command too long; nothing was applied"));
        // Multi-byte glyphs delete whole; invalid bytes decode lossily at
        // Return rather than panicking mid-line.
        for b in "界æ".as_bytes() {
            line.push(*b);
        }
        line.backspace();
        assert_eq!(line.take(), Ok("界".to_string()));
        for b in [b'a', 0xff, 0xfe] {
            line.push(b);
        }
        assert_eq!(line.take(), Ok("a\u{fffd}\u{fffd}".to_string()));
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
                FOOTER,
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

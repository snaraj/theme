//! `theme list`, `theme preview`, `theme status`, and the pieces they share:
//! scheme reads from the pigment cache (never a guess — unknown is a dash),
//! provenance labels decided by the parsed hostname, and the kitty-graphics
//! inline previews.

use crate::apply::{derive_options, schemes_dir, wallpaper_get};
use crate::config::Config;
use crate::imaging::img_size;
use crate::library::{all_images, resolve_local};
use crate::net::{host_label, url_host};
use crate::ui::{die, display_text, note, truncate_ellipsis};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const PREVIEW_COLS: usize = 7;

fn columns() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .or_else(|| {
            Command::new("tput")
                .arg("cols")
                .output()
                .ok()
                .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        })
        .unwrap_or(100)
}

fn in_kitty() -> bool {
    std::env::var("KITTY_WINDOW_ID")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

fn have(cmd: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|d| d.join(cmd).is_file())
}

/// Where a wallpaper came from, as a short label: the `theme.source` xattr
/// our own downloads record, falling back to macOS's download metadata.
/// Unknown is an honest "-", never a guess; the label is decided by the
/// PARSED hostname, never a substring.
pub fn wall_source(path: &Path) -> String {
    let xattr = Command::new("xattr")
        .args(["-p", "theme.source"])
        .arg(path)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let src = if xattr.is_empty() {
        Command::new("mdls")
            .args(["-raw", "-name", "kMDItemWhereFroms"])
            .arg(path)
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .and_then(|out| {
                out.lines().find_map(|l| {
                    let t = l.trim();
                    let t = t.strip_prefix('"')?;
                    Some(t.split('"').next().unwrap_or("").to_string())
                })
            })
            .unwrap_or_default()
    } else {
        xattr
    };
    let src = display_text(&src);
    if src.is_empty() || src == "(null)" {
        return "-".into();
    }
    if let Some(host) = url_host(&src) {
        if let Some(label) = host_label(&host) {
            return label.into();
        }
        return host.strip_prefix("www.").unwrap_or(&host).to_string();
    }
    let s = src.split_once("://").map(|(_, r)| r).unwrap_or(&src);
    let s = s.strip_prefix("www.").unwrap_or(s);
    s.split(['/', ':']).next().unwrap_or("").to_string()
}

/// The first 8 palette colors a wallpaper derives, from the pigment scheme
/// cache — a cache read, never an image reprocess. No cached entry (or a
/// corrupt one) is a silent None: the caller renders a dash.
pub fn wall_scheme(cfg: &Config, path: &Path) -> Option<Vec<String>> {
    let key = pigment::cache_key(path, &derive_options()).ok()?;
    let raw = fs::read_to_string(schemes_dir(cfg).join(format!("{key}.palette"))).ok()?;
    let pal = pigment::Palette::from_cache_format(&raw)?;
    Some(
        pal.colors
            .iter()
            .take(8)
            .map(|c| c.hex().trim_start_matches('#').to_string())
            .collect(),
    )
}

/// Wallpapers named on the iterator get a scheme derived if missing — the
/// caller bounds the work by bounding the list. Skipped under THEME_NO_APPLY
/// (it mutates the cache).
pub fn backfill_schemes<'a, I: IntoIterator<Item = &'a PathBuf>>(cfg: &Config, paths: I) {
    if cfg.no_apply {
        return;
    }
    let missing: Vec<&PathBuf> = paths
        .into_iter()
        .filter(|p| p.is_file() && wall_scheme(cfg, p).is_none())
        .collect();
    if missing.is_empty() {
        return;
    }
    note(&format!(
        "deriving {} missing colorscheme(s)…",
        missing.len()
    ));
    let mut derived = 0usize;
    for p in &missing {
        if pigment::cached_derive(p, &derive_options(), &schemes_dir(cfg)).is_ok() {
            derived += 1;
        }
    }
    if derived < missing.len() {
        note(&format!(
            "{} wallpaper(s) resisted derivation — still shown as -",
            missing.len() - derived
        ));
    }
}

/// An inline picture via kitty's graphics protocol in unicode-placeholder
/// mode: icat transmits a downscaled image and emits placeholder cells that
/// flow with text. icat's own output positions absolutely, so the cursor
/// choreography is stripped and each line of cells re-emitted with the
/// image-id color reapplied.
pub struct Preview {
    pub apc: String,
    pub rows: Vec<String>,
}

pub fn render_preview(img: &Path, cols: usize, rows: usize) -> Option<Preview> {
    if !in_kitty() || !have("kitten") {
        return None;
    }
    let out = Command::new("kitten")
        .args([
            "icat",
            "--unicode-placeholder",
            "--transfer-mode=file",
            "--stdin=no",
            "--use-window-size",
            "100,50,2000,1000",
            &format!("--place={cols}x{rows}@0x0"),
        ])
        .arg(img)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let out = String::from_utf8_lossy(&out.stdout).into_owned();
    let st = out.find("\x1b\\")?;
    let mut apc = out[..st + 2].to_string();
    if let Some(stripped) = apc.strip_prefix('\r') {
        apc = stripped.to_string();
    }
    let rest = &out[st + 2..];
    // The per-line color that binds cells to the transmitted image id.
    let color = find_color_intro(rest).unwrap_or_default();
    let cleaned = strip_choreography(rest);
    let w: usize = apc
        .split([',', ';'])
        .find_map(|kv| kv.strip_prefix("c=").and_then(|v| v.parse().ok()))
        .unwrap_or(cols);
    let pad = cols.saturating_sub(w);
    let mut out_rows = Vec::new();
    for line in cleaned.lines().take(rows) {
        if line.is_empty() {
            break;
        }
        let mut l = line.to_string();
        if pad > 0 {
            l.push_str(&" ".repeat(pad));
        }
        out_rows.push(format!("{color}{l}\x1b[39m"));
    }
    if out_rows.is_empty() {
        return None;
    }
    Some(Preview {
        apc,
        rows: out_rows,
    })
}

/// The first `ESC[38…m` truecolor/indexed intro in icat's cell output.
fn find_color_intro(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut i = 0;
    while let Some(pos) = s[i..].find("\x1b[38") {
        let start = i + pos;
        let mut j = start + 4;
        while j < b.len() && (b[j].is_ascii_digit() || matches!(b[j], b':' | b';')) {
            j += 1;
        }
        if b.get(j) == Some(&b'm') {
            return Some(s[start..=j].to_string());
        }
        i = start + 4;
    }
    None
}

/// Remove save/restore-cursor, absolute positioning, carriage returns,
/// cursor-forward, and the color intro/outro — the shell's sed chain.
fn strip_choreography(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '\r' {
            i += 1;
            continue;
        }
        if b[i] == '\x1b' && i + 1 < b.len() {
            match b[i + 1] {
                '7' | '8' => {
                    i += 2;
                    continue;
                }
                '[' => {
                    let mut j = i + 2;
                    while j < b.len() && (b[j].is_ascii_digit() || matches!(b[j], ';' | ':')) {
                        j += 1;
                    }
                    if j < b.len() && matches!(b[j], 'H' | 'C') {
                        i = j + 1;
                        continue;
                    }
                    if j < b.len() && b[j] == 'm' {
                        let body: String = b[i + 2..j].iter().collect();
                        if body == "39" || body.starts_with("38") {
                            i = j + 1;
                            continue;
                        }
                    }
                }
                _ => {}
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn human_bytes(path: &Path) -> String {
    let b = fs::metadata(path).map(|m| m.len()).unwrap_or(0) as f64;
    if b >= 1_048_576.0 {
        format!("{:.1}M", b / 1_048_576.0)
    } else {
        format!("{:.0}K", b / 1024.0)
    }
}

fn added_date(path: &Path) -> String {
    let out = Command::new("stat")
        .args(["-f", "%SB", "-t", "%Y-%m-%d"])
        .arg(path)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if !out.is_empty() {
        return out;
    }
    // Non-macOS: no birth time — modification time is the honest stand-in.
    fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| {
            let days = d.as_secs() / 86400;
            // Civil date from days since epoch (Howard Hinnant's algorithm).
            let z = days as i64 + 719_468;
            let era = z.div_euclid(146_097);
            let doe = z.rem_euclid(146_097);
            let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
            let y = yoe + era * 400;
            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
            let mp = (5 * doy + 2) / 153;
            let d2 = doy - (153 * mp + 2) / 5 + 1;
            let m = if mp < 10 { mp + 3 } else { mp - 9 };
            let y = if m <= 2 { y + 1 } else { y };
            format!("{y:04}-{m:02}-{d2:02}")
        })
        .unwrap_or_default()
}

fn birth_key(path: &Path) -> i64 {
    let out = Command::new("stat")
        .args(["-f", "%B"])
        .arg(path)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok());
    out.unwrap_or_else(|| {
        fs::metadata(path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    })
}

fn swatch_cells(scheme: &[String]) -> (String, usize) {
    // 8 swatches of 2 cells + a trailing space = exactly 24 visible columns.
    let mut out = String::new();
    let mut n = 0;
    for c in scheme {
        if let Some((r, g, b)) = crate::ui::parse_hex6(c) {
            out.push_str(&format!("\x1b[48;2;{r};{g};{b}m  \x1b[0m "));
            n += 1;
        }
    }
    (out, n)
}

#[allow(clippy::print_literal)] // column headers: the formatter pads them, hand-counted spaces would rot
pub fn cmd_list(cfg: &Config, verbose: bool, list_n: usize) {
    let mut files = all_images(cfg);
    files.sort_by_key(|f| std::cmp::Reverse(birth_key(f)));
    let total = files.len();
    let shown = if list_n > 0 && total > list_n {
        list_n
    } else {
        total
    };
    let rows: Vec<PathBuf> = files.into_iter().take(shown).collect();
    backfill_schemes(cfg, rows.iter());

    let cols = columns();
    let pv_ok = verbose && in_kitty() && have("kitten");
    let pvw = if pv_ok { 9 } else { 0 };
    let mut namew = if verbose {
        cols.saturating_sub(71 + pvw)
    } else {
        cols.saturating_sub(32)
    };
    namew = namew.clamp(16, 44);

    println!("wallpapers\n");
    if verbose {
        let pic = if pv_ok {
            format!("{:<9}", "PICTURE")
        } else {
            String::new()
        };
        println!(
            "  {pic}{:<namew$}  {:<24}  {:<10}  {:<6}  {:<7}  {}",
            "TITLE", "COLORSCHEME", "SOURCE", "FORMAT", "SIZE", "ADDED"
        );
    } else {
        println!("  {:<namew$}  {}", "TITLE", "COLORSCHEME");
    }
    for f in &rows {
        let stem = f.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        // Sanitize BEFORE measuring: a stripped byte must not count against
        // the column, and the row must never carry disk bytes as protocol.
        let name = truncate_ellipsis(&display_text(stem), namew);
        let mut pv2 = String::new();
        if pv_ok {
            match render_preview(f, PREVIEW_COLS, 2) {
                Some(p) => {
                    print!("{}", p.apc);
                    print!("  {}", p.rows[0]);
                    pv2 = p
                        .rows
                        .get(1)
                        .cloned()
                        .unwrap_or_else(|| " ".repeat(PREVIEW_COLS));
                }
                None => print!("  {:<7}", ""),
            }
        }
        print!("  {name:<namew$}  ");
        let (sw, n) = wall_scheme(cfg, f)
            .map(|s| swatch_cells(&s))
            .unwrap_or_default();
        print!("{sw}");
        if n == 0 {
            print!("{:<24}", "-");
        } else if verbose {
            for _ in n..8 {
                print!("   ");
            }
        }
        if verbose {
            let src = wall_source(f);
            let src = if src.chars().count() > 10 {
                truncate_ellipsis(&src, 10)
            } else {
                src
            };
            let fmt = f.extension().and_then(|e| e.to_str()).unwrap_or("");
            print!(
                "  {src:<10}  {fmt:<6}  {:<7}  {}",
                human_bytes(f),
                added_date(f)
            );
        }
        println!();
        if !pv2.is_empty() {
            println!("  {pv2}");
        }
    }
    if shown < total {
        println!("\n  newest {shown} of {total} — more: theme list -n <count>, or --all");
    }
}

pub fn cmd_preview(cfg: &Config, arg: Option<&str>) {
    let img: PathBuf = match arg {
        Some(a) => resolve_local(cfg, a).unwrap_or_else(|| {
            die(&format!(
                "no wallpaper uniquely matching '{a}' (looked in {})",
                cfg.wallpaper_dirs_display
            ))
        }),
        None => match wallpaper_get().filter(|p| p.is_file()) {
            Some(p) => p,
            None => die("no current wallpaper to preview — name one: theme preview <wallpaper>"),
        },
    };
    backfill_schemes(cfg, std::iter::once(&img));
    let name = display_text(img.file_stem().and_then(|s| s.to_str()).unwrap_or(""));
    let mut loc = img.display().to_string();
    if let Ok(home) = std::env::var("HOME")
        && let Some(rest) = loc.strip_prefix(&home)
        && rest.starts_with('/')
    {
        loc = format!("~{rest}");
    }
    let loc = display_text(&loc);
    let src = wall_source(&img);
    let dims = img_size(&img);
    let bytes = human_bytes(&img);
    let sw = wall_scheme(cfg, &img)
        .map(|scheme| {
            let mut s = String::new();
            for c in &scheme {
                if let Some((r, g, b)) = crate::ui::parse_hex6(c) {
                    s.push_str(&format!("\x1b[48;2;{r};{g};{b}m    \x1b[0m "));
                }
            }
            s
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "-".into());

    // Values must FIT the right-hand column — a wrapped line lands at column
    // 0 and shreds the block. LOCATION gets its own full-width line below.
    let cols = columns();
    let availw = cols.saturating_sub(22 + 13).max(12);
    let name = truncate_ellipsis(&name, availw);
    let src = truncate_ellipsis(&src, availw);
    let dims_disp = if dims.is_empty() {
        "?".to_string()
    } else {
        dims
    };
    let rlines = [
        String::new(),
        format!("{:<12} {}", "TITLE", name),
        format!("{:<12} {}", "SOURCE", src),
        format!("{:<12} {dims_disp} ({bytes})", "SIZE"),
        String::new(),
        "COLORSCHEME".to_string(),
        sw,
        String::new(),
    ];
    match render_preview(&img, 18, 8) {
        Some(p) => {
            print!("{}", p.apc);
            for i in 0..8 {
                let line = p
                    .rows
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| format!("{:<18}", ""));
                println!(
                    "  {line}  {}",
                    rlines.get(i).map(String::as_str).unwrap_or("")
                );
            }
        }
        None => {
            for line in &rlines {
                println!("  {line}");
            }
        }
    }
    println!("  {:<12} {loc}", "LOCATION");
}

/// The 16 colors of the active scheme: the pigment cache, or whatever other
/// conf current-theme.conf still points at.
pub fn scheme_colors(cfg: &Config) -> Vec<String> {
    let inc = include_line(cfg);
    if inc.is_empty() {
        return Vec::new();
    }
    if inc.ends_with("colors-kitty.conf") {
        fs::read_to_string(cfg.cache_dir.join("colors"))
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default()
    } else {
        fs::read_to_string(&inc)
            .map(|s| {
                s.lines()
                    .filter_map(|l| {
                        let mut it = l.split_whitespace();
                        let k = it.next()?;
                        let v = it.next()?;
                        let n: u8 = k.strip_prefix("color")?.parse().ok()?;
                        if n < 16 { Some(v.to_string()) } else { None }
                    })
                    .take(16)
                    .collect()
            })
            .unwrap_or_default()
    }
}

pub fn include_line(cfg: &Config) -> String {
    fs::read_to_string(&cfg.current)
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix("include ").map(str::to_string))
        .unwrap_or_default()
}

pub fn cmd_status(cfg: &Config) {
    let inc = include_line(cfg);
    let mode = if inc.is_empty() {
        "unset".to_string()
    } else if inc.ends_with("colors-kitty.conf") {
        "derived from wallpaper".to_string()
    } else {
        Path::new(&inc)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .trim_end_matches(".conf")
            .to_string()
    };
    let current = fs::read_to_string(cfg.cache_dir.join("wal")).unwrap_or_default();
    let current = current.trim().to_string();
    let desk = wallpaper_get()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    let inc_d = display_text(&inc);
    let current_d = display_text(&current);
    let desk_d = display_text(&desk);
    let mode = display_text(&mode);
    let shown = if !desk_d.is_empty() {
        desk_d.clone()
    } else if !current_d.is_empty() {
        current_d.clone()
    } else {
        "<none>".into()
    };
    println!("current theme:   {shown}");
    println!("mode:            {mode}");
    let colors = scheme_colors(cfg);
    if colors.is_empty() {
        println!("color scheme:    <none>");
    } else {
        println!("color scheme:    {}", crate::ui::swatch_row(&colors));
    }
    println!(
        "palette source:  {}",
        if inc_d.is_empty() { "<none>" } else { &inc_d }
    );
    let cur_path = Path::new(&current);
    let size_note = if !current.is_empty() && cur_path.is_file() {
        format!(" ({})", img_size(cur_path))
    } else {
        String::new()
    };
    println!(
        "palette image:   {}{size_note}",
        if current_d.is_empty() {
            "<none>"
        } else {
            &current_d
        }
    );
    println!(
        "wallpaper dir:   {} ({} images)",
        display_text(&cfg.wallpaper_dirs_display),
        all_images(cfg).len()
    );
    println!("variables:");
    if std::env::var("UNSPLASH_ACCESS_KEY")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        println!("  UNSPLASH_ACCESS_KEY   set (env)");
    } else if crate::unsplash::keychain_read("unsplash-access-key").is_some() {
        println!("  UNSPLASH_ACCESS_KEY   set (Keychain: unsplash-access-key)");
    } else {
        println!("  UNSPLASH_ACCESS_KEY   not set (theme unsplash --help)");
    }
    println!(
        "  THEME_WALLPAPER_DIR   {}",
        display_text(&cfg.wallpaper_dirs_display)
    );
    println!(
        "  THEME_FORMATS         {}",
        display_text(&cfg.formats_display())
    );
    println!(
        "  THEME_CONTRAST        {}",
        display_text(&std::env::var("THEME_CONTRAST").unwrap_or_else(|_| "4.5".into()))
    );
    println!(
        "  THEME_CACHE_DIR       {}",
        display_text(&cfg.cache_dir.display().to_string())
    );
}

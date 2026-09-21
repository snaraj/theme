//! `theme list`, `theme preview`, `theme status`, and the pieces they share:
//! scheme reads from the pigment cache (never a guess — unknown is a dash),
//! provenance labels decided by the parsed hostname, and the kitty-graphics
//! inline previews.

use crate::apply::{derive_options, schemes_dir, wallpaper_to_print};
use crate::config::Config;
use crate::imaging::img_size;
use crate::library::{all_images, resolve_local};
use crate::net::{host_label, url_host};
use crate::ui::{die, display_text, note, truncate_ellipsis};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const PREVIEW_COLS: usize = 7;

fn columns() -> usize {
    // One width authority for every screen: the tty itself first, then
    // COLUMNS, then the conservative stacked default — the PATH-resolved
    // tput probe is gone with it (issue #21).
    crate::ui::term_cols()
}

pub(crate) fn in_kitty() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
        && (std::env::var("KITTY_WINDOW_ID")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
            || std::env::var("TERM").is_ok_and(|v| v == "xterm-kitty"))
}

fn have(cmd: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|d| d.join(cmd).is_file())
}

/// Explain a missing requested picture to a person, without adding noise to pipes.
pub(crate) fn preview_failure(cols: usize) -> Option<&'static str> {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        None
    } else if !in_kitty() {
        Some("image preview unavailable in this terminal; use Kitty to view pictures.")
    } else if cols < 8 {
        Some("image preview unavailable: this window is too narrow; widen it.")
    } else if !have("kitten") {
        Some("image preview unavailable: Kitty's kitten helper was not found on PATH.")
    } else {
        Some("image preview unavailable: Kitty's image renderer failed for this file.")
    }
}

/// Where a wallpaper came from, as a short label: the `theme.source` xattr
/// our own downloads record, falling back to macOS's download metadata.
/// Unknown is an honest "-", never a guess; the label is decided by the
/// PARSED hostname, never a substring.
pub fn wall_source(path: &Path) -> String {
    wall_source_with_mdls(path, || {
        #[cfg(target_os = "macos")]
        {
            true
        }
        #[cfg(not(target_os = "macos"))]
        {
            !mdls_absent(std::env::var_os("PATH").as_deref())
        }
    })
}

fn wall_source_with_mdls(path: &Path, mdls: impl FnOnce() -> bool) -> String {
    let xattr = xattr_value(path, "theme.source");
    let src = if xattr.is_empty() && mdls() {
        Command::new("mdls")
            .args(["-raw", "-name", "kMDItemWhereFroms"])
            .arg(path)
            .output()
            .ok()
            .map(|o| mdls_source(&o.stdout))
            .unwrap_or_default()
    } else {
        xattr
    };
    source_label(&src)
}

fn mdls_source(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .find_map(|line| {
            let text = line.trim().strip_prefix('"')?;
            Some(text.split('"').next().unwrap_or("").to_string())
        })
        .unwrap_or_default()
}

fn source_label(src: &str) -> String {
    let src = display_text(src);
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

/// Amortize macOS metadata startup while retaining xattr precedence and the
/// existing per-file fallback. Results belong only to this requested snapshot.
pub(crate) fn wall_sources(paths: &[&Path]) -> BTreeMap<PathBuf, String> {
    if paths.is_empty() {
        return BTreeMap::new();
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Linux normally has no Spotlight helper. Establish absence once for
        // this batch, rather than attempting one failed spawn per file. An
        // unset PATH, existing candidate, or ambiguous stat error retains the
        // original per-file helper behavior and its xattr precedence.
        let mdls = std::cell::OnceCell::new();
        paths
            .iter()
            .map(|path| {
                let source = wall_source_with_mdls(path, || {
                    *mdls.get_or_init(|| !mdls_absent(std::env::var_os("PATH").as_deref()))
                });
                (path.to_path_buf(), source)
            })
            .collect()
    }
    #[cfg(target_os = "macos")]
    {
        const ARG_BYTES: usize = 64 * 1024;
        if paths.len() < 2 {
            return paths
                .iter()
                .map(|path| (path.to_path_buf(), wall_source(path)))
                .collect();
        }
        let mut sources = BTreeMap::new();
        let mut batch = Vec::new();
        let mut bytes = 0;
        for &path in paths {
            let source = xattr_value(path, "theme.source");
            if !source.is_empty() {
                sources.insert(path.to_path_buf(), source_label(&source));
                continue;
            }
            let size = path.as_os_str().len().saturating_add(1);
            if batch.len() == 32 || size > ARG_BYTES.saturating_sub(bytes) {
                source_batch(&batch, &mut sources);
                batch.clear();
                bytes = 0;
            }
            if size > ARG_BYTES {
                sources.insert(path.to_path_buf(), wall_source(path));
            } else {
                batch.push(path);
                bytes += size;
            }
        }
        source_batch(&batch, &mut sources);
        sources
    }
}

#[cfg(any(not(target_os = "macos"), test))]
fn mdls_absent(path: Option<&std::ffi::OsStr>) -> bool {
    path.is_some_and(|path| {
        std::env::split_paths(path).all(|dir| {
            fs::symlink_metadata(dir.join("mdls")).is_err_and(|error| {
                matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                )
            })
        })
    })
}

#[cfg(target_os = "macos")]
fn source_batch(paths: &[&Path], sources: &mut BTreeMap<PathBuf, String>) {
    if paths.len() < 2 {
        sources.extend(
            paths
                .iter()
                .map(|path| (path.to_path_buf(), wall_source(path))),
        );
        return;
    }
    let before: Vec<_> = paths
        .iter()
        .map(|path| crate::index::file_identity(path))
        .collect();
    let labels = mdls_batch(paths);
    for (i, &path) in paths.iter().enumerate() {
        let label = labels
            .as_ref()
            .filter(|_| before[i].is_some() && before[i] == crate::index::file_identity(path))
            .map(|labels| labels[i].clone())
            .unwrap_or_else(|| wall_source(path));
        sources.insert(path.to_path_buf(), label);
    }
}

#[cfg(target_os = "macos")]
const MDLS_BYTES: usize = 256 * 1024;

#[cfg(target_os = "macos")]
fn mdls_batch(paths: &[&Path]) -> Option<Vec<String>> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = Command::new("mdls")
        .args(["-raw", "-name", "kMDItemWhereFroms"])
        .args(paths)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .expect("piped mdls stdout")
        .take(MDLS_BYTES as u64 + 1)
        .read_to_end(&mut bytes);
    if read.is_err() || bytes.len() > MDLS_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    parse_mdls_batch(&bytes, paths.len(), child.wait().ok()?.success())
}

#[cfg(target_os = "macos")]
fn parse_mdls_batch(bytes: &[u8], count: usize, success: bool) -> Option<Vec<String>> {
    if !success || bytes.len() > MDLS_BYTES || count == 0 || count > 32 {
        return None;
    }
    let fields: Vec<_> = bytes.split(|&byte| byte == 0).collect();
    (fields.len() == count).then(|| {
        fields
            .iter()
            .map(|field| source_label(&mdls_source(field)))
            .collect()
    })
}

/// One extended attribute, read IN-PROCESS — no `xattr` subprocess, because
/// `search` asks five of these per file across a whole library. The bytes are
/// UNTRUSTED, so the display copy is sanitized HERE, where they enter, and
/// bounded by the buffer they land in. Every failure is the same empty answer
/// the callers render as unknown: no attribute, no xattr support on this
/// filesystem, or a value past the buffer (ERANGE).
fn xattr_value(path: &Path, key: &str) -> String {
    let mut buf = [0u8; 512];
    let Ok(n) = rustix::fs::getxattr(path, key, &mut buf) else {
        return String::new();
    };
    display_text(String::from_utf8_lossy(&buf[..n.min(buf.len())]).trim())
}

/// One `theme.*` metadata xattr (an API record persisted at download time),
/// capped for display a second time so the bound survives a wider read buffer
/// — the write-side gate in `record_meta` is not trusted to have run.
pub(crate) fn wall_meta(path: &Path, key: &str) -> String {
    xattr_value(path, key).chars().take(512).collect()
}

/// The first 8 palette colors a wallpaper derives, from the pigment scheme
/// cache — a cache read, never an image reprocess. No cached entry (or a
/// corrupt one) is a silent None: the caller renders a dash.
pub fn wall_scheme(cfg: &Config, path: &Path) -> Option<Vec<String>> {
    let colors = pigment::read_cached_colors(path, &derive_options(), &schemes_dir(cfg)).ok()??;
    Some(
        colors
            .iter()
            .take(8)
            .map(|c| c.hex().trim_start_matches('#').to_string())
            .collect(),
    )
}

/// Wallpapers named on the iterator get a scheme derived if missing — the
/// caller bounds the work by bounding the list. Skipped under THEME_NO_APPLY
/// (it mutates the cache).
pub fn backfill_schemes<'a, I: IntoIterator<Item = &'a PathBuf>>(
    cfg: &Config,
    paths: I,
) -> std::collections::BTreeMap<PathBuf, Vec<String>> {
    let mut schemes = std::collections::BTreeMap::new();
    let mut missing = Vec::new();
    for path in paths {
        if let Some(scheme) = wall_scheme(cfg, path) {
            schemes.insert(path.clone(), scheme);
        } else if path.is_file() {
            missing.push(path);
        }
    }
    if cfg.no_apply || missing.is_empty() {
        return schemes;
    }
    note(&format!(
        "deriving {} missing colorscheme(s)…",
        missing.len()
    ));
    let mut derived = 0;
    for path in &missing {
        if let Ok(palette) = pigment::cached_derive(path, &derive_options(), &schemes_dir(cfg)) {
            schemes.insert(
                (*path).clone(),
                palette
                    .colors
                    .iter()
                    .take(8)
                    .map(|c| c.hex().trim_start_matches('#').to_string())
                    .collect(),
            );
            derived += 1;
        }
    }
    if derived < missing.len() {
        note(&format!(
            "{} wallpaper(s) resisted derivation — still shown as -",
            missing.len() - derived
        ));
    }
    schemes
}

/// An inline picture via kitty's graphics protocol in unicode-placeholder
/// mode: icat transmits a downscaled image and emits placeholder cells that
/// flow with text. icat's own output positions absolutely, so the cursor
/// choreography is stripped and each line of cells re-emitted with the
/// image-id color reapplied.
#[derive(Clone)]
pub struct Preview {
    pub apc: String,
    pub rows: Vec<String>,
}

/// Use the caller's actual cell geometry, including after a resize. Some
/// PTYs omit pixels; their fallback uses conventional 1:2 cells.
pub(crate) fn preview_window_size() -> String {
    let ws = rustix::termios::tcgetwinsize(std::io::stdout()).ok();
    let cols = ws.as_ref().map_or(80, |w| w.ws_col.max(1));
    let rows = ws.as_ref().map_or(24, |w| w.ws_row.max(1));
    let (width, height) = ws
        .filter(|w| w.ws_xpixel > 0 && w.ws_ypixel > 0)
        .map_or((u32::from(cols) * 10, u32::from(rows) * 20), |w| {
            (u32::from(w.ws_xpixel), u32::from(w.ws_ypixel))
        });
    format!("{cols},{rows},{width},{height}")
}

pub fn render_preview(img: &Path, cols: usize, rows: usize) -> Option<Preview> {
    if !in_kitty() || !have("kitten") {
        return None;
    }
    let out = Command::new("kitten")
        .args([
            "icat",
            "--unicode-placeholder",
            "--transfer-mode=stream",
            "--stdin=no",
            "--use-window-size",
            &preview_window_size(),
            &format!("--place={cols}x{rows}@0x0"),
        ])
        .arg(img)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    parse_preview(std::str::from_utf8(&out.stdout).ok()?, cols, rows)
}

fn parse_preview(out: &str, cols: usize, rows: usize) -> Option<Preview> {
    let out = out.trim_start_matches('\r');
    let mut end = 0;
    // A streamed picture can span several APCs. Keep every packet intact,
    // including tmux's wrappers; its doubled ESC is not the wrapper's end.
    loop {
        let rest = &out[end..];
        let wrapped = rest.starts_with("\x1bPtmux;");
        if !wrapped && !rest.starts_with("\x1b_G") {
            break;
        }
        let bytes = rest.as_bytes();
        let mut i = if wrapped { 7 } else { 3 };
        loop {
            if i + 1 >= bytes.len() {
                return None;
            }
            if bytes[i] == 0x1b {
                if wrapped && bytes[i + 1] == 0x1b {
                    i += 2;
                    continue;
                }
                if bytes[i + 1] == b'\\' {
                    end += i + 2;
                    break;
                }
            }
            i += 1;
        }
    }
    if end == 0 {
        return None;
    }
    let apc = out[..end].to_string();
    let rest = &out[end..];
    // The per-line color that binds cells to the transmitted image id.
    let color = find_color_intro(rest)?;
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

#[cfg(test)]
mod graphics_tests {
    use super::parse_preview;

    const CELLS: &str = "\x1b[38:2:1:2:3m\x1b7\x1b[1;0H\u{10eeee}\u{305}\u{305}\n\x1b8";

    #[test]
    fn streamed_packets_and_placeholder_colours_survive() {
        let packets = "\x1b_Ga=T,c=1,r=1,m=1;YWJj\x1b\\\x1b_Gm=0;ZA==\x1b\\";
        let p = parse_preview(&format!("\r{packets}{CELLS}"), 3, 1).unwrap();
        assert_eq!(p.apc, packets);
        assert_eq!(
            p.rows,
            ["\x1b[38:2:1:2:3m\u{10eeee}\u{305}\u{305}  \x1b[39m"]
        );
    }

    #[test]
    fn tmux_wrappers_keep_their_escaped_inner_terminators() {
        let packets = "\x1b_Ga=T,c=1,m=1;YWJj\x1b\\\x1b_Gm=0;ZA==\x1b\\";
        let wrapped = format!("\x1bPtmux;{}\x1b\\", packets.replace('\x1b', "\x1b\x1b"));
        let p = parse_preview(&format!("{wrapped}{CELLS}"), 1, 1).unwrap();
        assert_eq!(p.apc, wrapped);
        assert_eq!(p.rows.len(), 1);
    }

    #[test]
    fn incomplete_graphics_never_become_placeholder_text() {
        for output in [
            CELLS,
            "\x1b_Gm=1;YWJj",
            "\x1bPtmux;\x1b\x1b_Gdata\x1b\x1b\\",
            "\x1b_Ga=T;YWJj\x1b\\plain",
        ] {
            assert!(parse_preview(output, 3, 1).is_none());
        }
    }
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

pub(crate) fn human_bytes(path: &Path) -> String {
    let b = fs::metadata(path).map(|m| m.len()).unwrap_or(0) as f64;
    if b >= 1_048_576.0 {
        format!("{:.1}M", b / 1_048_576.0)
    } else {
        format!("{:.0}K", b / 1024.0)
    }
}

/// When the file was BORN, seconds since the epoch — a syscall, where a
/// `stat` spawn used to be. macOS keeps the birth time in the stat itself;
/// Linux keeps it behind `statx`, and only on the filesystems that record
/// one, so there the modification time stays the honest stand-in it has
/// always been.
#[cfg(target_os = "macos")]
fn birth_secs(path: &Path) -> Option<i64> {
    rustix::fs::stat(path).ok().map(|st| st.st_birthtime)
}

#[cfg(not(target_os = "macos"))]
fn birth_secs(path: &Path) -> Option<i64> {
    #[cfg(target_os = "linux")]
    if let Ok(x) = rustix::fs::statx(
        rustix::fs::CWD,
        path,
        rustix::fs::AtFlags::empty(),
        rustix::fs::StatxFlags::BTIME,
    ) && x.stx_mask & rustix::fs::StatxFlags::BTIME.bits() != 0
    {
        return Some(x.stx_btime.tv_sec);
    }
    rustix::fs::stat(path).ok().map(|st| st.st_mtime)
}

/// macOS spells ADDED in local time; other platforms retain their UTC date.
/// Reuse the parsed macOS zone while its source fingerprint remains unchanged.
/// Unsupported TZ syntax retains the BSD stat path; rejected files fail closed.
pub(crate) fn added_date(path: &Path) -> String {
    #[cfg(target_os = "macos")]
    let timezone = std::env::var_os("TZ");
    #[cfg(target_os = "macos")]
    {
        static ZONE: std::sync::Mutex<DateZoneCache> = std::sync::Mutex::new(DateZoneCache {
            fingerprint: String::new(),
            zone: None,
        });
        // BSD stat reports the link's own birthtime unless -L is requested.
        if let Some(date) = rustix::fs::lstat(path).ok().and_then(|st| {
            ZONE.lock()
                .unwrap_or_else(|_| die("timezone cache unavailable"))
                .get(timezone.as_deref())
                .and_then(|zone| date_in_zone(st.st_birthtime, zone))
        }) {
            return date;
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(out) = {
        let mut command = Command::new("stat");
        command.env_remove("TZ");
        if let Some(value) = &timezone {
            command.env("TZ", value);
        }
        command
            .args(["-f", "%SB", "-t", "%Y-%m-%d"])
            .arg(path)
            .output()
            .ok()
    }
    .filter(|o| o.status.success())
    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    .filter(|s| !s.is_empty())
    {
        return out;
    }
    let Some(secs) = birth_secs(path) else {
        return String::new();
    };
    // Civil date from days since epoch (Howard Hinnant's algorithm).
    let z = secs.div_euclid(86_400) + 719_468;
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
}

#[cfg(target_os = "macos")]
#[derive(Default)]
struct DateZoneCache {
    fingerprint: String,
    zone: Option<tz::TimeZone>,
}

#[cfg(target_os = "macos")]
impl DateZoneCache {
    fn get(&mut self, value: Option<&std::ffi::OsStr>) -> Option<&tz::TimeZone> {
        let before = crate::index::timezone_fingerprint(value);
        if self.fingerprint != before {
            let zone = date_zone(value);
            if before != crate::index::timezone_fingerprint(value) {
                die("timezone files changed while loading; retry the command");
            }
            self.fingerprint = before;
            self.zone = zone;
        }
        self.zone.as_ref()
    }
}

/// Follow the system's timezone symlinks, but read only a stable regular file.
#[cfg(target_os = "macos")]
fn read_zone_file(path: &str) -> std::io::Result<Vec<u8>> {
    use std::io::{Error, ErrorKind, Read};
    use std::os::unix::fs::MetadataExt;
    const MAX: u64 = 1024 * 1024;
    let descriptor = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )?;
    let mut file = std::fs::File::from(descriptor);
    let before = file.metadata()?;
    if !before.is_file() || before.len() > MAX {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "timezone must be a regular file of at most 1 MiB",
        ));
    }
    let stamp = |m: &std::fs::Metadata| {
        (
            m.dev(),
            m.ino(),
            m.len(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        )
    };
    let mut bytes = Vec::new();
    (&mut file).take(MAX + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX || stamp(&before) != stamp(&file.metadata()?) {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "timezone file changed while reading",
        ));
    }
    Ok(bytes)
}

#[cfg(target_os = "macos")]
fn zone_file(path: &str) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    match read_zone_file(path) {
        Ok(bytes) => Ok(bytes),
        // A missing candidate is normal when parsing a textual POSIX TZ rule.
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::NotADirectory
                    | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            Err(error.into())
        }
        // tz-rs may discard lookup errors; never let a rejected file reach the
        // legacy stat fallback, whose timezone reader has no such bounds.
        Err(error) => die(&format!("cannot read timezone file: {error}")),
    }
}

#[cfg(target_os = "macos")]
fn date_zone(value: Option<&std::ffi::OsStr>) -> Option<tz::TimeZone> {
    let settings = tz::TimeZoneSettings::new(tz::TimeZoneSettings::DEFAULT_DIRECTORIES, zone_file);
    match value {
        None => settings.parse_local().ok(),
        Some(value) if value.is_empty() => Some(tz::TimeZone::utc()),
        Some(value) => settings.parse_posix_tz(value.to_str()?).ok(),
    }
}

#[cfg(target_os = "macos")]
fn date_in_zone(secs: i64, zone: &tz::TimeZone) -> Option<String> {
    let date = tz::DateTime::from_timespec(secs, 0, zone.as_ref()).ok()?;
    Some(format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        date.month(),
        date.month_day()
    ))
}

pub(crate) fn birth_key(path: &Path) -> i64 {
    birth_secs(path).unwrap_or(0)
}

pub(crate) fn swatch_cells(scheme: &[String]) -> (String, usize) {
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
    // CACHED key: `sort_by_key` recomputes it about twice per COMPARISON,
    // which is ~2·n·log₂n reads of the same n birth times.
    files.sort_by_cached_key(|f| std::cmp::Reverse(birth_key(f)));
    let total = files.len();
    let shown = if list_n > 0 && total > list_n {
        list_n
    } else {
        total
    };
    let rows: Vec<PathBuf> = files.into_iter().take(shown).collect();
    let schemes = backfill_schemes(cfg, rows.iter());
    let sources =
        verbose.then(|| wall_sources(&rows.iter().map(PathBuf::as_path).collect::<Vec<_>>()));

    let cols = columns();
    let stacked = cols < if verbose { 87 } else { 44 };
    let indent = if cols < 16 { "" } else { "  " };
    let continuation = if cols < 16 { "" } else { "    " };
    let pv_ok = verbose && !stacked && cols >= 96 && in_kitty() && have("kitten");
    let pvw = if pv_ok { 9 } else { 0 };
    let mut namew = if verbose {
        cols.saturating_sub(71 + pvw)
    } else {
        cols.saturating_sub(32)
    };
    namew = namew.clamp(16, 44);

    println!("wallpapers\n");
    if stacked {
        for line in crate::ui::wrap_prefixed("TITLE / COLORSCHEME", cols, indent, indent) {
            println!("{line}");
        }
    } else if verbose {
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
        if stacked {
            let width = cols.saturating_sub(2).max(1);
            println!("  {}", truncate_ellipsis(&display_text(stem), width));
            let colors = schemes.get(f).map(Vec::as_slice).unwrap_or_default();
            if colors.is_empty() {
                println!("  -");
            } else {
                for row in colors.chunks((width / 3).clamp(1, 8)) {
                    println!("  {}", swatch_cells(row).0);
                }
            }
            if verbose {
                let source = sources
                    .as_ref()
                    .and_then(|s| s.get(f))
                    .map(String::as_str)
                    .unwrap_or("-");
                for (label, value) in [
                    ("SOURCE", source.to_string()),
                    (
                        "FORMAT",
                        f.extension()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_string(),
                    ),
                    ("SIZE", human_bytes(f)),
                    ("ADDED", added_date(f)),
                ] {
                    for line in crate::ui::wrap_prefixed(
                        &format!("{label}  {}", display_text(&value)),
                        cols,
                        indent,
                        continuation,
                    ) {
                        println!("{line}");
                    }
                }
            }
            println!();
            continue;
        }
        // Sanitize BEFORE measuring: a stripped byte must not count against
        // the column, and the row must never carry disk bytes as protocol.
        let name = crate::ui::pad_cells(&display_text(stem), namew);
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
        print!("  {name}  ");
        let (sw, n) = schemes.get(f).map(|s| swatch_cells(s)).unwrap_or_default();
        print!("{sw}");
        if n == 0 {
            print!("{:<24}", "-");
        } else if verbose {
            for _ in n..8 {
                print!("   ");
            }
        }
        if verbose {
            let src = sources
                .as_ref()
                .and_then(|sources| sources.get(f))
                .cloned()
                .unwrap_or_else(|| "-".into());
            let src = crate::ui::pad_cells(&src, 10);
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
        println!();
        for line in crate::ui::wrap_prefixed(
            &format!("newest {shown} of {total} — more: theme list -n <count>, or --all"),
            cols,
            indent,
            indent,
        ) {
            println!("{line}");
        }
    }
}

fn preview_dimensions(
    path: &Path,
    prepared: Result<&crate::presentation::PreparedPalette, &String>,
) -> String {
    match prepared {
        // Use the same validated source snapshot as the displayed palette.
        // Content-sniffed dimensions also work for mislabeled image files.
        Ok(p) => format!("{}x{}", p.profile.width, p.profile.height),
        Err(_) => img_size(path),
    }
}

pub fn cmd_preview(cfg: &Config, arg: Option<&str>, verbose: bool) {
    use std::fmt::Write as _;
    let img: PathBuf = match arg {
        Some(a) => resolve_local(cfg, a).unwrap_or_else(|| {
            die(&format!(
                "no wallpaper uniquely matching '{a}' (looked in {})",
                cfg.wallpaper_dirs_display
            ))
        }),
        None => match wallpaper_to_print(cfg, crate::apply::desktop_cache(cfg).as_ref())
            .filter(|p| p.is_file())
        {
            Some(p) => p,
            None => die("no current wallpaper to preview — name one: theme preview <wallpaper>"),
        },
    };
    let prepared = crate::presentation::cached_preview(cfg, &img);
    let name = display_text(img.file_stem().and_then(|s| s.to_str()).unwrap_or(""));
    let cols = columns();
    let mut fields = vec![("TITLE", name)];
    // Metadata remains available explicitly, without slowing ordinary previews
    // with source lookups or crowding the picture and its final colors.
    if verbose {
        for (label, key) in [
            ("ARTIST", "theme.artist"),
            ("PUBLISHED", "theme.published"),
            ("CAMERA", "theme.camera"),
            ("PLACE", "theme.place"),
            ("LICENSE", "theme.license"),
        ] {
            let value = wall_meta(&img, key);
            if !value.is_empty() {
                fields.push((label, value));
            }
        }
        let src = wall_source(&img);
        if src != "-" {
            fields.push(("SOURCE", src));
        }
        if let Some(ext) = img.extension().and_then(|e| e.to_str()) {
            fields.push(("FORMAT", ext.to_lowercase()));
        }
        let dims = preview_dimensions(&img, prepared.as_ref());
        let dims = if dims.is_empty() { "?" } else { &dims };
        fields.push(("SIZE", format!("{dims} ({})", human_bytes(&img))));
        let mut loc = img.display().to_string();
        if let Ok(home) = std::env::var("HOME")
            && let Some(rest) = loc.strip_prefix(&home)
            && rest.starts_with('/')
        {
            loc = format!("~{rest}");
        }
        fields.push(("LOCATION", display_text(&loc)));
    }

    // One buffered frame: intact thumbnail rows above the name and swatches.
    let mut frame = String::with_capacity(4096);
    let pw = cols.saturating_sub(2).min(24);
    if pw >= 8
        && let Some(p) = render_preview(&img, pw, 10)
    {
        write!(frame, "{}", p.apc).unwrap();
        for r in &p.rows {
            writeln!(frame, "  {r}").unwrap();
        }
        writeln!(frame).unwrap();
    } else if let Some(message) = preview_failure(pw) {
        for line in crate::ui::wrap_prefixed(message, cols, "  theme: ", "    ") {
            writeln!(frame, "{line}").unwrap();
        }
    }
    for (label, value) in &fields {
        for line in wrap_field(label, value, cols) {
            writeln!(frame, "{line}").unwrap();
        }
    }
    match prepared {
        Ok(p) => {
            for line in crate::ui::wrap_prefixed("COLORSCHEME", cols, "  ", "  ") {
                writeln!(frame, "{line}").unwrap();
            }
            for line in p.swatches(cols.saturating_sub(2)).lines() {
                writeln!(frame, "  {line}").unwrap();
            }
        }
        Err(e) => {
            for line in wrap_field(
                "COLORSCHEME",
                &format!("unavailable: {}", display_text(&e)),
                cols,
            ) {
                writeln!(frame, "{line}").unwrap();
            }
        }
    }
    print!("{frame}");
}

/// One metadata line, wrapped at the terminal edge with a hanging indent to
/// the value column — the invariant is that a wrapped value can never
/// visually merge with the next label's line. Narrower than the value
/// column + a 12-character window, the label takes its own line and the
/// value wraps indented beneath it (issue #19) — never at column 0.
fn wrap_field(label: &str, value: &str, cols: usize) -> Vec<String> {
    if cols < 15 + 12 {
        let mut out = vec![format!("  {label}")];
        out.extend(crate::ui::wrap_prefixed(value, cols, "    ", "    "));
        return out;
    }
    let width = cols.saturating_sub(15).max(12);
    let chars: Vec<char> = value.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() || i == 0 {
        let chunk: String = chars[i..(i + width).min(chars.len())].iter().collect();
        if i == 0 {
            out.push(format!("  {label:<12} {chunk}"));
        } else {
            out.push(format!("  {:<12} {chunk}", ""));
        }
        i += width;
    }
    out
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
    let desk = wallpaper_to_print(cfg, crate::apply::desktop_cache(cfg).as_ref())
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
    // Long values wrap with a hanging indent to the 17-column value column
    // (issue #19) — a continuation never lands at column 0 — and the
    // swatch row renders only as many swatches as the width holds.
    let cols = columns();
    let vwrap = |first: &str, value: &str| {
        let cont = " ".repeat(first.chars().count());
        for line in crate::ui::wrap_prefixed(value, cols, first, &cont) {
            println!("{line}");
        }
    };
    vwrap("current theme:   ", &shown);
    println!("mode:            {mode}");
    let colors = scheme_colors(cfg);
    if colors.is_empty() {
        println!("color scheme:    <none>");
    } else if cols >= 17 + 32 {
        println!("color scheme:    {}", crate::ui::swatch_row(&colors));
    } else {
        let n = (cols.saturating_sub(17) / 4).max(1);
        println!(
            "color scheme:    {}",
            crate::ui::swatch_row(&colors[..n.min(colors.len())])
        );
    }
    vwrap(
        "palette source:  ",
        if inc_d.is_empty() { "<none>" } else { &inc_d },
    );
    let cur_path = Path::new(&current);
    let size_note = if !current.is_empty() && cur_path.is_file() {
        format!(" ({})", img_size(cur_path))
    } else {
        String::new()
    };
    vwrap(
        "palette image:   ",
        &format!(
            "{}{size_note}",
            if current_d.is_empty() {
                "<none>"
            } else {
                &current_d
            }
        ),
    );
    vwrap(
        "wallpaper dir:   ",
        &format!(
            "{} ({} images)",
            display_text(&cfg.wallpaper_dirs_display),
            all_images(cfg).len()
        ),
    );
    println!("variables:");
    let key_state = if std::env::var("UNSPLASH_ACCESS_KEY")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        "set (env)"
    } else if crate::unsplash::keychain_read("unsplash-access-key").is_some() {
        "set (Keychain: unsplash-access-key)"
    } else {
        "not set (theme unsplash --help)"
    };
    vwrap("  UNSPLASH_ACCESS_KEY   ", key_state);
    vwrap(
        "  THEME_WALLPAPER_DIR   ",
        &display_text(&cfg.wallpaper_dirs_display),
    );
    vwrap(
        "  THEME_FORMATS         ",
        &display_text(&cfg.formats_display()),
    );
    vwrap(
        "  THEME_CONTRAST        ",
        &display_text(&std::env::var("THEME_CONTRAST").unwrap_or_else(|_| "4.5".into())),
    );
    vwrap(
        "  THEME_CACHE_DIR       ",
        &display_text(&cfg.cache_dir.display().to_string()),
    );
}

#[cfg(test)]
mod preview_dimension_tests {
    use super::*;
    use crate::presentation::prepare;

    fn fixture(name: &str) -> PathBuf {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("preview-dimensions-{}-{name}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_image(path: &Path, format: image::ImageFormat, width: u32, height: u32) {
        image::save_buffer_with_format(
            path,
            &[45, 90, 140].repeat((width * height) as usize),
            width,
            height,
            image::ColorType::Rgb8,
            format,
        )
        .unwrap();
    }

    #[test]
    fn prepared_dimensions_match_original_headers_and_keep_error_fallback() {
        let root = fixture("formats");
        for (format, extension, width, height) in [
            (image::ImageFormat::Png, "png", 37, 19),
            (image::ImageFormat::Jpeg, "jpg", 259, 131),
            (image::ImageFormat::WebP, "webp", 131, 257),
        ] {
            let path = root.join(format!("sample.{extension}"));
            write_image(&path, format, width, height);
            let opts = pigment::Options::default();
            pigment::cached_derive(&path, &opts, &root.join("cache")).unwrap();
            let raw = pigment::read_cached(&path, &opts, &root.join("cache"))
                .unwrap()
                .unwrap();
            let failed = prepare(raw.clone(), f64::NAN, 4.5);
            let prepared = prepare(raw, 1.0, 4.5);
            let expected = format!("{width}x{height}");
            assert_eq!(img_size(&path), expected);
            assert_eq!(preview_dimensions(&path, prepared.as_ref()), expected);
            assert!(failed.is_err());
            assert_eq!(preview_dimensions(&path, failed.as_ref()), expected);
            // A valid prepared snapshot does not reopen the image. The error
            // path still probes it and retains the existing unknown result.
            fs::remove_file(&path).unwrap();
            assert_eq!(preview_dimensions(&path, prepared.as_ref()), expected);
            assert_eq!(preview_dimensions(&path, failed.as_ref()), "");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prepared_dimensions_follow_content_for_mislabeled_and_extensionless_files() {
        let root = fixture("content");
        for (format, name) in [
            (image::ImageFormat::Png, "png-bytes.jpg"),
            (image::ImageFormat::Jpeg, "jpeg-bytes.png"),
            (image::ImageFormat::WebP, "webp-bytes.jpg"),
            (image::ImageFormat::Png, "extensionless"),
        ] {
            let path = root.join(name);
            write_image(&path, format, 139, 73);
            let prepared = prepare(
                pigment::derive(&path, &pigment::Options::default()).unwrap(),
                1.0,
                4.5,
            );
            assert_eq!(img_size(&path), "");
            assert_eq!(preview_dimensions(&path, prepared.as_ref()), "139x73");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn changed_source_gets_new_dimensions_without_changing_the_old_snapshot() {
        let root = fixture("changed");
        let path = root.join("sample.png");
        let cache = root.join("cache");
        let opts = pigment::Options::default();
        write_image(&path, image::ImageFormat::Png, 37, 19);
        let first = prepare(
            pigment::cached_derive(&path, &opts, &cache).unwrap(),
            1.0,
            4.5,
        );
        write_image(&path, image::ImageFormat::Png, 71, 43);
        let second = prepare(
            pigment::cached_derive(&path, &opts, &cache).unwrap(),
            1.0,
            4.5,
        );
        assert_eq!(img_size(&path), "71x43");
        assert_eq!(preview_dimensions(&path, first.as_ref()), "37x19");
        assert_eq!(preview_dimensions(&path, second.as_ref()), "71x43");
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(all(test, target_os = "macos"))]
mod date_tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn local_dates_match_bsd_across_zones_dst_and_midnight() {
        let path = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("date-parity-{}", std::process::id()));
        std::fs::write(&path, b"date parity fixture").unwrap();
        let birth = birth_secs(&path).unwrap();
        // Epoch, local New Year, both 2024 DST transitions, and summer midnight.
        let timestamps = [
            -1,
            0,
            1_704_085_199,
            1_704_085_200,
            1_710_053_999,
            1_710_054_000,
            1_730_613_599,
            1_730_613_600,
            1_719_806_399,
            1_719_806_400,
        ];
        for value in [
            None,
            Some(""),
            Some("UTC"),
            Some("America/New_York"),
            Some(":America/New_York"),
            Some("/usr/share/zoneinfo/America/New_York"),
            Some(":/usr/share/zoneinfo/America/New_York"),
            Some("EST5EDT,M3.2.0,M11.1.0"),
        ] {
            let zone = date_zone(value.map(OsStr::new)).unwrap();
            let command = |program: &str| {
                let mut command = Command::new(program);
                command.env_remove("TZ");
                if let Some(value) = value {
                    command.env("TZ", value);
                }
                command
            };
            for secs in timestamps {
                let expected = command("/bin/date")
                    .args(["-r", &secs.to_string(), "+%Y-%m-%d"])
                    .output()
                    .unwrap();
                assert!(expected.status.success());
                assert_eq!(
                    date_in_zone(secs, &zone).unwrap(),
                    String::from_utf8(expected.stdout).unwrap().trim(),
                    "TZ={value:?}, timestamp={secs}"
                );
            }
            let expected = command("/usr/bin/stat")
                .args(["-f", "%SB", "-t", "%Y-%m-%d"])
                .arg(&path)
                .output()
                .unwrap();
            assert!(expected.status.success());
            assert_eq!(
                date_in_zone(birth, &zone).unwrap(),
                String::from_utf8(expected.stdout).unwrap().trim(),
                "TZ={value:?}"
            );
        }
        let expected = Command::new("/usr/bin/stat")
            .args(["-f", "%SB", "-t", "%Y-%m-%d"])
            .arg(&path)
            .output()
            .unwrap();
        assert_eq!(
            added_date(&path),
            String::from_utf8(expected.stdout).unwrap().trim()
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn unsupported_zone_selects_existing_stat_fallback() {
        assert!(date_zone(Some(OsStr::new("not-a-timezone"))).is_none());
        use std::os::unix::ffi::OsStrExt;
        assert!(date_zone(Some(OsStr::from_bytes(b"\xff"))).is_none());
    }

    #[test]
    fn changed_private_timezone_refreshes_cached_date() {
        let dir = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("date-zone-refresh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("zone");
        std::fs::write(
            &path,
            read_zone_file("/usr/share/zoneinfo/America/New_York").unwrap(),
        )
        .unwrap();
        let mut cache = DateZoneCache::default();
        assert_eq!(
            date_in_zone(0, cache.get(Some(path.as_os_str())).unwrap()).unwrap(),
            "1969-12-31"
        );
        let before = cache.fingerprint.clone();
        std::fs::write(&path, read_zone_file("/usr/share/zoneinfo/UTC").unwrap()).unwrap();
        assert_eq!(
            date_in_zone(0, cache.get(Some(path.as_os_str())).unwrap()).unwrap(),
            "1970-01-01"
        );
        assert_ne!(before, cache.fingerprint);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn timezone_reader_is_bounded_and_requires_a_regular_file() {
        let dir = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("date-zone-reader-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("zone");
        std::fs::write(&path, b"small ordinary file").unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert_eq!(
            read_zone_file(link.to_str().unwrap()).unwrap(),
            b"small ordinary file"
        );
        assert_eq!(
            read_zone_file(dir.to_str().unwrap()).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(1024 * 1024 + 1)
            .unwrap();
        assert_eq!(
            read_zone_file(path.to_str().unwrap()).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod helper_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn source_helper_child() {
        let Some(image) = std::env::var_os("THEME_TEST_SOURCE_IMAGE") else {
            return;
        };
        let path = std::env::var_os("PATH");
        let absent = mdls_absent(path.as_deref());
        assert_eq!(
            absent,
            std::env::var("THEME_TEST_MDLS_ABSENT").unwrap() == "1"
        );
        let original = wall_source_with_mdls(Path::new(&image), || true);
        let optimized = wall_source_with_mdls(Path::new(&image), || !absent);
        assert_eq!(optimized, original);
        assert_eq!(optimized, std::env::var("THEME_TEST_SOURCE_LABEL").unwrap());
    }

    #[test]
    fn helper_absence_preserves_present_ambiguous_and_empty_path_behavior() {
        assert!(!mdls_absent(None)); // exec's default PATH is not absence.
        let dir = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("source-helper-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let image = dir.join("image.png");
        fs::write(&image, b"source fixture").unwrap();
        for name in ["missing", "present", "directory", "dangling", "loop"] {
            fs::create_dir(dir.join(name)).unwrap();
        }
        let helper = dir.join("present/mdls");
        fs::write(
            &helper,
            b"#!/bin/sh\nprintf '(\\n    \"https://images.unsplash.com/fixture\"\\n)\\n'\n",
        )
        .unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir(dir.join("directory/mdls")).unwrap();
        std::os::unix::fs::symlink("absent", dir.join("dangling/mdls")).unwrap();
        std::os::unix::fs::symlink("mdls", dir.join("loop/mdls")).unwrap();
        std::os::unix::fs::symlink("ancestor-loop", dir.join("ancestor-loop")).unwrap();
        assert!(fs::symlink_metadata(dir.join("ancestor-loop/mdls")).is_err());
        let missing = dir.join("missing");
        let present = dir.join("present");
        for (path, cwd, absent, label) in [
            (missing.as_os_str().to_owned(), &dir, true, "-"),
            (image.as_os_str().to_owned(), &dir, true, "-"),
            (
                std::env::join_paths([&missing, &present]).unwrap(),
                &dir,
                false,
                "unsplash",
            ),
            (std::ffi::OsString::new(), &present, false, "unsplash"),
            (dir.join("directory").into_os_string(), &dir, false, "-"),
            (dir.join("dangling").into_os_string(), &dir, false, "-"),
            (dir.join("loop").into_os_string(), &dir, false, "-"),
            (dir.join("ancestor-loop").into_os_string(), &dir, false, "-"),
        ] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "report::helper_tests::source_helper_child",
                    "--nocapture",
                ])
                .current_dir(cwd)
                .env("PATH", path)
                .env("THEME_TEST_SOURCE_IMAGE", &image)
                .env("THEME_TEST_MDLS_ABSENT", if absent { "1" } else { "0" })
                .env("THEME_TEST_SOURCE_LABEL", label)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(all(test, target_os = "macos"))]
mod source_tests {
    use super::*;

    #[test]
    fn batch_preserves_record_order_and_first_url_semantics() {
        let first = b"(\n    \"https://images.unsplash.com/a.jpg\",\n    \"https://example.org/ignored\"\n)";
        let second = b"(null)";
        let third = b"(\n    \"https://www.example.org/a%20picture.png\"\n)";
        let raw = [first.as_slice(), second.as_slice(), third.as_slice()].join(&0);
        assert_eq!(
            parse_mdls_batch(&raw, 3, true).unwrap(),
            ["unsplash", "-", "example.org"]
        );
        assert_eq!(mdls_source(first), "https://images.unsplash.com/a.jpg");
        assert_eq!(
            parse_mdls_batch(&raw, 3, true).unwrap(),
            [first.as_slice(), second.as_slice(), third.as_slice()]
                .map(|field| source_label(&mdls_source(field)))
        );
    }

    #[test]
    fn failed_incomplete_extra_and_oversized_batches_select_fallback() {
        let raw = b"(null)\0(null)";
        assert!(parse_mdls_batch(raw, 2, false).is_none());
        assert!(parse_mdls_batch(raw, 3, true).is_none());
        assert!(parse_mdls_batch(raw, 1, true).is_none());
        assert!(parse_mdls_batch(b"", 0, true).is_none());
        assert!(parse_mdls_batch(&vec![b'x'; MDLS_BYTES + 1], 1, true).is_none());
        assert!(parse_mdls_batch(&[b"x".as_slice(); 33].join(&0), 33, true).is_none());
    }

    #[test]
    fn batch_keeps_xattr_sources_and_filenames_with_spaces() {
        let dir = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("source-batch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = [dir.join("one image.png"), dir.join("two # image.png")];
        for (path, source) in paths.iter().zip([
            "https://images.unsplash.com/one",
            "https://www.example.org/two",
        ]) {
            std::fs::write(path, b"source fixture").unwrap();
            rustix::fs::setxattr(
                path,
                "theme.source",
                source.as_bytes(),
                rustix::fs::XattrFlags::empty(),
            )
            .unwrap();
        }
        let got = wall_sources(&paths.iter().map(PathBuf::as_path).collect::<Vec<_>>());
        assert_eq!(got[&paths[0]], "unsplash");
        assert_eq!(got[&paths[1]], "example.org");
        for path in &paths {
            assert_eq!(
                wall_source_with_mdls(path, || panic!("xattr source probed helper availability")),
                got[path]
            );
        }
        assert_eq!(got.len(), 2);
        assert!(wall_sources(&[]).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

//! Every message this tool prints passes through `die`/`note` (or an explicit
//! `display_text` copy at printf-style sinks), so control bytes stop here —
//! not at whichever call sites someone remembered. Filenames, cached records
//! and API contributor text can all carry an OSC 52 clipboard write; a
//! sanitized display copy is what may be shown, while the operational value
//! stays byte-exact for everything we open, copy, move or delete.

use rustix::termios::Termios;
use std::process::exit;
use std::sync::Mutex;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Strip control bytes (0x00-0x1f, 0x7f) from a display copy. Byte-level,
/// exactly like `tr -d '[:cntrl:]'` in the C locale: UTF-8 continuation
/// bytes are untouched.
pub fn display_text(s: &str) -> String {
    s.chars().filter(|c| !c.is_ascii_control()).collect()
}

/// Print `theme: <msg>` to stderr and exit 1. The message passes through
/// [`display_text`] like every other sink, and registered scratch files are
/// swept first — `exit` runs no destructors, so the terminal mode is given
/// back here too.
pub fn die(msg: &str) -> ! {
    restore_terminal();
    crate::scratch::cleanup();
    eprintln!("theme: {}", display_text(msg));
    exit(1);
}

/// The terminal settings raw mode replaced, held for whichever exit path
/// runs first. `Drop` covers quit, EOF, Ctrl-C and an unwinding panic;
/// `die` and the broken-pipe exit run no destructors and call
/// [`restore_terminal`] themselves. A cooked terminal is the browser's
/// responsibility on every one of them: a shell left in raw mode echoes
/// nothing and needs `stty sane` from the user.
static COOKED: Mutex<Option<Termios>> = Mutex::new(None);

/// Raw mode on the terminal the caller is looking at, for a reader that
/// wants keys rather than lines. `None` when the terminal refuses — the
/// caller then reads what the line discipline gives it, and nothing was
/// changed. Output post-processing is deliberately kept: every page this
/// tool prints is `println!` text that must still end with CR LF.
pub fn raw_mode() -> Option<RawMode> {
    use rustix::termios::{OptionalActions, OutputModes, tcgetattr, tcsetattr};
    let terminal = std::io::stdin();
    let cooked = tcgetattr(&terminal).ok()?;
    let mut raw = cooked.clone();
    raw.make_raw();
    raw.output_modes |= OutputModes::OPOST | OutputModes::ONLCR;
    tcsetattr(&terminal, OptionalActions::Flush, &raw).ok()?;
    *COOKED.lock().unwrap_or_else(|e| e.into_inner()) = Some(cooked);
    Some(RawMode(()))
}

/// Put the terminal back the way it was found, once. Safe to call on any
/// exit path, in any order, including from a panic while the lock is held
/// (`try_lock`, like the scratch sweep).
pub fn restore_terminal() {
    use rustix::termios::{OptionalActions, tcsetattr};
    if let Ok(mut slot) = COOKED.try_lock()
        && let Some(cooked) = slot.take()
    {
        let _ = tcsetattr(std::io::stdin(), OptionalActions::Flush, &cooked);
    }
}

/// Held for as long as raw mode should last.
pub struct RawMode(());

impl Drop for RawMode {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// The print!/println! shims' writer (declared atop main.rs, #59): one
/// locked, line-buffered stdout write. A broken pipe — the reader has gone —
/// ends the process with 141, the shell's own status for a tool cut off by
/// its reader, after the scratch sweep every normal exit runs; any other
/// write error panics with std's own message, as before. Like `die`,
/// `exit` runs no destructors: nothing between `Paused::new` and `finish`
/// in spaces.rs prints, and nothing may start to — a print there could
/// leave the wallpaper agent stopped.
#[cfg(not(test))]
pub fn out(args: std::fmt::Arguments<'_>, end: &str) {
    use std::io::Write;
    let mut o = std::io::stdout().lock();
    let Err(e) = o.write_fmt(args).and_then(|()| o.write_all(end.as_bytes())) else {
        return;
    };
    if e.kind() == std::io::ErrorKind::BrokenPipe {
        drop(o);
        restore_terminal();
        crate::scratch::cleanup();
        exit(141);
    }
    panic!("failed printing to stdout: {e}");
}

/// The eprintln! shim's writer: unbuffered stderr, as std's. A
/// broken pipe loses the message and nothing else — the caller keeps its
/// own exit status, so `die` under `2>&1 | head` still exits 1; any other
/// write error panics with std's own message.
#[cfg(not(test))]
pub fn err(args: std::fmt::Arguments<'_>, end: &str) {
    use std::io::Write;
    let mut o = std::io::stderr().lock();
    let Err(e) = o.write_fmt(args).and_then(|()| o.write_all(end.as_bytes())) else {
        return;
    };
    if e.kind() != std::io::ErrorKind::BrokenPipe {
        panic!("failed printing to stderr: {e}");
    }
}

/// Print `theme: <msg>` to stdout, sanitized.
pub fn note(msg: &str) {
    println!("theme: {}", display_text(msg));
}

/// Render hex colors as truecolor background swatches, 8 per row, matching
/// the shell's `swatch_row` (3-cell blocks, continuation rows indented to
/// the status block's value column).
pub fn swatch_row(colors: &[String]) -> String {
    let mut out = String::new();
    let total = colors.len();
    for (i, c) in colors.iter().enumerate() {
        let c = c.trim_start_matches('#');
        let (r, g, b) = match parse_hex6(c) {
            Some(t) => t,
            None => continue,
        };
        out.push_str(&format!("\x1b[48;2;{r};{g};{b}m   \x1b[0m "));
        if (i + 1) % 8 == 0 && i + 1 < total {
            out.push_str("\n                 ");
        }
    }
    out
}

/// Terminal width, from the terminal ITSELF first (issue #21): the v0.2.1
/// narrow fix read only the COLUMNS env var, which zsh does not export —
/// so every real terminal fell to the wide default and the 42-column
/// owner window still tore. Order, per class:
///
/// 1. stdout is a tty → POSIX `tcgetwinsize` (identical on macOS/Linux,
///    any terminal emulator — the emulator's own answer).
/// 2. stdout is NOT a tty (pipe/file) → the layout belongs to the pipe,
///    not the invoking terminal: COLUMNS when the caller says so
///    (tests, `COLUMNS=… theme | less`), else a conservative 60 that
///    prefers the stacked shape — a pipe has no image worth defending,
///    and /dev/tty is deliberately NOT consulted.
pub fn term_cols() -> usize {
    if let Ok(ws) = rustix::termios::tcgetwinsize(std::io::stdout())
        && ws.ws_col > 0
    {
        return ws.ws_col as usize;
    }
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(60)
}

/// Terminal cells occupied by plain, sanitized text. ASCII filenames and
/// labels do not need the Unicode tables. Ambiguous/private-use glyphs occupy
/// one cell, matching normal terminal mode rather than the CJK-width variant.
pub fn display_width(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        UnicodeWidthStr::width(s)
    }
}

/// Byte boundary after the complete graphemes fitting in `cells` columns.
fn cell_prefix(s: &str, cells: usize) -> usize {
    if s.is_ascii() {
        return s.len().min(cells);
    }
    let (mut end, mut used) = (0, 0);
    for (start, grapheme) in s.grapheme_indices(true) {
        let width = display_width(grapheme);
        if used + width > cells {
            break;
        }
        used += width;
        end = start + grapheme.len();
    }
    end
}

/// Hard-wrap without a minimum width, preserving graphemes and whitespace.
/// A grapheme wider than the whole row becomes an ellipsis, so a one-cell
/// terminal cannot overflow or stall on a two-cell image title.
pub fn cell_chunks(mut s: &str, cells: usize) -> Vec<String> {
    let cells = cells.max(1);
    let mut out = Vec::new();
    while !s.is_empty() {
        let end = cell_prefix(s, cells);
        if end > 0 {
            out.push(s[..end].to_owned());
            s = &s[end..];
        } else {
            let grapheme = s.graphemes(true).next().unwrap();
            out.push("…".into());
            s = &s[grapheme.len()..];
        }
    }
    out
}

/// A plain-text table cell whose width is measured in terminal columns.
pub fn pad_cells(s: &str, cells: usize) -> String {
    let mut out = truncate_ellipsis(s, cells);
    out.push_str(&" ".repeat(cells.saturating_sub(display_width(&out))));
    out
}

/// Word-wrap PLAIN text (no escapes) so no emitted line exceeds `cols`
/// where geometry allows: the first line starts with `first`, every
/// continuation with `cont` — a continuation never lands at column 0. A
/// word wider than the window hard-splits. The window floors at 12
/// cells: below `prefix + 12` a line may exceed a hopeless terminal
/// instead of shredding into one-character columns (issue #19).
pub fn wrap_prefixed(text: &str, cols: usize, first: &str, cont: &str) -> Vec<String> {
    let win = |p: &str| cols.saturating_sub(display_width(p)).max(12);
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for mut word in text.split_whitespace() {
        loop {
            let w = if out.is_empty() {
                win(first)
            } else {
                win(cont)
            };
            let used = display_width(&cur);
            let sep = if cur.is_empty() { 0 } else { 1 };
            if used + sep + display_width(word) <= w {
                if sep == 1 {
                    cur.push(' ');
                }
                cur.push_str(word);
                break;
            }
            if cur.is_empty() {
                let take = cell_prefix(word, w);
                // The established 12-cell floor accommodates ordinary glyphs;
                // retain progress for any future wider grapheme as well.
                if take == 0 {
                    cur.push('…');
                    word = &word[word.graphemes(true).next().unwrap().len()..];
                } else {
                    cur.push_str(&word[..take]);
                    word = &word[take..];
                }
            }
            let pfx = if out.is_empty() { first } else { cont };
            out.push(format!("{pfx}{cur}"));
            cur.clear();
        }
    }
    let pfx = if out.is_empty() { first } else { cont };
    out.push(format!("{pfx}{cur}"));
    out
}

/// Parse exactly six hex digits into (r, g, b).
pub fn parse_hex6(s: &str) -> Option<(u8, u8, u8)> {
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some((
        u8::from_str_radix(&s[0..2], 16).ok()?,
        u8::from_str_radix(&s[2..4], 16).ok()?,
        u8::from_str_radix(&s[4..6], 16).ok()?,
    ))
}

/// Truncate to terminal cells, reserving one cell for the ellipsis. Combining
/// marks, emoji modifiers, and joined emoji stay with their whole grapheme.
pub fn truncate_ellipsis(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if display_width(s) > max {
        let mut t = s[..cell_prefix(s, max - 1)].to_owned();
        t.push('…');
        t
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_text_strips_osc_keeps_text() {
        let s = "osc52-safe\x1b]52;c;U0FGRQ==\x07.png";
        assert_eq!(display_text(s), "osc52-safe]52;c;U0FGRQ==.png");
        assert!(!display_text(s).contains('\x1b'));
    }

    #[test]
    fn display_text_keeps_utf8() {
        assert_eq!(display_text("héllo…"), "héllo…");
    }

    /// The narrow-render wrap (issue #19): words fill the window, every
    /// continuation carries its prefix (never column 0), an unbroken word
    /// wider than the window hard-splits, and no line exceeds the width
    /// while the window stays above its 12-character floor.
    #[test]
    fn wrapped_lines_fit_and_continuations_carry_their_prefix() {
        let out = wrap_prefixed("alpha beta gamma delta epsilon", 14, "* ", "~ ");
        assert_eq!(out, ["* alpha beta", "~ gamma delta", "~ epsilon"]);
        assert!(out.iter().all(|l| l.chars().count() <= 14));
        let out = wrap_prefixed("abcdefghijklmnopqrstuvwxyz", 14, "  ", "  ");
        assert_eq!(out, ["  abcdefghijkl", "  mnopqrstuvwx", "  yz"]);
        // Below prefix+12 the window floors at 12 rather than shredding.
        let out = wrap_prefixed("abcdefghijklmn", 6, "    ", "    ");
        assert_eq!(out, ["    abcdefghijkl", "    mn"]);
    }

    #[test]
    fn truncation_reserves_one_cell_and_preserves_graphemes() {
        assert_eq!(truncate_ellipsis("abcdef", 4), "abc…");
        assert_eq!(truncate_ellipsis("abc", 4), "abc");
        assert_eq!(truncate_ellipsis("中国山", 5), "中国…");
        assert_eq!(truncate_ellipsis("中国", 1), "…");
        assert_eq!(truncate_ellipsis("中国", 0), "");
        assert_eq!(truncate_ellipsis("e\u{301}xy", 2), "e\u{301}…");
        assert_eq!(truncate_ellipsis("👩‍💻xy", 3), "👩‍💻…");
        assert_eq!(truncate_ellipsis("👍🏽xy", 3), "👍🏽…");
        assert_eq!(truncate_ellipsis("🇺🇸xy", 3), "🇺🇸…");
    }

    #[test]
    fn widths_and_padding_match_terminal_cells() {
        for (text, expected) in [
            ("plain", 5),
            ("中国", 4),
            ("e\u{301}", 1),
            ("👩‍💻", 2),
            ("👍🏽", 2),
            ("🇺🇸", 2),
            ("\u{f120}", 1),
        ] {
            assert_eq!(display_width(text), expected, "{text:?}");
        }
        assert_eq!(pad_cells("中国", 7), "中国   ");
        assert_eq!(pad_cells("e\u{301}", 3), "e\u{301}  ");
        assert_eq!(pad_cells("\u{f120}", 3), "\u{f120}  ");
        assert_eq!(pad_cells("👩‍💻", 1), "…");
    }

    #[test]
    fn narrow_chunks_never_split_or_overflow_wide_graphemes() {
        assert_eq!(cell_chunks("中国", 1), ["…", "…"]);
        assert_eq!(cell_chunks("e\u{301}👩‍💻x", 2), ["e\u{301}", "👩‍💻", "x"]);
        assert_eq!(cell_chunks("🇺🇸👍🏽", 2), ["🇺🇸", "👍🏽"]);
        assert_eq!(cell_chunks("abcdef", 3), ["abc", "def"]);
        for cols in [1, 8, 13, 25] {
            let text = "中国山水 e\u{301}toile 👩‍💻 👍🏽 🇺🇸 \u{f120}";
            assert!(
                cell_chunks(text, cols)
                    .iter()
                    .all(|s| display_width(s) <= cols)
            );
        }
    }

    #[test]
    fn word_wrap_counts_wide_prefixes_and_unbroken_names() {
        assert_eq!(
            wrap_prefixed("中国中国中国中国中国中国中国", 15, "界 ", "界 "),
            ["界 中国中国中国", "界 中国中国中国", "界 中国"]
        );
        let lines = wrap_prefixed("中国山水中国山水中国山水中国山水中国山水.png", 25, "", "  ");
        assert!(lines.iter().all(|s| display_width(s) <= 25));
    }
}

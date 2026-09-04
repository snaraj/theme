//! Every message this tool prints passes through `die`/`note` (or an explicit
//! `display_text` copy at printf-style sinks), so control bytes stop here —
//! not at whichever call sites someone remembered. Filenames, cached records
//! and API contributor text can all carry an OSC 52 clipboard write; a
//! sanitized display copy is what may be shown, while the operational value
//! stays byte-exact for everything we open, copy, move or delete.

use std::process::exit;

/// Strip control bytes (0x00-0x1f, 0x7f) from a display copy. Byte-level,
/// exactly like `tr -d '[:cntrl:]'` in the C locale: UTF-8 continuation
/// bytes are untouched.
pub fn display_text(s: &str) -> String {
    s.chars().filter(|c| !c.is_ascii_control()).collect()
}

/// Print `theme: <msg>` to stderr and exit 1. The message passes through
/// [`display_text`] like every other sink, and registered scratch files are
/// swept first — `exit` runs no destructors.
pub fn die(msg: &str) -> ! {
    crate::scratch::cleanup();
    eprintln!("theme: {}", display_text(msg));
    exit(1);
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

/// Word-wrap PLAIN text (no escapes) so no emitted line exceeds `cols`
/// where geometry allows: the first line starts with `first`, every
/// continuation with `cont` — a continuation never lands at column 0. A
/// word wider than the window hard-splits. The window floors at 12
/// characters: below `prefix + 12` a line may exceed a hopeless terminal
/// instead of shredding into one-character columns (issue #19).
pub fn wrap_prefixed(text: &str, cols: usize, first: &str, cont: &str) -> Vec<String> {
    let win = |p: &str| cols.saturating_sub(p.chars().count()).max(12);
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let mut chars: Vec<char> = word.chars().collect();
        loop {
            let w = if out.is_empty() {
                win(first)
            } else {
                win(cont)
            };
            let used = cur.chars().count();
            let sep = if cur.is_empty() { 0 } else { 1 };
            if used + sep + chars.len() <= w {
                if sep == 1 {
                    cur.push(' ');
                }
                cur.extend(chars.iter());
                break;
            }
            if cur.is_empty() {
                let take = w.min(chars.len());
                cur.extend(chars.drain(..take));
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

/// Truncate a display string to `max` characters, appending `…` when cut —
/// the shell's `printf '%.*s…'` shape (character-based, like bash ${#s}).
pub fn truncate_ellipsis(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() > max {
        let mut t: String = chars[..max.saturating_sub(1)].iter().collect();
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
    fn truncation_is_character_based() {
        assert_eq!(truncate_ellipsis("abcdef", 4), "abc…");
        assert_eq!(truncate_ellipsis("abc", 4), "abc");
    }
}

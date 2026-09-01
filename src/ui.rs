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

    #[test]
    fn truncation_is_character_based() {
        assert_eq!(truncate_ellipsis("abcdef", 4), "abc…");
        assert_eq!(truncate_ellipsis("abc", 4), "abc");
    }
}

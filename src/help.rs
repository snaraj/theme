//! `theme help` and the per-command texts. The header prints LIVE values —
//! desktop, scheme, terminal, OS — so each one gets its own display copy;
//! the kitty branch's OSC 8 hyperlink is the ONE trusted exception, our own
//! literal. Keep help generic and extensible, and never let it grow:
//! additions pay for themselves by consolidating something else.

use crate::apply::wallpaper_get;
use crate::config::Config;
use crate::report::{include_line, render_preview, scheme_colors};
use crate::ui::{display_text, swatch_row, truncate_ellipsis, wrap_prefixed};
use std::path::Path;
use std::process::Command;

fn columns() -> usize {
    crate::ui::term_cols()
}

/// Bare-screen header geometry (issue #19): the image sits beside the live
/// fields ONLY when the whole side-by-side row provably fits — 2 indent +
/// image + 2 gap + 13 label column + the 32-cell swatch row. Anything
/// narrower stacks: thumbnail above (clamped to the terminal, DROPPED below
/// an 8-cell floor rather than torn), fields below as a label line with the
/// value indented beneath, truncated/count-fitted to the width. A value
/// never lands at column 0 and never interleaves an image row.
const IMG_COLS: usize = 14;
const IMG_ROWS: usize = 6;
const IMG_FLOOR: usize = 8;
const SIDE_MIN: usize = 2 + IMG_COLS + 2 + 13 + 32;

/// The command table as data so one renderer owns the width discipline —
/// same text as before, no growth (the help doctrine holds).
const SECTIONS: &[(&str, &[(&str, &str)])] = &[
    (
        "Apply Commands:",
        &[
            (
                "set",
                "apply a wallpaper: local name/path, or any actionable link",
            ),
            (
                "random",
                "random wallpaper from the configured wallpaper folder",
            ),
            (
                "unsplash",
                "Unsplash photos: search, page-url, random; auth and status",
            ),
            (
                "url",
                "download a direct image URL or Pinterest pin, save it, apply it",
            ),
        ],
    ),
    (
        "Library Commands:",
        &[
            (
                "list, ls",
                "wallpaper table: title + colorscheme (-v adds source, format, size, date)",
            ),
            (
                "preview",
                "one wallpaper up close: picture, colorscheme, title, location",
            ),
            (
                "rename",
                "rename a saved wallpaper, keeping the naming format",
            ),
            ("remove, rm", "delete saved wallpapers by name"),
        ],
    ),
    (
        "Info Commands:",
        &[
            ("status", "current theme, color-scheme swatches, variables"),
            (
                "update",
                "replace this binary with the latest release (verified)",
            ),
            ("version, -V", "version, repository, and maintainer"),
            ("help", "this text (per-command: theme <command> --help)"),
        ],
    ),
];

const FLAGS: &[(&str, &str)] = &[
    ("--rotate left|right", "turn the image 90° before applying"),
    (
        "--extend[=RRGGBB]",
        "centre flat art on a matching canvas (default 000000)",
    ),
    (
        "--desktop-only",
        "set the desktop wallpaper only; terminal colors stay",
    ),
];

/// One key/description table: two columns with the description wrapped to a
/// hanging indent while it keeps a real window; below that, the key on its
/// own line and the description indented beneath — never at column 0.
fn print_table(cols: usize, items: &[(&str, &str)], keyw: usize) {
    for (k, d) in items {
        if cols >= 2 + keyw + 2 + 20 {
            let first = format!("  {k:<keyw$}  ");
            let cont = " ".repeat(2 + keyw + 2);
            for l in wrap_prefixed(d, cols, &first, &cont) {
                println!("{l}");
            }
        } else {
            println!("  {k}");
            for l in wrap_prefixed(d, cols, "      ", "      ") {
                println!("{l}");
            }
        }
    }
}

fn cmd_out(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

pub fn usage(cfg: &Config) {
    let desk = wallpaper_get();
    let inc = include_line(cfg);
    let label = if inc.is_empty() || inc.ends_with("colors-kitty.conf") {
        String::new()
    } else {
        Path::new(&inc)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .trim_end_matches(".conf")
            .to_string()
    };
    let colors = scheme_colors(cfg);
    let eight: Vec<String> = colors.iter().take(8).cloned().collect();
    let in_kitty = std::env::var("KITTY_WINDOW_ID")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let term = if in_kitty {
        // Our own literal OSC 8 hyperlink — assigned WITHOUT display_text,
        // which would strip the very sequence it exists to emit.
        "\x1b]8;;https://sw.kovidgoyal.net/kitty/\x07kitty\x1b]8;;\x07".to_string()
    } else {
        // TERM_PROGRAM and TERM are environment data — either can carry an
        // OSC 52 write, and this line prints it as a fact.
        let raw = std::env::var("TERM_PROGRAM")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| std::env::var("TERM").ok())
            .unwrap_or_default();
        display_text(&raw)
    };
    // uname/sw_vers resolve through PATH, so their output is not ours to
    // trust either.
    let os = if cmd_out("uname", &["-s"]) == "Darwin" {
        format!(
            "macOS {} ({})",
            cmd_out("sw_vers", &["-productVersion"]),
            cmd_out("uname", &["-m"])
        )
    } else {
        cmd_out("uname", &["-srm"])
    };
    let os = display_text(&os);
    let name = match &desk {
        Some(d) => display_text(d.file_stem().and_then(|s| s.to_str()).unwrap_or("")),
        None => "<none>".into(),
    };
    // The title line opens every bare screen — the same compile-time
    // version the `version` subcommand reports.
    println!("theme v{}", env!("CARGO_PKG_VERSION"));
    let cols = columns();
    let desk_file = desk.as_deref().filter(|d| d.is_file());
    if cols >= SIDE_MIN {
        let sw = if eight.is_empty() {
            "<none>".to_string()
        } else {
            // The scheme-name tail rides along only when it provably fits
            // beside the 32 swatch cells.
            let tail = display_text(&label);
            let tail = if !tail.is_empty() && cols >= SIDE_MIN + 1 + tail.chars().count() {
                format!(" {tail}")
            } else {
                String::new()
            };
            format!("{}{tail}", swatch_row(&eight))
        };
        let availw = cols.saturating_sub(18 + 13).max(12);
        let name = truncate_ellipsis(&name, availw);
        let rlines = [
            String::new(),
            format!("{:<12} {}", "THEME", name),
            format!("{:<12} {}", "COLORSCHEME", sw),
            format!("{:<12} {}", "TERMINAL", term),
            format!("{:<12} {}", "OS", os),
            String::new(),
        ];
        let pv = desk_file.and_then(|d| render_preview(d, IMG_COLS, IMG_ROWS));
        match pv {
            Some(p) => {
                print!("{}", p.apc);
                for i in 0..IMG_ROWS {
                    let line = p
                        .rows
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| format!("{:<IMG_COLS$}", ""));
                    println!(
                        "  {line}  {}",
                        rlines.get(i).map(String::as_str).unwrap_or("")
                    );
                }
            }
            None => {
                for line in &rlines {
                    if !line.is_empty() {
                        println!("  {line}");
                    }
                }
            }
        }
    } else {
        // STACKED: the image rows stay contiguous whatever the width, or
        // the image is absent with dignity — never torn.
        let tcols = cols.saturating_sub(2).min(IMG_COLS);
        if tcols >= IMG_FLOOR
            && let Some(p) = desk_file.and_then(|d| render_preview(d, tcols, IMG_ROWS))
        {
            print!("{}", p.apc);
            for r in &p.rows {
                println!("  {r}");
            }
        }
        println!();
        let vw = cols.saturating_sub(4).max(12);
        let n = (cols.saturating_sub(4) / 4).clamp(1, 8);
        let sw = if eight.is_empty() {
            "<none>".to_string()
        } else {
            swatch_row(&eight[..n.min(eight.len())])
        };
        for (l, v) in [
            ("THEME", truncate_ellipsis(&name, vw)),
            ("COLORSCHEME", sw),
            ("TERMINAL", term),
            ("OS", truncate_ellipsis(&os, vw)),
        ] {
            println!("  {l}");
            println!("    {v}");
        }
    }
    println!();
    for (title, items) in SECTIONS {
        println!("{title}");
        print_table(cols, items, 14);
        println!();
    }
    println!("Usage:\n  theme <command> [flags]\n");
    // The parenthetical rides along only where it fits.
    println!(
        "{}",
        if cols >= 33 {
            "Global Flags (any image command):"
        } else {
            "Global Flags:"
        }
    );
    print_table(cols, FLAGS, 21);
    println!();
    for l in wrap_prefixed(
        "Use \"theme <command> --help\" for more information about a given command.",
        cols,
        "",
        "  ",
    ) {
        println!("{l}");
    }
    crate::update::maybe_note(cfg);
}

pub fn usage_cmd(cfg: &Config, cmd: &str) -> i32 {
    let wdir = display_text(&cfg.wallpaper_dirs_display);
    match cmd {
        "random" => print!(
            "theme random [--rotate left|right] [--extend[=RRGGBB]]

  Pick a random wallpaper from {wdir} and apply it everywhere:
  desktop wallpaper + kitty recolor (live windows and future ones).

  Examples:
    theme random
    theme random --rotate right
"
        ),
        "set" => print!(
            "theme set <image | url> [--rotate left|right] [--extend[=RRGGBB]]

  Apply a specific wallpaper: desktop + palette + kitty. <image> is a
  path or a name under {wdir} (extension optional). set also
  understands actionable links: an unsplash.com/photos/… page routes
  through 'theme unsplash', any other URL through 'theme url'.

  Examples:
    theme set spain-city-mountains
    theme set nebulosa-red.png --extend
    theme set https://unsplash.com/photos/a-computer-screen-with-a-wave-on-it-mOpfECCgeC4
"
        ),
        "unsplash" => print!(
            "theme unsplash <query… | photo-url | subcommand> [--rotate left|right] [--extend[=RRGGBB]]

  Fetch an Unsplash photo into {wdir} — named from your query plus
  the photo's own description — then apply it (desktop + palette +
  kitty). Downloads the RAW original rendition, preferring 3840px+ on
  search. A query needs no quotes; a photo page link
  (unsplash.com/photos/…) fetches exactly that photo. Bare
  'theme unsplash' shows this help.

  Subcommands:
    random    fully random photo (landscape, high resolution)
    auth      one-time account link (OAuth): Unsplash+ photos then
              download watermark-free, like the site's Download button —
              without it they arrive WATERMARKED. Needs the app secret
              once (env UNSPLASH_SECRET_KEY / Keychain 'unsplash-secret-key')
    status    API window: requests left, tier, key source, linked
              account (costs 1 request)

  Needs UNSPLASH_ACCESS_KEY or the 'unsplash-access-key' Keychain item.

  Examples:
    theme unsplash random
    theme unsplash neon city rain
    theme unsplash https://unsplash.com/photos/winged-person-with-halo-in-sky-coy_MhYMLHs
    theme unsplash auth
"
        ),
        "url" => print!(
            "theme url <link> [--rotate left|right]

  Download an image from a direct URL or a Pinterest pin page, save it
  into {wdir}, then apply it (desktop + palette + kitty).

  Sharpness: direct i.pinimg.com /NNNx/ downscales are auto-upgraded to
  the full-resolution /originals/ variant when it exists, and the desktop
  is set in fill mode (crop to cover — never letterbox bars).
  --rotate turns a portrait pin 90° into a landscape before applying.
  --extend centres flat-background art on a matching-color canvas instead.

  Examples:
    theme url https://www.pinterest.com/pin/300685712645323833/
    theme url https://i.pinimg.com/1200x/39/76/d8/3976d….jpg --rotate right
    theme url https://i.pinimg.com/736x/cc/a1/35/cca13….jpg --extend
"
        ),
        "list" | "ls" => print!(
            "theme list [-v] [-n <count> | --all]

  Wallpapers as a table sorted by LATEST ADDED — the newest 10 by
  default; -n <count> or --all widens it (colorschemes render from
  cache; anything missing is derived once, only for the rows shown).
  -v adds a small picture preview (kitty graphics; in kitty only) plus
  source — the site it came from, recorded at download time or read
  from macOS download metadata, \"-\" when unknown — format, size, and
  date added.

  A truncated title copied from the table (with or without the …)
  works in set/rename/rm when only one wallpaper starts with it.

  Examples:
    theme list
    theme list -v
"
        ),
        "preview" => print!(
            "theme preview [name | -w <name>]

  One wallpaper up close: a picture (kitty only; skipped elsewhere) above
  every fact the file actually has — title, artist, published date,
  camera, place, license, source, format, size — empty fields are
  omitted, never rendered blank. Long values wrap under their own
  column. Colorscheme swatches and the location (~/path) close the
  block. With no name it previews the CURRENT wallpaper; a name
  (positional or -w/--wallpaper, truncated titles welcome) previews
  that one.

  Examples:
    theme preview
    theme preview neon-pink-and-purple-light-particles
    theme preview -w trees-on-forest…
"
        ),
        "status" => print!(
            "theme status

  Show the current theme: wallpaper path, mode, the color
  scheme as truecolor swatches (like nvim/kitty theme pickers), palette
  source, and the variables the CLI reads (Unsplash key presence — never
  the value — wallpaper dir, palette cache).

  Example:
    theme status
"
        ),
        "rename" => print!(
            "theme rename <wallpaper> <new name…>

  Rename a saved wallpaper, keeping the library's naming format (the new
  name is slugified, the extension stays). The new name needs no quotes.
  If it is the current wallpaper, the desktop is re-pointed automatically.

  Examples:
    theme rename pinterest-20260829-181509-extended.jpg red-samurai-poster
    theme rename starry-boat-3840x2160-v0-uyzg0992aegb1 starry boat painting
"
        ),
        "rm" | "remove" => print!(
            "theme rm <wallpaper…>

  Delete saved wallpapers by name — resolved in {wdir} like
  `set`/`rename` (extension optional), so no path is needed. Only
  library files can be deleted. Several names at once are fine.

  Examples:
    theme rm albedo-wings-black
    theme rm old-one.jpg other-old-one
"
        ),
        "update" => print!(
            "theme update [--version <vX.Y.Z>]

  Check the latest GitHub release (snaraj/theme) and install it over
  this binary in one step, printing current → new. Already current is
  a no-op. The download is verified against the release's SHA256SUMS
  BEFORE anything is installed, and the swap is atomic — the running
  binary is never left partial or replaced by unverified bytes. If the
  install location is not writable the command says so and stops;
  theme never elevates.

  --version installs a specific release instead of the latest — older
  versions may be unsupported or break (a downgrade says so before it
  proceeds), through the same verified pipeline.

  The bare `theme` screen notes when a newer release exists (checked
  at most once a day, silently); THEME_NO_UPDATE_CHECK=1 disables the
  check. `theme update` itself always runs when you ask.

  Examples:
    theme update
    theme update --version v0.1.0
"
        ),
        "help" | "" => usage(cfg),
        other => {
            // A typo should SAY so, not silently answer with the global
            // usage as if the command existed — and it is an ERROR.
            eprintln!("theme: unknown command '{}' — full usage:\n", display_text(other));
            usage(cfg);
            return 1;
        }
    }
    0
}

//! `theme help` and the per-command texts. The header prints LIVE values —
//! desktop, scheme, terminal, OS — so each one gets its own display copy;
//! the kitty branch's OSC 8 hyperlink is the ONE trusted exception, our own
//! literal. Keep help generic and extensible, and never let it grow:
//! additions pay for themselves by consolidating something else.

use crate::apply::wallpaper_get;
use crate::config::Config;
use crate::report::{include_line, render_preview, scheme_colors};
use crate::ui::{display_text, swatch_row, truncate_ellipsis};
use std::path::Path;
use std::process::Command;

fn columns() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(100)
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
    let sw = if colors.is_empty() {
        "<none>".to_string()
    } else {
        let eight: Vec<String> = colors.iter().take(8).cloned().collect();
        let tail = if label.is_empty() {
            String::new()
        } else {
            format!(" {}", display_text(&label))
        };
        format!("{}{tail}", swatch_row(&eight))
    };
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
    let availw = columns().saturating_sub(18 + 13).max(12);
    let name = truncate_ellipsis(&name, availw);
    let rlines = [
        String::new(),
        format!("{:<12} {}", "THEME", name),
        format!("{:<12} {}", "COLORSCHEME", sw),
        format!("{:<12} {}", "TERMINAL", term),
        format!("{:<12} {}", "OS", os),
        String::new(),
    ];
    let pv = desk
        .as_deref()
        .filter(|d| d.is_file())
        .and_then(|d| render_preview(d, 14, 6));
    match pv {
        Some(p) => {
            print!("{}", p.apc);
            for i in 0..6 {
                let line = p
                    .rows
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| format!("{:<14}", ""));
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
    print!(
        "
Apply Commands:
  set             apply a wallpaper: local name/path, or any actionable link
  random          random wallpaper from the configured wallpaper folder
  unsplash        Unsplash photos: search, page-url, random; auth and status
  url             download a direct image URL or Pinterest pin, save it, apply it

Library Commands:
  list, ls        wallpaper table: title + colorscheme (-v adds source, format, size, date)
  preview         one wallpaper up close: picture, colorscheme, title, location
  rename          rename a saved wallpaper, keeping the naming format
  remove, rm      delete saved wallpapers by name

Info Commands:
  status          current theme, color-scheme swatches, variables
  version, -V     version, repository, and maintainer
  help            this text (per-command: theme <command> --help)

Usage:
  theme <command> [flags]

Global Flags (any image command):
  --rotate left|right    turn the image 90° before applying
  --extend[=RRGGBB]      centre flat art on a matching canvas (default 000000)
  --desktop-only         set the desktop wallpaper only; terminal colors stay

Use \"theme <command> --help\" for more information about a given command.
"
    );
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

  One wallpaper up close, styled like the list: a larger picture on the
  left (kitty only; skipped elsewhere) and the labeled facts on the
  right — title, location (~/path), source, size — plus a larger render
  of its colorscheme. With no name it previews the CURRENT wallpaper; a
  name (positional or -w/--wallpaper, truncated titles welcome) previews
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

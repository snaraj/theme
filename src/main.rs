//! theme — desktop wallpaper + terminal palette CLI.
//!
//! The Rust port of the dotfiles shell tool: behavior parity is the
//! contract (the boundary fixture is the proof), pigment replaces pywal,
//! and every terminal sits behind one emitter each.
//! Set THEME_NO_APPLY=1 to exercise every code path without touching the
//! desktop.
//!
//! unsafe_code is denied by the workspace lints table (`[lints]
//! workspace = true` in Cargo.toml), not a crate attribute — same as pigment.

mod apply;
mod commands;
mod config;
mod help;
mod imaging;
mod json;
mod library;
mod main_flags;
mod net;
mod report;
mod save;
mod scratch;
mod search;
/// The per-Space wallpaper store is macOS-only machinery.
#[cfg(target_os = "macos")]
mod spaces;
mod ui;
mod unsplash;
mod update;
mod urlcmd;

use config::Config;
use std::process::Command;
use ui::die;

/// `YYYYmmdd-HHMMSS`, local time — the date-stamped fallback names.
pub fn timestamp() -> String {
    Command::new("date")
        .arg("+%Y%m%d-%H%M%S")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "00000000-000000".into())
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cfg = Config::from_env();
    // `update` owns its argv end-to-end: its grammar parses the RAW argv,
    // BEFORE the global flag pass could swallow anything, and --version
    // exists for no other command — every other verb refuses it like any
    // unknown flag, pre-mutation, in the dispatch validation below.
    if argv.first().map(String::as_str) == Some("update") {
        let (want_help, version_sel) = main_flags::parse_update(&argv[1..]);
        if want_help {
            let code = help::usage_cmd(&cfg, "update");
            scratch::cleanup();
            std::process::exit(code);
        }
        update::cmd_update(&cfg, &version_sel);
        scratch::cleanup();
        return;
    }
    let mut flags = main_flags::parse(&argv);
    let args = std::mem::take(&mut flags.args);

    // Validate the ENTIRE normalized argv before dispatch: `--help` anywhere
    // means help for the command, and any other leading-dash token anywhere
    // is refused HERE — before a single side effect, so
    // `theme rm victim --bogus` deletes nothing.
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    let mut want_help = false;
    for a in args.iter().skip(1) {
        match a.as_str() {
            "-h" | "--help" => want_help = true,
            s if s.starts_with('-') => {
                die(&format!(
                    "unknown option '{s}' for 'theme {cmd}' — try: theme {cmd} --help"
                ));
            }
            _ => {}
        }
    }
    // --mkdir belongs to one verb, and it is refused here for the same
    // reason: `theme rm victim --mkdir x` must delete nothing.
    if !flags.mkdir.is_empty() && cmd != "get" {
        die("--mkdir is a 'theme get' flag");
    }
    if want_help {
        let code = help::usage_cmd(&cfg, cmd);
        scratch::cleanup();
        std::process::exit(code);
    }

    match cmd {
        "random" => commands::cmd_local(&cfg, None, &flags),
        "set" => {
            // set is generic over SOURCES: an Unsplash photo page routes
            // through the unsplash path, any other URL through the url
            // path, anything else is a library name.
            let arg = args.get(1).map(String::as_str).unwrap_or("");
            if arg.is_empty() {
                die("usage: theme set <image | link>");
            }
            if unsplash::is_photo_page(arg) {
                unsplash::cmd_unsplash(&cfg, arg, &mut flags);
            } else if arg.contains("://") {
                urlcmd::cmd_url(&cfg, arg, &mut flags);
            } else {
                commands::cmd_local(&cfg, Some(arg), &flags);
            }
        }
        "unsplash" => {
            // kubectl-style root: bare `theme unsplash` is the command's
            // help, not a surprise download.
            let rest = &args[1..];
            match rest {
                [] => {
                    let code = help::usage_cmd(&cfg, "unsplash");
                    scratch::cleanup();
                    std::process::exit(code);
                }
                [one] if one == "status" => unsplash::cmd_unsplash_status(&cfg),
                [one] if one == "auth" => unsplash::cmd_unsplash_auth(),
                [one] if one == "random" => unsplash::cmd_unsplash(&cfg, "", &mut flags),
                _ => {
                    let q = rest.join(" ");
                    unsplash::cmd_unsplash(&cfg, &q, &mut flags);
                }
            }
        }
        "get" => {
            // The library half of `set`: same sources, same saver, and then
            // it STOPS — the file lands and is shown, nothing is applied.
            if args.len() != 2 {
                die("theme get takes exactly one link");
            }
            let arg = args[1].as_str();
            if !arg.contains("://") {
                die(
                    "usage: theme get <link> [--mkdir <folder>]   (links only — a library name has nothing to download)",
                );
            }
            let sub: Option<String> = (!flags.mkdir.is_empty()).then(|| flags.mkdir.clone());
            let saved = if unsplash::is_photo_page(arg) {
                unsplash::fetch_unsplash(&cfg, arg, &mut flags, sub.as_deref())
            } else {
                urlcmd::fetch_url(&cfg, arg, &mut flags, sub.as_deref())
            };
            report::cmd_preview(&cfg, Some(&saved.to_string_lossy()));
        }
        "url" => die("theme url was folded into theme set — run: theme set <link>"),
        "list" | "ls" => report::cmd_list(&cfg, flags.verbose, flags.list_n),
        "search" => {
            // Same kubectl-style root as unsplash: bare `theme search` is the
            // command's help, not a whole-library dump.
            if args.len() < 2 {
                let code = help::usage_cmd(&cfg, "search");
                scratch::cleanup();
                std::process::exit(code);
            }
            search::cmd_search(&cfg, &args[1..], flags.list_n);
        }
        "preview" => {
            let positional = args.get(1).map(String::as_str);
            let by_flag = if flags.wallpaper.is_empty() {
                None
            } else {
                Some(flags.wallpaper.as_str())
            };
            report::cmd_preview(&cfg, by_flag.or(positional));
        }
        "status" => report::cmd_status(&cfg),
        "rename" => {
            if args.len() < 2 {
                die("usage: theme rename <wallpaper> <new name…>");
            }
            commands::cmd_rename(&cfg, &args[1..]);
        }
        "rm" | "remove" => {
            if args.len() < 2 {
                die("usage: theme rm <wallpaper…>");
            }
            commands::cmd_rm(&cfg, &args[1..]);
        }
        "version" | "--version" | "-V" => update::cmd_version(&cfg),
        // `theme help <command>` asks the same question as `theme <command>
        // --help` and gets the same answer — the whole bare screen was
        // never it. A bare `theme help` (and an unknown name) still routes
        // to the full usage through `usage_cmd`'s own arms.
        "help" | "-h" | "--help" => {
            let code = help::usage_cmd(&cfg, args.get(1).map(String::as_str).unwrap_or(""));
            scratch::cleanup();
            std::process::exit(code);
        }
        other => die(&format!(
            "unknown command '{other}' — run 'theme help' for the list"
        )),
    }
    scratch::cleanup();
}

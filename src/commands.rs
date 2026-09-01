//! The local-library verbs: apply (named or random), delete, rename.

use crate::apply::{set_desktop, use_image, wallpaper_get};
use crate::config::Config;
use crate::library::{random_local, resolve_library, resolve_local, slugify};
use crate::main_flags::Flags;
use crate::net::mime_of;
use crate::save::save_wallpaper;
use crate::scratch;
use crate::ui::{die, note};
use std::fs;
use std::path::PathBuf;

pub fn cmd_local(cfg: &Config, arg: Option<&str>, flags: &Flags) {
    let img: PathBuf = match arg {
        Some(a) => resolve_local(cfg, a).unwrap_or_else(|| {
            die(&format!(
                "no wallpaper uniquely matching '{a}' (looked in {}; a truncated name from theme list works when only one wallpaper starts with it)",
                cfg.wallpaper_dirs_display
            ))
        }),
        None => random_local(cfg).unwrap_or_else(|| {
            die(&format!("no images found in {}", cfg.wallpaper_dirs_display))
        }),
    };
    let img = if flags.transforms_requested() {
        // Never modify the library file itself — save the transformed copy
        // as its own wallpaper so the original stays available.
        let tmp = scratch::new();
        if fs::copy(&img, &tmp).is_err() {
            die(&format!("cannot copy {}", img.display()));
        }
        flags.apply_transforms(&tmp);
        let mime = mime_of(&tmp);
        let stem = img.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let hint = format!("{stem}{}", flags.hint_suffix());
        let saved = save_wallpaper(cfg, &tmp, &mime, &hint, "", &flags.source_url);
        scratch::done(&tmp);
        saved
    } else {
        img
    };
    use_image(cfg, &img, flags.desktop_only);
}

pub fn cmd_rm(cfg: &Config, names: &[String]) {
    let cur = wallpaper_get();
    for name in names {
        let img = resolve_library(cfg, name).unwrap_or_else(|| {
            die(&format!(
                "no wallpaper named '{name}' in {} (rm takes library NAMES, never paths)",
                cfg.wallpaper_dirs_display
            ))
        });
        if fs::remove_file(&img).is_err() {
            die(&format!("could not delete {}", img.display()));
        }
        let base = img.file_name().and_then(|n| n.to_str()).unwrap_or("");
        note(&format!("successfully deleted \"{base}\""));
        if cur.as_deref() == Some(img.as_path()) {
            note("that was the current wallpaper — pick a new one with theme set / theme random");
        }
    }
}

pub fn cmd_rename(cfg: &Config, args: &[String]) {
    let img = resolve_library(cfg, &args[0]).unwrap_or_else(|| {
        die(&format!(
            "no wallpaper named '{}' in {} (rename takes library NAMES, never paths)",
            args[0], cfg.wallpaper_dirs_display
        ))
    });
    let new = args[1..].join(" ");
    if new.is_empty() {
        die("usage: theme rename <wallpaper> <new name…>   (see theme rename --help)");
    }
    let base = slugify(&new);
    if base.is_empty() {
        die("that name slugifies to nothing — give it at least one letter or digit");
    }
    let ext = img.extension().and_then(|e| e.to_str()).unwrap_or("");
    let dest = img.with_file_name(format!("{base}.{ext}"));
    if dest == img {
        note(&format!(
            "already named {}",
            img.file_name().and_then(|n| n.to_str()).unwrap_or("")
        ));
        return;
    }
    // symlink_metadata: a dangling symlink is an occupied name, not a free
    // one — same reason as the saver.
    if fs::symlink_metadata(&dest).is_ok() {
        die(&format!(
            "{} already exists",
            dest.file_name().and_then(|n| n.to_str()).unwrap_or("")
        ));
    }
    if fs::rename(&img, &dest).is_err() {
        die("rename failed");
    }
    note(&format!(
        "successfully renamed \"{}\" to \"{}\"",
        img.file_name().and_then(|n| n.to_str()).unwrap_or(""),
        dest.file_name().and_then(|n| n.to_str()).unwrap_or("")
    ));
    let cur = wallpaper_get();
    if cur.as_deref() == Some(img.as_path()) {
        set_desktop(cfg, &dest);
        note("desktop re-pointed at the new name");
    }
    // Keep the palette-image record accurate too, so status stays truthful.
    let record = cfg.cache_dir.join("wal");
    if fs::read_to_string(&record)
        .map(|s| s.trim() == img.display().to_string())
        .unwrap_or(false)
    {
        let _ = fs::write(&record, dest.display().to_string());
    }
}

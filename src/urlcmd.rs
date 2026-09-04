//! The link path behind `set` and `get`: a direct image URL or any og:image
//! page (Pinterest pins), validated by CONTENT, saved through the
//! descriptor-bound saver. Fetching and applying are separate — `get` stops
//! at the saved file.

use crate::apply::use_image;
use crate::config::Config;
use crate::library::slugify;
use crate::main_flags::Flags;
use crate::net::{fetch_img, host_label, mime_of, url_host};
use crate::save::save_wallpaper;
use crate::ui::{die, note};
use crate::{scratch, timestamp};
use std::path::{Path, PathBuf};

/// A descriptive filename hint from a URL: its basename, or — when that
/// carries no letters at all — the whole host-and-path.
fn name_hint(url: &str) -> String {
    let path = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let path = path.split('?').next().unwrap_or("");
    let base = path.rsplit('/').next().unwrap_or("");
    let stem = base.rsplit_once('.').map(|(s, _)| s).unwrap_or(base);
    if slugify(stem).bytes().any(|b| b.is_ascii_lowercase()) {
        base.to_string()
    } else {
        path.to_string()
    }
}

/// Like fetch_img, but an `i.pinimg.com/NNNx/` downscale is first tried at
/// `/originals/` with a note; returns the URL that actually served.
fn fetch_best(url: &str, dest: &Path) -> Option<String> {
    if let Some(pos) = url.find("//i.pinimg.com/") {
        let after = &url[pos + "//i.pinimg.com/".len()..];
        if let Some(slash) = after.find('/') {
            let seg = &after[..slash];
            if seg.len() >= 2
                && seg.ends_with('x')
                && seg[..seg.len() - 1].bytes().all(|b| b.is_ascii_digit())
                && !seg[..seg.len() - 1].is_empty()
            {
                let orig = format!(
                    "{}//i.pinimg.com/originals/{}",
                    &url[..pos],
                    &after[slash + 1..]
                );
                if orig != url && fetch_img(&orig, dest) {
                    note("upgraded the pinimg downscale to /originals/");
                    return Some(orig);
                }
            }
        }
    }
    if fetch_img(url, dest) {
        Some(url.to_string())
    } else {
        None
    }
}

/// og:image / og:title from an HTML page, either attribute order, either
/// quote style, case-insensitive — the shell's OG_PY.
pub fn og_meta(html: &str) -> (String, String) {
    let mut image = String::new();
    let mut title = String::new();
    // ASCII-only lowercasing: tag/attribute names are ASCII, and unlike
    // to_lowercase() it is length-preserving, so offsets found in `lower`
    // stay valid in the original (İ and friends change byte length).
    let lower = html.to_ascii_lowercase();
    let mut at = 0;
    while let Some(pos) = lower[at..].find("<meta") {
        let start = at + pos;
        let end = lower[start..]
            .find('>')
            .map(|e| start + e)
            .unwrap_or(lower.len());
        let tag = &html[start..end];
        let prop = attr(tag, "property")
            .or_else(|| attr(tag, "name"))
            .unwrap_or_default();
        if let Some(content) = attr(tag, "content") {
            if prop.eq_ignore_ascii_case("og:image") && image.is_empty() {
                image = content;
            } else if prop.eq_ignore_ascii_case("og:title") && title.is_empty() {
                title = content;
            }
        }
        at = end;
    }
    (image, title)
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut at = 0;
    loop {
        let pos = lower[at..].find(name)? + at;
        let rest = &tag[pos + name.len()..];
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix('=') {
            let rest = rest.trim_start();
            let quote = rest.chars().next()?;
            if quote == '"' || quote == '\'' {
                let body = &rest[1..];
                if let Some(endq) = body.find(quote) {
                    return Some(body[..endq].to_string());
                }
            }
        }
        at = pos + name.len();
        if at >= lower.len() {
            return None;
        }
    }
}

/// Download `link` into the library and return the saved path — no apply.
/// `subdir` overrides the provider label the download would otherwise be
/// filed under (`theme get --mkdir`).
pub fn fetch_url(cfg: &Config, link: &str, flags: &mut Flags, subdir: Option<&str>) -> PathBuf {
    if link.is_empty() {
        die("usage: theme set <image-url | pinterest-pin-url>");
    }
    flags.source_url = link.to_string();
    let tmp = scratch::new();
    let link = match fetch_best(link, &tmp) {
        Some(served) => served,
        None => die(&format!("download failed: {link}")),
    };
    let mut mime = mime_of(&tmp);
    // Name hint without its extension (the saver strips from the LAST dot).
    // A bare CDN hash is not a name — date-stamp those.
    let mut hint = name_hint(&link);
    if let Some((stem, _)) = hint.rsplit_once('.') {
        hint = stem.to_string();
    }
    if hint.len() >= 16
        && hint
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        hint = format!("pinterest-{}", timestamp());
    }
    if mime.starts_with("text/html") || mime.starts_with("application/xhtml") {
        // Pinterest pins (and any og:image page) resolve ONE level.
        let html = match std::fs::read(&tmp) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(_) => die(&format!("could not parse the page at {link}")),
        };
        let (page_img, page_title) = og_meta(&html);
        if page_img.is_empty() {
            die("no og:image on that page — pass a direct image URL instead");
        }
        // The og:image is REMOTE-supplied. A string check is not enough — curl
        // resolves the host, so decimal/hex/octal IP spellings and DNS names
        // that point inward slip past it. Fast-reject the obvious shapes, then
        // RESOLVE the host ourselves, require every address to be global, and
        // pin curl to the vetted one (no redirects) so it cannot reach a
        // destination we did not clear.
        if !crate::net::is_public_http(&page_img) {
            die("that page's og:image is not a public http(s) image URL");
        }
        let vetted = match crate::net::vet_untrusted(&page_img) {
            Ok(v) => v,
            Err(e) => die(&format!("refusing that page's og:image - {e}")),
        };
        if !crate::net::fetch_vetted(&vetted, &tmp) {
            die(&format!("download failed: {page_img}"));
        }
        mime = mime_of(&tmp);
        if !mime.starts_with("image/") {
            die(&format!("resolved link is {mime}, not an image"));
        }
        hint = if page_title.is_empty() {
            name_hint(&page_img)
        } else {
            page_title
        };
    } else if !mime.starts_with("image/") {
        die(&format!("that URL is {mime}, not an image"));
    }
    flags.apply_transforms(&tmp);
    if flags.transforms_requested() {
        // A transform re-encodes (WebP lands as PNG): name the save by the
        // post-transform bytes — the order cmd_local already uses.
        mime = mime_of(&tmp);
    }
    // Route the download into its provider's subfolder; an unrecognized
    // host keeps the library root.
    let sub = subdir.unwrap_or_else(|| url_host(&link).and_then(|h| host_label(&h)).unwrap_or(""));
    let hint = format!("{hint}{}", flags.hint_suffix());
    let saved = save_wallpaper(cfg, &tmp, &mime, &hint, sub, &flags.source_url);
    scratch::done(&tmp);
    saved
}

pub fn cmd_url(cfg: &Config, link: &str, flags: &mut Flags) {
    let saved = fetch_url(cfg, link, flags, None);
    use_image(cfg, &saved, flags.desktop_only);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn og_meta_both_orders_and_quotes() {
        let h = r#"<META Property='og:image' Content='https://x/1.jpg'>
                   <meta content="A Title" property="og:title">"#;
        let (i, t) = og_meta(h);
        assert_eq!(i, "https://x/1.jpg");
        assert_eq!(t, "A Title");
    }

    /// to_lowercase() is not length-preserving (İ grows 2→3 bytes, Å and K
    /// shrink), so lowercased-copy offsets desynced from the original and
    /// the tag slice drifted — or split a UTF-8 boundary and panicked. A
    /// page salted with all three around AND inside the tags must resolve.
    #[test]
    fn og_meta_survives_length_changing_lowercase() {
        let h = "<title>İSTANBUL Å K İİİ</title>\n\
                 <meta property='og:image' content='https://x/İ-pic.jpg'>\n\
                 <meta property='og:title' content='Işık Å K'>";
        let (i, t) = og_meta(h);
        assert_eq!(i, "https://x/İ-pic.jpg");
        assert_eq!(t, "Işık Å K");
    }

    #[test]
    fn name_hint_falls_back_to_path_for_numeric_basenames() {
        assert_eq!(name_hint("https://a.b/c/photo-name.jpg"), "photo-name.jpg");
        assert_eq!(name_hint("https://a.b/3840/2160.jpg"), "a.b/3840/2160.jpg");
    }
}

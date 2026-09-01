//! Unsplash: fetch, status, and the one-time OAuth account link. Every
//! credential travels to curl as `-K -` stdin config — never argv — and is
//! grammar-gated at the point it enters that config: curl config is a
//! GRAMMAR (`directive = "value"`), a quote ends a value and a newline
//! starts a NEW directive, so a hostile credential must die with ZERO
//! transfers.

use crate::apply::use_image;
use crate::config::Config;
use crate::json::Json;
use crate::main_flags::Flags;
use crate::net::{curl_config, curl_download, host_under, mime_of, url_host};
use crate::save::save_wallpaper;
use crate::scratch;
use crate::ui::{die, note};
use std::io::Write as _;
use std::process::{Command, Stdio};

pub fn keychain_read(service: &str) -> Option<String> {
    let out = Command::new("security")
        .args(["find-generic-password", "-s", service, "-w"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if v.is_empty() { None } else { Some(v) }
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn unsplash_key() -> Option<String> {
    env_nonempty("UNSPLASH_ACCESS_KEY").or_else(|| keychain_read("unsplash-access-key"))
}

fn user_token() -> Option<String> {
    env_nonempty("UNSPLASH_USER_TOKEN").or_else(|| keychain_read("unsplash-user-token"))
}

const NO_KEY: &str = "no Unsplash key: export UNSPLASH_ACCESS_KEY, or run `security add-generic-password -s unsplash-access-key -a \"$USER\" -w` after getting a free key at https://unsplash.com/oauth/applications";

/// The closed token set real Unsplash keys, secrets, tokens and codes use —
/// and nothing that can reach curl's config syntax. Every SOURCE passes
/// through this: env, Keychain and pasted alike.
fn require_credential(what: &str, value: &str) {
    if value.is_empty() {
        die(&format!("{what} is empty"));
    }
    if !value.bytes().all(|b| {
        b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'+' | b'/' | b'=' | b'-')
    }) {
        die(&format!(
            "{what} contains characters that cannot occur in an Unsplash credential (letters, digits and . _ ~ + / = - only) — refusing to build a curl request with it"
        ));
    }
}

/// ONE curl config line carrying the strongest available credential.
/// Building it validates it, so calling this before any request is the
/// main-shell check the shell version needed two layers for.
fn auth_line() -> String {
    if let Some(tok) = user_token() {
        require_credential(
            "the Unsplash account token (UNSPLASH_USER_TOKEN / Keychain unsplash-user-token)",
            &tok,
        );
        format!("header = \"Authorization: Bearer {tok}\"\n")
    } else {
        let key = unsplash_key().unwrap_or_default();
        require_credential(
            "the Unsplash access key (UNSPLASH_ACCESS_KEY / Keychain unsplash-access-key)",
            &key,
        );
        format!("header = \"Authorization: Client-ID {key}\"\n")
    }
}

struct Photo {
    img_url: String,
    width: i64,
    height: i64,
    name: String,
    download_location: String,
    who: String,
    premium: bool,
    /// Capture-time metadata the FILE will not carry (CDNs strip EXIF):
    /// persisted as theme.* xattrs at save time so preview can render it.
    published: String,
    camera: String,
    place: String,
}

/// The shell's UNSPLASH_PY, minus the NUL transport it needed: candidates
/// (list or single), widest ≥3840 preferred, description → slug-minus-id →
/// id as the name hint, raw before full, premium decided by the EXACT image
/// host.
fn parse_photo(json: &Json) -> Option<Photo> {
    let list: Vec<&Json> = match json {
        Json::Arr(a) => a.iter().collect(),
        other => vec![other],
    };
    if list.is_empty() {
        return None;
    }
    fn width(p: &Json) -> i64 {
        p.get("width").and_then(Json::as_f64).unwrap_or(0.0) as i64
    }
    let big: Vec<&Json> = list.iter().copied().filter(|p| width(p) >= 3840).collect();
    let best: &Json = if big.is_empty() {
        list.into_iter().max_by_key(|p| width(p))?
    } else {
        big.into_iter().max_by_key(|p| width(p))?
    };
    let name = best
        .str_field("alt_description")
        .or_else(|| best.str_field("description"))
        .map(str::to_string)
        .or_else(|| {
            let slug = best.str_field("slug").unwrap_or("");
            let trimmed = strip_photo_id(slug);
            if !trimmed.is_empty() {
                Some(trimmed)
            } else {
                best.str_field("id").map(str::to_string)
            }
        })
        .unwrap_or_else(|| "photo".into());
    let urls = best.get("urls");
    let img_url = urls
        .and_then(|u| u.str_field("raw").or_else(|| u.str_field("full")))
        .unwrap_or("")
        .to_string();
    let premium = url_host(&img_url).as_deref() == Some("plus.unsplash.com");
    Some(Photo {
        width: width(best),
        height: best.get("height").and_then(Json::as_f64).unwrap_or(0.0) as i64,
        name,
        download_location: best
            .get("links")
            .and_then(|l| l.str_field("download_location"))
            .unwrap_or("")
            .to_string(),
        who: best
            .get("user")
            .and_then(|u| u.str_field("name"))
            .unwrap_or("")
            .to_string(),
        published: best
            .str_field("created_at")
            .map(|s| s.chars().take(10).collect()) // the date half of the ISO stamp
            .unwrap_or_default(),
        camera: {
            let ex = best.get("exif");
            let mk = ex.and_then(|e| e.str_field("make")).unwrap_or("");
            let md = ex.and_then(|e| e.str_field("model")).unwrap_or("");
            if md.starts_with(mk) {
                md.to_string() // many models repeat the make; don't double it
            } else {
                format!("{mk} {md}").trim().to_string()
            }
        },
        place: best
            .get("location")
            .map(|l| {
                let name = l.str_field("name").unwrap_or("");
                if !name.is_empty() {
                    return name.to_string();
                }
                let city = l.str_field("city").unwrap_or("");
                let country = l.str_field("country").unwrap_or("");
                [city, country]
                    .iter()
                    .filter(|s| !s.is_empty())
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default(),
        img_url,
        premium,
    })
}

/// An Unsplash slug carries the 11-char photo id as its last token.
fn strip_photo_id(slug: &str) -> String {
    if let Some(pos) = slug.rfind('-') {
        let tail = &slug[pos + 1..];
        if tail.len() == 11
            && tail
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return slug[..pos].to_string();
        }
    }
    slug.to_string()
}

pub fn cmd_unsplash(cfg: &Config, arg: &str, flags: &mut Flags) {
    if unsplash_key().is_none() {
        die(NO_KEY);
    }
    // Grammar-check the credential before a single request is built.
    let auth = auth_line();

    let mut query = arg.to_string();
    let mut pick = String::new();
    let url;
    if arg.contains("://") {
        // Only an EXACT https Unsplash photo page qualifies, and the slug is
        // charset-allowlisted before it may sit in URL position.
        let path = arg
            .strip_prefix("https://unsplash.com/photos/")
            .or_else(|| arg.strip_prefix("https://www.unsplash.com/photos/"));
        match path {
            Some(p) if !p.is_empty() => {}
            _ => die(
                "only https://unsplash.com/photos/… links work here — for other links use: theme url",
            ),
        }
        pick = arg.rsplit("/photos/").next().unwrap_or("").to_string();
        pick = pick.split('?').next().unwrap_or("").to_string();
        pick = pick.split('/').next().unwrap_or("").to_string();
        if pick.is_empty()
            || !pick
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            die("that link carries no valid photo id (letters, digits, - and _ only)");
        }
        url = format!("https://api.unsplash.com/photos/{pick}");
        query.clear();
        flags.source_url = arg.to_string();
    } else {
        url = "https://api.unsplash.com/photos/random?count=5&orientation=landscape&content_filter=high".into();
    }

    // -g (globoff) keeps [] and {} literal — one command is one request;
    // -G --data-urlencode encodes the WHOLE query, so & or [] stays text.
    let mut args: Vec<&str> = vec!["-fsLg", "--max-time", "30", "-K", "-"];
    let q;
    if !query.is_empty() {
        q = format!("query={query}");
        args.extend_from_slice(&["-G", "--data-urlencode", &q]);
    }
    args.push(&url);
    let body = curl_config(&auth, &args)
        .unwrap_or_else(|| die("Unsplash request failed (bad key, rate limit, or no network)"));
    let photo = String::from_utf8(body)
        .ok()
        .and_then(|s| Json::parse(&s))
        .and_then(|j| parse_photo(&j))
        .unwrap_or_else(|| die("Unsplash returned no usable photo"));

    let mut img_url = photo.img_url.clone();
    if photo.premium && user_token().is_none() {
        note("Unsplash+ photo over application-key auth: the file WILL carry the watermark");
        note("one-time fix: theme unsplash auth (links your Unsplash+ account, clean files after)");
    }
    // The download-report call attaches the credential, so its target must
    // be an api.unsplash.com HTTPS URL and nothing else.
    let dl = if photo
        .download_location
        .starts_with("https://api.unsplash.com/")
    {
        photo.download_location.clone()
    } else {
        String::new()
    };
    // Unsplash+ entitlement lives in the DOWNLOAD endpoint: called WITH the
    // account bearer it answers a signed delivery URL — the clean file. The
    // answer chooses a host, so it is bound before use: https and a
    // dot-subdomain of unsplash.com, through the same parser as every other
    // host decision here.
    let mut reported = false;
    if photo.premium && user_token().is_some() && !dl.is_empty() {
        let entitled = curl_config(
            &auth,
            &["-fsg", "--max-time", "30", "-K", "-", "--url", &dl],
        )
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| Json::parse(&s).and_then(|j| j.str_field("url").map(str::to_string)))
        .unwrap_or_default();
        let host = url_host(&entitled).unwrap_or_default();
        if entitled.starts_with("https://")
            && !host.is_empty()
            && host != "unsplash.com"
            && host_under(&host, "unsplash.com")
        {
            img_url = entitled;
            reported = true;
        } else {
            note(
                "entitled download unavailable — falling back to the standard (watermarked) rendition",
            );
        }
    }
    if img_url.is_empty() {
        die("Unsplash returned no image URL");
    }
    if flags.source_url.is_empty() {
        flags.source_url = img_url.clone();
    }
    if photo.width < 3840 {
        if !pick.is_empty() {
            note(&format!(
                "that photo's original is {}x{} (under 3840px)",
                photo.width, photo.height
            ));
        } else {
            note(&format!(
                "best of 5 candidates is {}x{} (wanted 3840px+)",
                photo.width, photo.height
            ));
        }
    }
    let tmp = scratch::new();
    if !curl_download(&img_url, &tmp, 90) {
        die("photo download failed");
    }
    let mut mime = mime_of(&tmp);
    if !mime.starts_with("image/") {
        die(&format!("Unsplash served {mime}, not an image"));
    }
    flags.apply_transforms(&tmp);
    if flags.transforms_requested() {
        // A transform re-encodes (WebP lands as PNG): name the save by the
        // post-transform bytes — the order cmd_local already uses.
        mime = mime_of(&tmp);
    }
    // Name = your search prompt (when given) + the photo's own description.
    // The photographer is credited in the terminal note, not the filename.
    let hint = format!(
        "{}{}{}",
        if query.is_empty() {
            String::new()
        } else {
            format!("{query} ")
        },
        photo.name,
        flags.hint_suffix()
    );
    let saved = save_wallpaper(cfg, &tmp, &mime, &hint, "unsplash", &flags.source_url);
    scratch::done(&tmp);
    // The served file carries no EXIF (the CDN strips it) — persist the
    // capture-time facts as theme.* xattrs so preview can render them.
    crate::save::record_meta(&saved, "theme.artist", &photo.who);
    crate::save::record_meta(&saved, "theme.published", &photo.published);
    crate::save::record_meta(&saved, "theme.camera", &photo.camera);
    crate::save::record_meta(&saved, "theme.place", &photo.place);
    crate::save::record_meta(&saved, "theme.license", "Unsplash License");
    // Unsplash API guideline: report the download so the photographer is
    // credited. --url draws an explicit boundary so the (validated) target
    // can never be read as another curl option.
    if !dl.is_empty() && !reported {
        let _ = curl_config(
            &auth,
            &[
                "-fsg",
                "--max-time",
                "15",
                "-K",
                "-",
                "-o",
                "/dev/null",
                "--url",
                &dl,
            ],
        );
    }
    if !photo.who.is_empty() {
        note(&format!("photo by {} on Unsplash", photo.who));
    }
    use_image(cfg, &saved, flags.desktop_only);
}

pub fn cmd_unsplash_status(cfg: &Config) {
    let _ = cfg;
    if unsplash_key().is_none() {
        die(NO_KEY);
    }
    let auth = auth_line();
    let tmp = scratch::new();
    let tmp_s = tmp.display().to_string();
    let ok = curl_config(
        &auth,
        &[
            "-fsg",
            "--max-time",
            "15",
            "-K",
            "-",
            "-D",
            &tmp_s,
            "-o",
            "/dev/null",
            "https://api.unsplash.com/photos?page=1&per_page=1",
        ],
    )
    .is_some();
    if !ok {
        die("Unsplash request failed (bad key, rate limit exhausted, or no network)");
    }
    let headers = std::fs::read_to_string(&tmp).unwrap_or_default();
    scratch::done(&tmp);
    let header = |name: &str| -> String {
        headers
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                if k.to_lowercase() == name {
                    Some(v.trim().trim_end_matches('\r').to_string())
                } else {
                    None
                }
            })
            .unwrap_or_default()
    };
    let limit = header("x-ratelimit-limit");
    let mut remaining = header("x-ratelimit-remaining");
    // Printed as facts, so the shape is checked rather than trusted.
    if limit.is_empty() || !limit.bytes().all(|b| b.is_ascii_digit()) {
        die("Unsplash answered without usable rate-limit headers");
    }
    let digits = remaining.strip_prefix('-').unwrap_or(&remaining);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        remaining = String::new();
    }
    if remaining.starts_with('-') {
        println!(
            "requests left this hour:  {remaining}/{limit} (window EXCEEDED — resets on the hour)"
        );
    } else {
        println!("requests left this hour:  {remaining}/{limit} (resets on the hour)");
    }
    match limit.as_str() {
        "50" => println!("tier:                     demo (50/hour; production raises it to 5000)"),
        "5000" => println!("tier:                     production (5000/hour)"),
        _ => println!("tier:                     custom limit {limit}/hour"),
    }
    if env_nonempty("UNSPLASH_ACCESS_KEY").is_some() {
        println!("key:                      set (env UNSPLASH_ACCESS_KEY)");
    } else {
        println!("key:                      set (Keychain: unsplash-access-key)");
    }
    if user_token().is_some() {
        println!(
            "account:                  user token linked (Bearer) — Unsplash+ files come clean"
        );
    } else {
        println!("account:                  application access key (Client-ID); no user is logged");
        println!(
            "                          in, so Unsplash+ photos arrive WATERMARKED — see: theme unsplash auth"
        );
    }
    println!("note:                     this check spent 1 request of the window above");
}

pub fn cmd_unsplash_auth() {
    let key = unsplash_key().unwrap_or_else(|| die(NO_KEY));
    let secret = env_nonempty("UNSPLASH_SECRET_KEY")
        .or_else(|| keychain_read("unsplash-secret-key"))
        .unwrap_or_else(|| {
            die("the exchange needs your app's SECRET key (shown beside the access key at https://unsplash.com/oauth/applications): export UNSPLASH_SECRET_KEY, or store it once with `security add-generic-password -s unsplash-secret-key -a \"$USER\" -w`")
        });
    // Both halves enter curl's -K grammar as `data = "…"` values, and the
    // key also enters a URL handed to the browser. Check them BEFORE either
    // can travel.
    require_credential(
        "the Unsplash access key (UNSPLASH_ACCESS_KEY / Keychain unsplash-access-key)",
        &key,
    );
    require_credential(
        "the Unsplash app secret (UNSPLASH_SECRET_KEY / Keychain unsplash-secret-key)",
        &secret,
    );
    let authurl = format!(
        "https://unsplash.com/oauth/authorize?client_id={key}&redirect_uri=urn:ietf:wg:oauth:2.0:oob&response_type=code&scope=public"
    );
    note("opening the authorize page — sign in as your Unsplash+ account and approve");
    let opened = Command::new("open")
        .arg(&authurl)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !opened {
        note(&format!("open: {authurl}"));
    }
    print!("paste the code shown after approving: ");
    let _ = std::io::stdout().flush();
    let mut code = String::new();
    let _ = std::io::stdin().read_line(&mut code);
    // Browser copies arrive padded — strip transport noise BEFORE the gate.
    let code: String = code
        .chars()
        .filter(|c| !matches!(c, ' ' | '\t' | '\r' | '\n'))
        .collect();
    if code.is_empty()
        || !code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        die(
            "that does not look like an authorization code (letters, digits, - and _ only) — copy just the code text, without surrounding characters",
        );
    }
    require_credential("the authorization code", &code);
    let config = format!(
        "data = \"client_id={key}\"\ndata = \"client_secret={secret}\"\ndata = \"redirect_uri=urn:ietf:wg:oauth:2.0:oob\"\ndata = \"code={code}\"\ndata = \"grant_type=authorization_code\"\n"
    );
    let body = curl_config(
        &config,
        &["-fsg", "--max-time", "30", "-K", "-", "https://unsplash.com/oauth/token"],
    )
    .unwrap_or_else(|| {
        die("token exchange failed — wrong/expired code, or the app's redirect URIs do not include urn:ietf:wg:oauth:2.0:oob (add it on the dashboard)")
    });
    let tok = String::from_utf8(body)
        .ok()
        .and_then(|s| Json::parse(&s))
        .and_then(|j| j.str_field("access_token").map(str::to_string))
        .unwrap_or_default();
    if tok.is_empty() {
        die("Unsplash answered without an access token");
    }
    if !tok.bytes().all(|b| {
        b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'+' | b'/' | b'=' | b'-')
    }) {
        die("unexpected token shape — refusing to store it");
    }
    // Stored by piping into `security -i`, never as an argument.
    let user = std::env::var("USER").unwrap_or_default();
    let stored = (|| -> Option<bool> {
        let mut child = Command::new("security")
            .arg("-i")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        child
            .stdin
            .take()?
            .write_all(
                format!("add-generic-password -U -a {user} -s unsplash-user-token -w {tok}\n")
                    .as_bytes(),
            )
            .ok()?;
        Some(child.wait().ok()?.success())
    })()
    .unwrap_or(false);
    if !stored {
        die("could not store the token in the Keychain");
    }
    note("linked — Unsplash+ downloads are now watermark-free (Keychain: unsplash-user-token)");
}

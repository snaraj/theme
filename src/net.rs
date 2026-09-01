//! URL parsing discipline and the curl subprocess boundary.
//!
//! curl stays a SUBPROCESS on purpose: credentials travel to it as `-K -`
//! stdin config lines (never argv, never `ps`-visible), and the boundary
//! fixture drives every network case through a deterministic PATH-stubbed
//! curl — an in-process HTTP client would erase both properties.

use crate::config::{MAX_DOWNLOAD_BYTES, UA};
use std::io::Write;
use std::net::IpAddr;
use std::path::Path;
use std::process::{Command, Stdio};

/// The lowercased hostname of an http(s) URL, or None. Userinfo is stripped
/// BEFORE the host is read (the host of `https://unsplash.com@evil.invalid/x`
/// is evil.invalid); the port goes after.
pub fn url_host(url: &str) -> Option<String> {
    let h = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let h = h.split(['/', '?', '#']).next().unwrap_or("");
    let h = h.rsplit('@').next().unwrap_or("");
    let h = h.split(':').next().unwrap_or("");
    if h.is_empty() {
        None
    } else {
        Some(h.to_lowercase())
    }
}

/// Is `host` the domain `dom`, or a subdomain of it? Deliberately NOT a
/// substring test: the dot boundary is what keeps `evilunsplash.com` and
/// `unsplash.com.evil.invalid` out.
pub fn host_under(host: &str, dom: &str) -> bool {
    host == dom || host.ends_with(&format!(".{dom}"))
}

/// Hostname → provider label (exact-hostname discipline, never substrings).
pub fn host_label(host: &str) -> Option<&'static str> {
    for d in ["unsplash.com", "images.unsplash.com"] {
        if host_under(host, d) {
            return Some("unsplash");
        }
    }
    for d in ["pinimg.com", "pinterest.com"] {
        if host_under(host, d) {
            return Some("pinterest");
        }
    }
    for d in ["redd.it", "reddit.com", "redditmedia.com"] {
        if host_under(host, d) {
            return Some("reddit");
        }
    }
    None
}

/// `i.pinimg.com/736x/…` → `i.pinimg.com/originals/…` (identity elsewhere).
pub fn pinimg_original(url: &str) -> String {
    if let Some(pos) = url.find("i.pinimg.com/") {
        let after = &url[pos + "i.pinimg.com/".len()..];
        if let Some(slash) = after.find('/') {
            let seg = &after[..slash];
            let ok = seg.len() >= 2
                && seg.ends_with(|c: char| c.is_ascii_digit() || c == 'x')
                && seg.contains('x')
                && seg.starts_with(|c: char| c.is_ascii_digit())
                && seg.chars().all(|c| c.is_ascii_digit() || c == 'x')
                && (2..=4).contains(&seg.split('x').next().unwrap_or("").len());
            if ok {
                return format!(
                    "{}i.pinimg.com/originals/{}",
                    &url[..pos],
                    &after[slash + 1..]
                );
            }
        }
    }
    url.to_string()
}

/// Fetch `url` to `dest`: the pinimg-upgraded URL first, then exactly what
/// was asked for. Mirrors the shell's `fetch_img` flags: -fsLg, 60s, UA.
pub fn fetch_img(url: &str, dest: &Path) -> bool {
    let up = pinimg_original(url);
    if curl_download(&up, dest, 60) {
        return true;
    }
    if up == url {
        return false;
    }
    curl_download(url, dest, 60)
}

/// http(s) only, and not aimed at loopback/link-local/private/unspecified.
/// The gate for URLs supplied by REMOTE metadata (an og:image): a wallpaper
/// fetch has no business reaching the local host or private network on the
/// say-so of a downloaded page. Literal-IP and `localhost` targets are
/// rejected here; a DNS name that resolves inward is left to curl's transport
/// (we do not resolve, to stay a thin subprocess boundary).
pub fn is_public_http(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
    else {
        return false;
    };
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    let auth = auth.rsplit('@').next().unwrap_or(""); // strip userinfo
    // Host: a bracketed IPv6 literal, or host[:port]. url_host is not reused
    // here because its `:`-split mangles `[::1]`.
    let host = if let Some(inner) = auth.strip_prefix('[') {
        inner.split(']').next().unwrap_or("")
    } else {
        auth.split(':').next().unwrap_or("")
    }
    .to_lowercase();
    if host.is_empty() {
        return false;
    }
    if host == "localhost" || host.ends_with(".localhost") {
        return false;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(a)) => {
            !(a.is_loopback()
                || a.is_private()
                || a.is_link_local()
                || a.is_unspecified()
                || a.is_broadcast())
        }
        Ok(IpAddr::V6(a)) => {
            let seg0 = a.segments()[0];
            !(a.is_loopback()
                || a.is_unspecified()
                || (seg0 & 0xfe00) == 0xfc00 // unique-local fc00::/7
                || (seg0 & 0xffc0) == 0xfe80) // link-local fe80::/10
        }
        Err(_) => true, // a DNS name — scheme-gated, resolved by curl
    }
}

pub fn curl_download(url: &str, dest: &Path, timeout: u32) -> bool {
    // `--url` binds the value so an option-shaped URL cannot become an option;
    // `--proto`/`--proto-redir '=http,https'` refuse file:, gopher:, and any
    // cross-protocol redirect; `--max-filesize` is the first (time-independent)
    // byte cap, backstopped by a post-open size check at the saver.
    Command::new("curl")
        .args([
            "-fsLg",
            "--proto",
            "=http,https",
            "--proto-redir",
            "=http,https",
            "--max-filesize",
            &MAX_DOWNLOAD_BYTES.to_string(),
            "--max-time",
            &timeout.to_string(),
            "-A",
            UA,
            "-o",
        ])
        .arg(dest)
        .arg("--url")
        .arg(url)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run curl with `-K -` stdin config (the credential transport) plus extra
/// argv. Returns stdout on success.
pub fn curl_config(config: &str, args: &[&str]) -> Option<Vec<u8>> {
    let mut child = Command::new("curl")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(config.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    if out.status.success() {
        Some(out.stdout)
    } else {
        None
    }
}

/// `file -b --mime-type` — content-typed, never extension-typed.
pub fn mime_of(path: &Path) -> String {
    Command::new("file")
        .args(["-b", "--mime-type"])
        .arg(path)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_parsing_strips_userinfo_first() {
        assert_eq!(
            url_host("https://unsplash.com@evil.invalid/x").as_deref(),
            Some("evil.invalid")
        );
        assert_eq!(
            url_host("https://UNSPLASH.com:443/a?b#c").as_deref(),
            Some("unsplash.com")
        );
        assert_eq!(url_host("ftp://x"), None);
        assert_eq!(url_host("https://"), None);
    }

    #[test]
    fn host_under_is_a_dot_boundary() {
        assert!(host_under("unsplash.com", "unsplash.com"));
        assert!(host_under("images.unsplash.com", "unsplash.com"));
        assert!(!host_under("evilunsplash.com", "unsplash.com"));
        assert!(!host_under("unsplash.com.evil.invalid", "unsplash.com"));
    }

    #[test]
    fn provider_labels_are_exact() {
        assert_eq!(host_label("images.unsplash.com"), Some("unsplash"));
        assert_eq!(host_label("i.pinimg.com"), Some("pinterest"));
        assert_eq!(host_label("evilunsplash.com"), None);
    }

    #[test]
    fn public_http_rejects_ssrf_shapes() {
        // Accepted: ordinary public http(s), incl. a public literal IP.
        assert!(is_public_http("https://images.unsplash.com/a.jpg"));
        assert!(is_public_http("http://example.com/a.png"));
        assert!(is_public_http("https://203.0.113.7/a.png"));
        // Rejected: non-http(s) schemes and option-shaped values.
        for u in [
            "file:///etc/passwd",
            "gopher://x/1",
            "ftp://h/a",
            "-O/tmp/x",
            "--config=/tmp/evil",
        ] {
            assert!(!is_public_http(u), "accepted {u}");
        }
        // Rejected: loopback / link-local / private / unspecified targets.
        for u in [
            "http://localhost/a",
            "http://sub.localhost/a",
            "http://127.0.0.1/a",
            "http://127.9.9.9/a",
            "http://0.0.0.0/a",
            "http://169.254.1.1/a",
            "http://10.0.0.1/a",
            "http://192.168.1.1/a",
            "http://172.16.0.1/a",
            "http://[::1]/a",
            "http://[fe80::1]/a",
            "http://[fc00::1]/a",
        ] {
            assert!(!is_public_http(u), "accepted {u}");
        }
    }

    #[test]
    fn pinimg_upgrade_shapes() {
        assert_eq!(
            pinimg_original("https://i.pinimg.com/736x/a/b.jpg"),
            "https://i.pinimg.com/originals/a/b.jpg"
        );
        assert_eq!(
            pinimg_original("https://i.pinimg.com/1200x/a/b.jpg"),
            "https://i.pinimg.com/originals/a/b.jpg"
        );
        // Already originals, other hosts, and non-size segments: identity.
        for u in [
            "https://i.pinimg.com/originals/a/b.jpg",
            "https://example.com/736x/a.jpg",
            "https://i.pinimg.com/xx/a.jpg",
        ] {
            assert_eq!(pinimg_original(u), u);
        }
    }
}

//! URL parsing discipline and the curl subprocess boundary.
//!
//! curl stays a SUBPROCESS on purpose: credentials travel to it as `-K -`
//! stdin config lines (never argv, never `ps`-visible), and the boundary
//! fixture drives every network case through a deterministic PATH-stubbed
//! curl — an in-process HTTP client would erase both properties.

use crate::config::{MAX_DOWNLOAD_BYTES, UA};
use crate::ui::note;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};
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

/// A globally-routable unicast address: NOT loopback, private, link-local,
/// ULA, unspecified, broadcast, CGNAT, documentation, or `0.0.0.0/8` — and an
/// IPv4-mapped/compatible IPv6 is judged by the v4 it wraps.
pub fn is_global_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => is_global_v4(a),
        IpAddr::V6(a) => {
            if let Some(v4) = a.to_ipv4() {
                return is_global_v4(&v4); // ::a.b.c.d and ::ffff:a.b.c.d
            }
            let seg0 = a.segments()[0];
            !(a.is_loopback()
                || a.is_unspecified()
                || (seg0 & 0xfe00) == 0xfc00 // ULA fc00::/7
                || (seg0 & 0xffc0) == 0xfe80) // link-local fe80::/10
        }
    }
}

fn is_global_v4(a: &Ipv4Addr) -> bool {
    let o = a.octets();
    !(a.is_loopback()
        || a.is_private()
        || a.is_link_local()
        || a.is_unspecified()
        || a.is_broadcast()
        || a.is_documentation()
        || o[0] == 0 // "this network" 0.0.0.0/8
        || (o[0] == 100 && (o[1] & 0xc0) == 64)) // CGNAT 100.64.0.0/10
}

/// Split an http(s) URL into (host, port), bracket-aware for IPv6. The port
/// defaults to the scheme's (443/80). Userinfo is stripped first.
fn host_port(url: &str) -> Option<(String, u16)> {
    let (default_port, rest) = url
        .strip_prefix("https://")
        .map(|r| (443u16, r))
        .or_else(|| url.strip_prefix("http://").map(|r| (80u16, r)))?;
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    let auth = auth.rsplit('@').next().unwrap_or("");
    if auth.is_empty() {
        return None;
    }
    if let Some(inner) = auth.strip_prefix('[') {
        let (host, after) = inner.split_once(']')?;
        let port = after
            .strip_prefix(':')
            .and_then(|p| p.parse().ok())
            .unwrap_or(default_port);
        Some((host.to_string(), port))
    } else {
        // host[:port] — only treat a trailing all-digit segment as the port.
        match auth.rsplit_once(':') {
            Some((host, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => {
                Some((host.to_string(), p.parse().ok()?))
            }
            _ => Some((auth.to_string(), default_port)),
        }
    }
}

/// A URL whose host RESOLVED to a vetted global address, carried alongside the
/// address curl must be pinned to. Constructing one is the proof the
/// destination — not merely the string — was checked.
pub struct Vetted {
    url: String,
    host: String,
    port: u16,
    ip: IpAddr,
}

/// Resolve `url`'s host and require EVERY resolved address to be global,
/// returning the URL bound to one vetted address. `getaddrinfo` parses numeric
/// host spellings (decimal `2130706433`, hex, octal, IPv4-mapped IPv6) into
/// their real address, so vetting the RESULT closes those spellings AND a DNS
/// name that resolves inward in one move — curl can no longer resolve to
/// something we did not check, because we pin it to what we did.
pub fn vet_untrusted(url: &str) -> Result<Vetted, String> {
    let (host, port) = host_port(url).ok_or("not an http(s) URL")?;
    let addrs: Vec<_> = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {host}: {e}"))?
        .collect();
    if addrs.is_empty() {
        return Err(format!("{host} did not resolve"));
    }
    for sa in &addrs {
        if !is_global_ip(&sa.ip()) {
            return Err(format!("{host} resolves to a non-public address"));
        }
    }
    Ok(Vetted {
        url: url.to_string(),
        host,
        port,
        ip: addrs[0].ip(),
    })
}

/// Fetch a vetted URL, pinning curl to the address we cleared and forbidding
/// redirects — so the bytes come from exactly the destination we vetted. The
/// pinimg `/originals/` upgrade stays on the same host, so the pin still holds.
pub fn fetch_vetted(v: &Vetted, dest: &Path) -> bool {
    let up = pinimg_original(&v.url);
    if up != v.url && curl_pinned(&up, v, dest, 60) {
        note("upgraded the pinimg downscale to /originals/");
        return true;
    }
    curl_pinned(&v.url, v, dest, 60)
}

fn curl_pinned(url: &str, v: &Vetted, dest: &Path, timeout: u32) -> bool {
    // `--resolve host:port:ip` pins curl to the address we vetted; NO `-L` and
    // `--max-redirs 0` forbid following a redirect to an unvetted (e.g.
    // loopback) destination — curl will not re-vet, so it must not chase.
    Command::new("curl")
        .args([
            "-fsg",
            "--max-redirs",
            "0",
            "--proto",
            "=http,https",
            "--proto-redir",
            "=http,https",
            "--resolve",
            &format!("{}:{}:{}", v.host, v.port, v.ip),
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
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    /// A loopback server that counts accepted connections and, if given a
    /// response, sends it. Non-blocking with a short deadline so the thread
    /// winds down on its own.
    fn spawn_server(resp: Option<String>) -> (u16, Arc<AtomicU32>, std::thread::JoinHandle<()>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        let port = l.local_addr().unwrap().port();
        let hits = Arc::new(AtomicU32::new(0));
        let h = hits.clone();
        let jh = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                match l.accept() {
                    Ok((mut s, _)) => {
                        h.fetch_add(1, Ordering::SeqCst);
                        let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
                        let _ = s.read(&mut [0u8; 512]);
                        if let Some(r) = &resp {
                            let _ = s.write_all(r.as_bytes());
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        });
        (port, hits, jh)
    }

    /// The SSRF vectors the string check missed: numeric host spellings and a
    /// name resolving inward. Each must be REFUSED at the resolve-and-vet gate
    /// with the loopback listener recording ZERO connections — proving the
    /// block happens before any packet, not merely in an argv.
    #[test]
    fn ssrf_vectors_never_reach_a_loopback_listener() {
        let (port, hits, jh) = spawn_server(None);
        let vectors = [
            format!("http://127.0.0.1:{port}/x.png"), // literal loopback
            format!("http://2130706433:{port}/x.png"), // decimal spelling
            format!("http://0x7f000001:{port}/x.png"), // hex spelling
            format!("http://017700000001:{port}/x.png"), // octal spelling
            format!("http://[::ffff:127.0.0.1]:{port}/x"), // IPv4-mapped IPv6
            format!("http://localhost:{port}/x.png"), // name → loopback
        ];
        for v in &vectors {
            assert!(
                vet_untrusted(v).is_err(),
                "vet admitted an SSRF vector: {v}"
            );
        }
        jh.join().unwrap();
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "the loopback listener was reached"
        );
    }

    /// A public-shaped URL that 302-redirects to loopback must not be chased:
    /// the pinned hop reaches the allowed server, sees the redirect, and does
    /// NOT follow it, so the loopback victim records zero connections.
    #[test]
    fn a_redirect_to_loopback_is_not_followed() {
        let (victim_port, victim_hits, vjh) = spawn_server(None);
        let redirect = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{victim_port}/x\r\nContent-Length: 0\r\n\r\n"
        );
        let (a_port, a_hits, ajh) = spawn_server(Some(redirect));
        // Pin an allowed (in-test loopback) hop by hand — Vetted's fields are
        // visible to this child module — and prove curl_pinned reaches it but
        // will not chase its redirect to the victim.
        let v = Vetted {
            url: format!("http://pin.test:{a_port}/r"),
            host: "pin.test".to_string(),
            port: a_port,
            ip: "127.0.0.1".parse().unwrap(),
        };
        let dest = std::env::temp_dir().join(format!("theme-redir-{}", std::process::id()));
        let _ = curl_pinned(&v.url, &v, &dest, 5);
        let _ = std::fs::remove_file(&dest);
        vjh.join().unwrap();
        ajh.join().unwrap();
        assert!(
            a_hits.load(Ordering::SeqCst) >= 1,
            "the allowed hop was not reached"
        );
        assert_eq!(
            victim_hits.load(Ordering::SeqCst),
            0,
            "curl followed the redirect to loopback"
        );
    }

    #[test]
    fn global_ip_predicate() {
        for ip in ["93.184.216.34", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(is_global_ip(&ip.parse().unwrap()), "rejected public {ip}");
        }
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "172.16.0.1",
            "169.254.0.1",
            "100.64.0.1", // CGNAT
            "0.0.0.0",
            "::1",
            "::ffff:127.0.0.1", // mapped loopback
            "fe80::1",
            "fc00::1",
        ] {
            assert!(
                !is_global_ip(&ip.parse().unwrap()),
                "admitted non-global {ip}"
            );
        }
    }

    #[test]
    fn host_port_splits_scheme_default_and_ipv6() {
        assert_eq!(host_port("https://a.b/c"), Some(("a.b".into(), 443)));
        assert_eq!(host_port("http://a.b/c"), Some(("a.b".into(), 80)));
        assert_eq!(host_port("https://a.b:8443/c"), Some(("a.b".into(), 8443)));
        assert_eq!(host_port("http://[::1]:9/x"), Some(("::1".into(), 9)));
        assert_eq!(host_port("https://[::1]/x"), Some(("::1".into(), 443)));
        assert_eq!(host_port("https://u@a.b/x"), Some(("a.b".into(), 443)));
        assert_eq!(host_port("file:///x"), None);
    }

    /// A public IP literal vets clean (no DNS, no network) — the positive path
    /// the refusals must not swallow.
    #[test]
    fn a_public_literal_vets_ok() {
        assert!(vet_untrusted("https://93.184.216.34/a.png").is_ok());
    }

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

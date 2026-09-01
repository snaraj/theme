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

/// True only for a globally-routable unicast address — an allowlist by
/// exclusion of EVERY IANA special-use range, not a partial denylist (the
/// stdlib `is_global` is nightly-only). An IPv4-mapped/compatible IPv6 is
/// judged by the v4 it wraps; the pinned untrusted hop refuses anything else.
pub fn is_global_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => is_global_v4(a),
        IpAddr::V6(a) => {
            // ::ffff:0:0/96 — judge by the embedded v4.
            if let Some(v4) = a.to_ipv4_mapped() {
                return is_global_v4(&v4);
            }
            // ::/96 (incl. ::, ::1, deprecated IPv4-compatible) — non-global.
            if a.to_ipv4().is_some() {
                return false;
            }
            let s = a.segments();
            let special = (s[0] & 0xfe00) == 0xfc00                       // fc00::/7 ULA
                || (s[0] & 0xffc0) == 0xfe80                              // fe80::/10 link-local
                || (s[0] & 0xff00) == 0xff00                              // ff00::/8 multicast
                || (s[0] == 0x2001 && s[1] == 0x0db8)                     // 2001:db8::/32 doc
                || (s[0] == 0x2001 && s[1] == 0x0002 && s[2] == 0)        // 2001:2::/48 benchmarking
                || (s[0] == 0x2001 && (s[1] & 0xfff0) == 0x0020)          // 2001:20::/28 ORCHIDv2
                || (s[0] == 0x0064 && s[1] == 0xff9b && s[2] == 0x0001)   // 64:ff9b:1::/48 NAT64 local
                || (s[0] == 0x0100 && s[1] == 0 && s[2] == 0 && s[3] == 0); // 100::/64 discard
            !special
        }
    }
}

fn is_global_v4(a: &Ipv4Addr) -> bool {
    let o = a.octets();
    let special = o[0] == 0                                  // 0.0.0.0/8 this-network
        || o[0] == 10                                       // 10/8 private
        || o[0] == 127                                      // 127/8 loopback
        || (o[0] == 100 && (o[1] & 0xc0) == 64)             // 100.64/10 CGNAT
        || (o[0] == 169 && o[1] == 254)                     // 169.254/16 link-local
        || (o[0] == 172 && (16..=31).contains(&o[1]))       // 172.16/12 private
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)          // 192.0.0/24 IETF protocol
        || (o[0] == 192 && o[1] == 0 && o[2] == 2)          // 192.0.2/24 TEST-NET-1
        || (o[0] == 192 && o[1] == 88 && o[2] == 99)        // 192.88.99/24 6to4 relay
        || (o[0] == 192 && o[1] == 168)                     // 192.168/16 private
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19))      // 198.18/15 benchmarking
        || (o[0] == 198 && o[1] == 51 && o[2] == 100)       // 198.51.100/24 TEST-NET-2
        || (o[0] == 203 && o[1] == 0 && o[2] == 113)        // 203.0.113/24 TEST-NET-3
        || o[0] >= 224; // 224/4 multicast + 240/4 reserved + 255.255.255.255 broadcast
    !special
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

/// Every proxy-selecting variable curl consults, in both cases. Scrubbed from
/// the pinned hop's child env so a proxy cannot re-resolve the host inward.
const PROXY_VARS: [&str; 10] = [
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "ftp_proxy",
    "no_proxy",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "FTP_PROXY",
    "NO_PROXY",
];

fn pinned_command(url: &str, v: &Vetted, dest: &Path, timeout: u32) -> Command {
    // `--resolve host:port:ip` pins curl to the address we vetted; NO `-L` and
    // `--max-redirs 0` forbid following a redirect to an unvetted (e.g.
    // loopback) destination — curl will not re-vet, so it must not chase.
    // `--noproxy '*'` (plus scrubbing every proxy var from the child env, in
    // case a curl build ignores the flag) stops a proxy from re-resolving the
    // host inward and defeating the pin entirely.
    let mut cmd = Command::new("curl");
    cmd.args([
        "-fsg",
        "--noproxy",
        "*",
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
    .arg(url);
    for k in PROXY_VARS {
        cmd.env_remove(k);
    }
    cmd
}

fn curl_pinned(url: &str, v: &Vetted, dest: &Path, timeout: u32) -> bool {
    pinned_command(url, v, dest, timeout)
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

    /// A proxy env var must not defeat the pin: with a hostile proxy injected
    /// straight into curl's child env (past the scrub), `--noproxy '*'` keeps
    /// curl off it — the proxy records ZERO connections and the pinned server
    /// is reached directly.
    #[test]
    fn a_proxy_env_cannot_defeat_the_pin() {
        let ok = "HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc".to_string();
        let (proxy_port, proxy_hits, pjh) = spawn_server(Some(ok.clone()));
        let (pin_port, pin_hits, sjh) = spawn_server(Some(ok));
        let v = Vetted {
            url: format!("http://pin.test:{pin_port}/x"),
            host: "pin.test".to_string(),
            port: pin_port,
            ip: "127.0.0.1".parse().unwrap(),
        };
        let dest = std::env::temp_dir().join(format!("theme-proxy-{}", std::process::id()));
        let proxy = format!("http://127.0.0.1:{proxy_port}");
        let mut cmd = pinned_command(&v.url, &v, &dest, 5);
        cmd.env("http_proxy", &proxy)
            .env("HTTP_PROXY", &proxy)
            .env("all_proxy", &proxy);
        let _ = cmd.status();
        let _ = std::fs::remove_file(&dest);
        pjh.join().unwrap();
        sjh.join().unwrap();
        assert_eq!(
            proxy_hits.load(Ordering::SeqCst),
            0,
            "curl used the proxy despite --noproxy"
        );
        assert!(
            pin_hits.load(Ordering::SeqCst) >= 1,
            "the pinned server was not reached directly"
        );
    }

    /// The belt to `--noproxy`'s suspenders: every proxy var is scrubbed from
    /// the child env (proven structurally — a curl build ignoring --noproxy
    /// still sees no proxy variable).
    #[test]
    fn the_pinned_command_scrubs_proxy_env() {
        let v = Vetted {
            url: "http://pin.test/x".to_string(),
            host: "pin.test".to_string(),
            port: 80,
            ip: "203.0.113.9".parse().unwrap(),
        };
        let cmd = pinned_command(&v.url, &v, Path::new("/dev/null"), 5);
        let removed: Vec<_> = cmd
            .get_envs()
            .filter(|(_, val)| val.is_none())
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        for k in PROXY_VARS {
            assert!(removed.contains(&k.to_string()), "{k} not scrubbed");
        }
    }

    #[test]
    fn global_ip_predicate() {
        for ip in [
            "93.184.216.34",
            "1.1.1.1",
            "8.8.8.8",
            "198.20.0.1", // just outside the 198.18/15 benchmark block
            "2606:4700:4700::1111",
            "2001:4860:4860::8888",
            "::ffff:1.1.1.1", // mapped PUBLIC v4 stays global
        ] {
            assert!(is_global_ip(&ip.parse().unwrap()), "rejected public {ip}");
        }
        for ip in [
            // v4 special-use, one representative per range.
            "0.0.0.0",         // 0/8
            "10.0.0.1",        // 10/8
            "127.0.0.1",       // 127/8
            "100.64.0.1",      // 100.64/10 CGNAT
            "169.254.0.1",     // 169.254/16
            "172.16.0.1",      // 172.16/12
            "192.0.0.1",       // 192.0.0/24
            "192.0.2.1",       // 192.0.2/24 TEST-NET-1
            "192.88.99.1",     // 192.88.99/24
            "192.168.1.1",     // 192.168/16
            "198.18.0.1",      // 198.18/15 benchmarking
            "198.19.0.1",      // 198.18/15 upper half
            "198.51.100.1",    // 198.51.100/24 TEST-NET-2
            "203.0.113.1",     // 203.0.113/24 TEST-NET-3
            "224.0.0.1",       // 224/4 multicast
            "239.255.255.255", // 224/4 multicast upper
            "240.0.0.1",       // 240/4 reserved
            "255.255.255.255", // broadcast
            // v6 special-use.
            "::",               // ::/128
            "::1",              // ::1/128
            "::ffff:127.0.0.1", // mapped loopback
            "::ffff:10.0.0.1",  // mapped private
            "64:ff9b:1::1",     // NAT64 local
            "100::1",           // discard-only
            "2001:db8::1",      // documentation
            "2001:2::1",        // benchmarking
            "2001:20::1",       // ORCHIDv2
            "fe80::1",          // link-local
            "fc00::1",          // ULA
            "ff02::1",          // multicast
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

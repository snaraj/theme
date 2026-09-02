//! URL parsing discipline and the curl subprocess boundary.
//!
//! curl stays a SUBPROCESS on purpose: credentials travel to it as `-K -`
//! stdin config lines (never argv, never `ps`-visible), and the boundary
//! fixture drives every network case through a deterministic stubbed curl —
//! an in-process HTTP client would erase both properties.
//!
//! Two transport trust levels (round 8): the wallpaper/credential lanes
//! resolve `curl` from PATH per the reviewed #12 parity design, but the
//! SELF-UPDATE lane — where one binary supplies metadata, digest file, AND
//! the hashed bytes about to replace the running executable — only ever
//! runs a curl at a fixed absolute path that passes
//! [`crate::save::trusted_system_binary`], validated before every use.
//! PATH is never consulted there.

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

/// The FAST string gate for remote-metadata URLs: http(s) only, and a literal
/// IP or `localhost` target must be global. This is the pre-filter with the
/// clear message — the authoritative check is [`vet_untrusted`], which
/// resolves the host and vets every address it actually maps to.
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
        // One predicate for literals — the same the resolve gate applies, so
        // the pre-gate cannot be weaker than the authoritative check.
        Ok(ip) => is_global_ip(&ip),
        Err(_) => true, // a DNS name — judged by resolve-and-vet, not here
    }
}

/// The IANA IPv6 global-unicast ALLOCATED rows, as (first 32 bits, prefix
/// length ≤ 32) — snapshot fetched 2026-09-01 from
/// <https://www.iana.org/assignments/ipv6-unicast-address-assignments/ipv6-unicast-address-assignments.csv>.
///
/// Maintenance contract: this table mirrors the registry's ALLOCATED rows
/// only. IANA reserves everything it does not list, so a stale table can only
/// OVER-REJECT a future allocation (a parity bug to fix by refreshing from the
/// CSV) — it can never admit reserved space. `2002::/16` (6to4, allocated) is
/// deliberately absent: the code judges it by its embedded v4 before the table.
const V6_ALLOCATED: [(u32, u32); 35] = [
    (0x2001_0000, 23), // IANA — special-purpose; excluded again after the table
    (0x2001_0200, 23), // APNIC
    (0x2001_0400, 23), // ARIN
    (0x2001_0600, 23), // RIPE NCC
    (0x2001_0800, 22), // RIPE NCC
    (0x2001_0c00, 23), // APNIC (contains 2001:db8::/32, excluded below)
    (0x2001_0e00, 23), // APNIC
    (0x2001_1200, 23), // LACNIC
    (0x2001_1400, 22), // RIPE NCC
    (0x2001_1800, 23), // ARIN
    (0x2001_1a00, 23), // RIPE NCC
    (0x2001_1c00, 22), // RIPE NCC
    (0x2001_2000, 19), // RIPE NCC
    (0x2001_4000, 23), // RIPE NCC
    (0x2001_4200, 23), // AFRINIC
    (0x2001_4400, 23), // APNIC
    (0x2001_4600, 23), // RIPE NCC
    (0x2001_4800, 23), // ARIN
    (0x2001_4a00, 23), // RIPE NCC
    (0x2001_4c00, 23), // RIPE NCC
    (0x2001_5000, 20), // RIPE NCC
    (0x2001_8000, 19), // APNIC
    (0x2001_a000, 20), // APNIC
    (0x2001_b000, 20), // APNIC
    (0x2003_0000, 18), // RIPE NCC
    (0x2400_0000, 12), // APNIC
    (0x2410_0000, 12), // APNIC
    (0x2600_0000, 12), // ARIN
    (0x2610_0000, 23), // ARIN
    (0x2620_0000, 23), // ARIN
    (0x2630_0000, 12), // ARIN
    (0x2800_0000, 12), // LACNIC
    (0x2a00_0000, 12), // RIPE NCC
    (0x2a10_0000, 12), // RIPE NCC
    (0x2c00_0000, 12), // AFRINIC
];

/// True only for a globally-routable unicast address (the stdlib `is_global`
/// is nightly-only). The two halves are structured differently on purpose:
///
/// - **v6 is an allocation-based allowlist, fail-closed**: an address is
///   global only when it falls inside a currently-ALLOCATED IANA
///   global-unicast row ([`V6_ALLOCATED`]), minus the non-global
///   special-purpose rows inside allocated space (`2001::/23` as a disclosed
///   blanket, `2001:db8::/32`). `2000::/3` is *assignable*, not allocated —
///   its reserved blocks (`2000::` itself, `2d00::/8` … `3fff::/20`) and all
///   space outside it are denied because no row lists them.
/// - **v4 space is fully allocated, so a denylist is sound there** — with the
///   two IANA globally-reachable /32 exceptions inside `192.0.0.0/24`.
///
/// An IPv4-mapped IPv6 (`::ffff:0:0/96`) is judged by the v4 it wraps, as is
/// a 6to4 address (`2002::/16` — its destination IS the embedded v4).
pub fn is_global_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => is_global_v4(a),
        IpAddr::V6(a) => {
            // ::ffff:0:0/96 — judge by the embedded v4 (outside every table
            // row, so this must come first).
            if let Some(v4) = a.to_ipv4_mapped() {
                return is_global_v4(&v4);
            }
            let s = a.segments();
            if s[0] == 0x2002 {
                // 2002::/16 6to4 — judged by the embedded v4 destination.
                let v4 =
                    Ipv4Addr::new((s[1] >> 8) as u8, s[1] as u8, (s[2] >> 8) as u8, s[2] as u8);
                return is_global_v4(&v4);
            }
            let a32 = (u32::from(s[0]) << 16) | u32::from(s[1]);
            if !V6_ALLOCATED
                .iter()
                .any(|&(prefix, len)| (a32 ^ prefix) >> (32 - len) == 0)
            {
                return false; // not in any ALLOCATED row — reserved/future space
            }
            if s[0] == 0x2001 && (s[1] & 0xfe00) == 0 {
                return false; // 2001::/23 IANA special-purpose (disclosed blanket)
            }
            if s[0] == 0x2001 && s[1] == 0x0db8 {
                return false; // 2001:db8::/32 documentation (inside 2001:c00::/23)
            }
            true
        }
    }
}

fn is_global_v4(a: &Ipv4Addr) -> bool {
    let o = a.octets();
    // IANA marks exactly two /32s inside 192.0.0.0/24 globally reachable:
    // 192.0.0.9 (PCP anycast) and 192.0.0.10 (NAT64/DNS64 discovery).
    if o == [192, 0, 0, 9] || o == [192, 0, 0, 10] {
        return true;
    }
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

/// Fixed absolute candidates for the self-update transport. Not PATH: a
/// planted curl there controls the release metadata, the SHA256SUMS file,
/// and the bytes hashed against it in one stroke, so PATH resolution would
/// make the SHA-256 check self-referential (Codex round 8).
#[cfg(target_os = "macos")]
const CURL_CANDIDATES: [&str; 1] = ["/usr/bin/curl"];
#[cfg(not(target_os = "macos"))]
const CURL_CANDIDATES: [&str; 2] = ["/usr/bin/curl", "/bin/curl"];

/// First candidate that passes [`crate::save::trusted_system_binary`]
/// (root-owned binary in a root-owned, non-writable directory). Split from
/// [`trusted_curl`] so the refusal path is drivable in unit tests with
/// planted candidates.
fn resolve_curl(cands: &[&str]) -> Option<std::path::PathBuf> {
    cands
        .iter()
        .find(|c| crate::save::trusted_system_binary(c))
        .map(std::path::PathBuf::from)
}

/// The one curl the self-update/footer lane may run, RE-VALIDATED on every
/// call — never cached, never PATH-resolved. None means the lane must
/// refuse (explicit `theme update`) or silently stand down (footer note).
///
/// TEST SEAM, debug builds only and compiled OUT of release: the boundary
/// fixture drives this lane with a deterministic stub via THEME_CURL
/// (empty value simulates "no trusted curl"). A release binary carries no
/// read of the variable at all — the fixture pins that too.
pub fn trusted_curl() -> Option<std::path::PathBuf> {
    #[cfg(debug_assertions)]
    if let Ok(p) = std::env::var("THEME_CURL") {
        return if p.is_empty() {
            None
        } else {
            Some(std::path::PathBuf::from(p))
        };
    }
    resolve_curl(&CURL_CANDIDATES)
}

/// [`curl_config`] for the self-update lane: the caller passes the
/// validated absolute curl from [`trusted_curl`], and the child env is
/// scrubbed of every proxy-selecting variable (`--noproxy '*'` on the argv
/// as well, for curl builds that ignore the env) so no ambient variable
/// can interpose a middlebox between GitHub and bytes headed for
/// executable replacement. Callers put `-q` FIRST in `args` — no curlrc.
pub fn curl_config_trusted(program: &Path, config: &str, args: &[&str]) -> Option<Vec<u8>> {
    let mut child = Command::new(program);
    child
        .args(args)
        .args(["--noproxy", "*"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for k in PROXY_VARS {
        child.env_remove(k);
    }
    let mut child = child.spawn().ok()?;
    child.stdin.take()?.write_all(config.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    if out.status.success() {
        Some(out.stdout)
    } else {
        None
    }
}

/// The proxy scrub, shared with the asset-download hops in `update.rs`.
pub fn scrub_proxy_env(cmd: &mut Command) {
    for k in PROXY_VARS {
        cmd.env_remove(k);
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

    /// Round 8: the self-update transport resolver refuses a curl that is
    /// not root-owned in a root-owned directory — a planted candidate in
    /// user territory never resolves, and an invalid candidate ahead of the
    /// system one is skipped, not trusted.
    #[test]
    fn update_transport_only_trusts_system_curl() {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-tmp")
            .join(format!("net-curl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let planted = d.join("curl");
        std::fs::write(&planted, "#!/bin/sh\nexit 0\n").unwrap();
        let planted = planted.to_str().unwrap().to_string();
        assert_eq!(resolve_curl(&[&planted]), None, "planted curl resolved");
        assert_eq!(resolve_curl(&[]), None);
        // The real system curl passes on every platform CI runs, and an
        // invalid candidate listed first must not shadow it.
        let sys = std::path::PathBuf::from("/usr/bin/curl");
        assert_eq!(resolve_curl(&["/usr/bin/curl"]), Some(sys.clone()));
        assert_eq!(resolve_curl(&[&planted, "/usr/bin/curl"]), Some(sys));
        let _ = std::fs::remove_dir_all(&d);
    }
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

    /// Both halves of the predicate, with the FIRST and LAST address inside
    /// every rejected range and the adjacent address outside it — so an
    /// off-by-one at any boundary fails a named case, not a sampled one.
    #[test]
    fn global_ip_predicate() {
        let global = [
            // Ordinary public v4.
            "93.184.216.34",
            "8.8.8.8",
            // v4 boundary neighbours of each rejected range.
            "1.0.0.0",         // above 0.0.0.0/8
            "9.255.255.255",   // below 10/8
            "11.0.0.0",        // above 10/8
            "100.63.255.255",  // below 100.64/10
            "100.128.0.0",     // above 100.64/10
            "126.255.255.255", // below 127/8
            "128.0.0.0",       // above 127/8
            "169.253.255.255", // below 169.254/16
            "169.255.0.0",     // above 169.254/16
            "172.15.255.255",  // below 172.16/12
            "172.32.0.0",      // above 172.16/12
            "192.0.0.9",       // IANA globally-reachable exception (PCP anycast)
            "192.0.0.10",      // IANA globally-reachable exception (NAT64 disc.)
            "192.0.1.0",       // between 192.0.0/24 and 192.0.2/24
            "192.0.3.0",       // above 192.0.2/24
            "192.88.98.255",   // below 192.88.99/24
            "192.88.100.0",    // above 192.88.99/24
            "192.167.255.255", // below 192.168/16
            "192.169.0.0",     // above 192.168/16
            "198.17.255.255",  // below 198.18/15
            "198.20.0.0",      // above 198.18/15
            "198.51.99.255",   // below 198.51.100/24
            "198.51.101.0",    // above 198.51.100/24
            "203.0.112.255",   // below 203.0.113/24
            "203.0.114.0",     // above 203.0.113/24
            "223.255.255.255", // last before 224/4
            // v6: one address inside EVERY currently-ALLOCATED IANA row
            // (2002::/16 is exercised via its embedded-v4 branch below).
            "2001:200::1",
            "2001:400::1",
            "2001:600::1",
            "2001:800::1",
            "2001:c00::1",
            "2001:e00::1",
            "2001:1200::1",
            "2001:1400::1",
            "2001:1800::1",
            "2001:1a00::1",
            "2001:1c00::1",
            "2001:2000::1",
            "2001:3fff::1", // last /16 of the 2001:2000::/19 row
            "2001:4000::1",
            "2001:4200::1",
            "2001:4400::1",
            "2001:4600::1",
            "2001:4800::1",
            "2001:4a00::1",
            "2001:4c00::1",
            "2001:5000::1",
            "2001:8000::1",
            "2001:a000::1",
            "2001:b000::1",
            "2003::1",
            "2400::1",
            "2410::1",
            "2600::1",
            "2610::1",
            "2620::1",
            "2630::1",
            "2800::1",
            "2a00::1",
            "2a10::1",
            "2c00::1",
            // Real-world anchors (live-AAAA classes: Google, Cloudflare,
            // Wikimedia, Fastly) and the db8 neighbours (inside 2001:c00::/23).
            "2001:4860:4860::8888",
            "2606:4700:4700::1111",
            "2620:0:863:ed1a::1",
            "2a04:4e42:87::313",
            "2001:db7:ffff::1",
            "2001:db9::1",
            "2002:101:101::1", // 6to4 embedding public 1.1.1.1
            "::ffff:1.1.1.1",  // mapped PUBLIC v4 stays global
        ];
        for ip in global {
            assert!(is_global_ip(&ip.parse().unwrap()), "rejected public {ip}");
        }
        let non_global = [
            // v4 special-use: first and last of each range, plus the
            // neighbours of the two in-range exceptions.
            "0.0.0.0",
            "0.255.255.255",
            "10.0.0.0",
            "10.255.255.255",
            "100.64.0.0",
            "100.127.255.255",
            "127.0.0.0",
            "127.255.255.255",
            "169.254.0.0",
            "169.254.255.255",
            "172.16.0.0",
            "172.31.255.255",
            "192.0.0.0",
            "192.0.0.8",  // neighbour below the .9/.10 exceptions
            "192.0.0.11", // neighbour above the .9/.10 exceptions
            "192.0.0.255",
            "192.0.2.0",
            "192.0.2.255",
            "192.88.99.0",
            "192.88.99.255",
            "192.168.0.0",
            "192.168.255.255",
            "198.18.0.0",
            "198.19.255.255",
            "198.51.100.0",
            "198.51.100.255",
            "203.0.113.0",
            "203.0.113.255",
            "224.0.0.0", // multicast through reserved to broadcast, one block
            "239.255.255.255",
            "240.0.0.0",
            "255.255.255.255",
            // v6 outside 2000::/3 — rejected by STRUCTURE, not by rows.
            "::",
            "::1",
            "100:0:0:1::1", // IANA dummy prefix (the round-4 reproduction)
            "100::1",       // discard-only
            "64:ff9b:1::1", // NAT64 local
            "400::1",       // reserved
            "800::1",       // reserved
            "5f00::1",      // reserved (reviewer-named)
            "1fff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", // last below 2000::/3
            "4000::",       // first above 2000::/3
            "4000::1",
            "fec0::1",          // deprecated site-local
            "fe80::1",          // link-local
            "fc00::1",          // ULA
            "ff02::1",          // multicast
            "::ffff:127.0.0.1", // mapped loopback
            "::ffff:10.0.0.1",  // mapped private
            // v6 inside assignable 2000::/3 but NOT in any ALLOCATED row:
            // the round-5 reproduction and its family, one per RESERVED row,
            // and the unlisted gaps between allocated rows.
            "2000::", // reviewer-named: 2000:: itself has no allocation
            "2000::1",
            "2004::1",      // unlisted gap after 2003::/18's /16
            "2003:4000::1", // just past 2003::/18
            "2100::1",      // unlisted gap
            "2500::1",      // unlisted gap between 2410::/12 and 2600::/12
            "2700::1",      // unlisted gap
            "2900::1",      // unlisted gap
            "2b00::1",      // unlisted gap
            "2c10::1",      // just past 2c00::/12
            "2001:6000::1", // gap between 2001:5000::/20 and 2001:8000::/19
            "2001:c000::1", // just past 2001:b000::/20
            "2d00::1",      // RESERVED row
            "2e00::1",      // RESERVED row
            "3000::1",      // RESERVED row
            "3800::1",      // RESERVED row
            "3c00::1",      // RESERVED row
            "3e00::1",      // RESERVED row
            "3f00::1",      // RESERVED row
            "3f80::1",      // RESERVED row
            "3fc0::1",      // RESERVED row
            "3fe0::1",      // RESERVED row
            "3ff0::1",      // RESERVED row
            "3ff8::1",      // RESERVED row
            "3ffc::1",      // RESERVED row
            "3ffe::1",      // RESERVED row (returned 6bone)
            "3fff:1000::1", // reviewer-named: unlisted, past the doc /20
            "3fff:ffff::1", // reviewer-named: unlisted top of the /3
            "3fff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
            // v6 non-global special rows INSIDE allocated space.
            "2001::", // first of 2001::/23 (IANA special-purpose blanket)
            "2001::1",
            "2001:2::1",                              // benchmarking, inside the /23
            "2001:20::1",                             // ORCHIDv2, inside the /23
            "2001:1ff:ffff:ffff:ffff:ffff:ffff:ffff", // last of 2001::/23
            "2001:db8::",                             // first of 2001:db8::/32
            "2001:db8:ffff:ffff:ffff:ffff:ffff:ffff", // last of 2001:db8::/32
            "2002:7f00:1::1",                         // 6to4 embedding loopback 127.0.0.1
            "2002:c0a8:1::1",                         // 6to4 embedding private 192.168.0.1
            "3fff::",                                 // documentation 3fff::/20
            "3fff::1",
            "3fff:fff:ffff:ffff:ffff:ffff:ffff:ffff", // last of 3fff::/20
        ];
        for ip in non_global {
            assert!(
                !is_global_ip(&ip.parse().unwrap()),
                "admitted non-global {ip}"
            );
        }
    }

    /// Live-DNS sanity for the allocation table's parity risk (over-rejecting
    /// real space): AAAA records of major CDN/RIR-hosted sites must classify
    /// global. Ignored in CI — the suite stays hermetic; run locally with
    /// `cargo test -- --ignored` when the table changes.
    #[test]
    #[ignore = "live DNS — local evidence only"]
    fn live_aaaa_records_classify_global() {
        let mut seen = 0;
        for host in [
            "www.cloudflare.com",
            "www.google.com",
            "www.wikipedia.org",
            "www.fastly.com",
        ] {
            let Ok(addrs) = (host, 443).to_socket_addrs() else {
                continue;
            };
            for sa in addrs {
                if matches!(sa.ip(), IpAddr::V6(_)) {
                    seen += 1;
                    assert!(is_global_ip(&sa.ip()), "{host} AAAA {} rejected", sa.ip());
                }
            }
        }
        assert!(seen > 0, "no AAAA records resolved - no evidence gathered");
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
        assert!(is_public_http("https://93.184.216.34/a.png"));
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
        // Rejected: loopback / link-local / private / unspecified targets —
        // and, since the literal branch is is_global_ip itself, the special
        // ranges too (TEST-NET-3, IANA dummy space).
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
            "http://203.0.113.7/a",
            "http://[::1]/a",
            "http://[fe80::1]/a",
            "http://[fc00::1]/a",
            "http://[100:0:0:1::1]/a",
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

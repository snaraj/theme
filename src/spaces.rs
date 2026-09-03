//! Every macOS Space, not just the one you are looking at.
//!
//! The `wallpaper` helper calls NSWorkspace per NSScreen, and since macOS 14
//! that reaches only the ACTIVE Mission Control Space of each display: the
//! rest of the desktop keeps whatever it had, and a Space created tomorrow
//! inherits none of it. The per-Space choices live in one binary plist —
//! `~/Library/Application Support/com.apple.wallpaper/Store/Index.plist` —
//! so `theme set` finishes the job here: it takes the record the helper just
//! wrote for the active Space and copies it into every other slot, seeding
//! the all-Spaces fallback so later Spaces inherit it too.
//!
//! Two rules shape the code. The store is Apple's, so its format is never
//! re-implemented: `/usr/bin/plutil` converts both ways and only the XML in
//! between is ours (a small in-house tree, the json.rs precedent — the
//! dependency budget here is zero). And the store is a file in the user's
//! home, so it is reached the way the saver reaches a library: audited
//! chain, descriptor-bound open, identity re-checked at the rename. The
//! screensaver (`Idle`) records are copied through untouched — a wallpaper
//! change may not silently pick an aerial.

use crate::save::{trusted_spawn, trusted_system_binary};
use rustix::fs::{AtFlags, Mode, OFlags};
use rustix::io::Errno;
use rustix::process::{Pid, Signal, kill_process};
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime};

const STORE: &str = "Library/Application Support/com.apple.wallpaper/Store/Index.plist";
const PLUTIL: &str = "/usr/bin/plutil";
const PGREP: &str = "/usr/bin/pgrep";
/// The store is a handful of kilobytes; four megabytes is a ceiling no
/// legitimate one approaches, and the XML expansion gets its own.
const MAX_STORE: u64 = 4 * 1024 * 1024;
const MAX_XML: u64 = 32 * 1024 * 1024;
/// The helper's own write may still be in flight when we look: the agent
/// persists it asynchronously. Twenty 50 ms looks is a second of patience.
const TRIES: u32 = 20;
/// Deepest nesting the parser will follow. The real store is eight levels;
/// the cap is what keeps a malformed file from recursing off the stack.
const MAX_DEPTH: usize = 64;

/// Copy the record the helper just wrote into every other slot of the store.
/// `helper_started` is the instant BEFORE the helper ran: it is what
/// separates the fresh record from a stale one naming the same image.
pub fn sync_all_spaces(img: &Path, helper_started: SystemTime) -> Result<(), String> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?);
    if !home.is_absolute() {
        return Err("HOME is not an absolute path".into());
    }
    let path = home.join(STORE);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {}
        // No per-Space store on this machine — nothing to carry anywhere.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("cannot look at the store: {e}")),
    }
    let dirfd = store_dir(path.parent().ok_or("the store has no folder")?)?;

    let img = std::fs::canonicalize(img).map_err(|e| format!("cannot resolve the image: {e}"))?;
    let uri = file_uri(&img);
    let cutoff = helper_started
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64 - 2)
        .unwrap_or(i64::MIN);
    let name = img.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let (tree, was, template) = load(&dirfd, &uri, cutoff, name)?;

    // Transactional in memory: the rewrite works on a clone, so a refusal
    // anywhere in it leaves the tree we read intact and nothing is written.
    let mut next = tree.clone();
    rewrite(&mut next, &template)?;
    let bytes = plutil("binary1", write_xml(&next).as_bytes())?;

    let paused = Paused::new(agent_pids());
    replace_store(&dirfd, &was, &bytes)?;
    paused.release();
    Ok(())
}

/// The store's folder, held open by descriptor. Same custody machinery the
/// saver and the update-check cache use — audited chain, a root-down
/// O_NOFOLLOW walk, and the fd's identity checked against the name again.
fn store_dir(dir: &Path) -> Result<OwnedFd, String> {
    let canon = std::fs::canonicalize(dir).map_err(|e| format!("cannot resolve the store: {e}"))?;
    crate::save::audit_chain(&canon, crate::save::native_platform())?;
    let fd = crate::update::open_chain_nofollow(&canon)
        .ok_or("the store folder could not be opened without following a symlink")?;
    let st = rustix::fs::fstat(&fd).map_err(|e| format!("cannot stat the store folder: {e}"))?;
    if !crate::update::fd_custody_ok(&st, rustix::process::getuid().as_raw()) {
        return Err("the store folder is not a directory you alone own".into());
    }
    let now = rustix::fs::stat(&canon).map_err(|e| format!("cannot re-check the store: {e}"))?;
    if now.st_dev != st.st_dev || now.st_ino != st.st_ino {
        return Err("the store folder moved underneath the sync".into());
    }
    Ok(fd)
}

/// Read + convert + parse, until the helper's record is on disk. After the
/// patience runs out, the newest record of the same image is accepted
/// whatever its date — the fill options may be a run behind, the image is
/// still right.
fn load(
    dirfd: &OwnedFd,
    uri: &str,
    cutoff: i64,
    name: &str,
) -> Result<(Plist, rustix::fs::Stat, Plist), String> {
    for attempt in 0..=TRIES {
        let (bytes, st) = read_store(dirfd)?;
        let xml = plutil("xml1", &bytes)?;
        let tree = parse_xml(std::str::from_utf8(&xml).map_err(|_| "plutil emitted no text")?)?;
        if let Some(t) = template(&tree, uri, Some(cutoff)) {
            return Ok((tree, st, t));
        }
        if attempt == TRIES {
            // Only the DATE is ever relaxed: an older record of this image
            // may carry a run-behind fill mode, but it is still this image.
            // Fail closed rather than copy somebody else's wallpaper.
            let t = template(&tree, uri, None).ok_or_else(|| {
                format!("the wallpaper helper did not record {name} in the store")
            })?;
            return Ok((tree, st, t));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("the store could not be read".into())
}

/// The store's bytes and the identity the rename will be checked against.
fn read_store(dirfd: &OwnedFd) -> Result<(Vec<u8>, rustix::fs::Stat), String> {
    let fd = rustix::fs::openat(
        dirfd,
        "Index.plist",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|e| format!("cannot open the store: {e}"))?;
    let st = rustix::fs::fstat(&fd).map_err(|e| format!("cannot stat the store: {e}"))?;
    if rustix::fs::FileType::from_raw_mode(st.st_mode) != rustix::fs::FileType::RegularFile
        || st.st_uid != rustix::process::getuid().as_raw()
        || st.st_nlink != 1
        || st.st_mode & 0o022 != 0
        || st.st_size as u64 > MAX_STORE
    {
        return Err("the store is not a plain unshared file you alone own".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::from(fd)
        .take(MAX_STORE)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read the store: {e}"))?;
    Ok((bytes, st))
}

/// Apple's own converter, both directions. Reached by absolute path and
/// spawned from an empty environment — the boundary every trusted child in
/// this tool starts from. The store outgrows a pipe buffer, so stdin is fed
/// from a thread rather than deadlocking against our own read of stdout.
fn plutil(format: &str, input: &[u8]) -> Result<Vec<u8>, String> {
    if !trusted_system_binary(PLUTIL) {
        return Err("no trusted /usr/bin/plutil".into());
    }
    let mut child = trusted_spawn(Path::new(PLUTIL))
        .args(["-convert", format, "-o", "-", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run plutil: {e}"))?;
    let mut sink = child.stdin.take().ok_or("plutil took no input")?;
    let data = input.to_vec();
    let feed = std::thread::spawn(move || sink.write_all(&data));
    let mut out = Vec::new();
    let read = child
        .stdout
        .take()
        .ok_or("plutil gave no output")?
        .take(MAX_XML)
        .read_to_end(&mut out);
    let status = child.wait().map_err(|e| format!("plutil failed: {e}"))?;
    let fed = feed
        .join()
        .map_err(|_| "the plutil feeder died".to_string())?;
    if read.is_err() || fed.is_err() || !status.success() {
        return Err(format!("plutil could not convert the store to {format}"));
    }
    Ok(out)
}

/// The URI Apple writes into a wallpaper choice, spelled CFURL's way:
/// `URL(fileURLWithPath:)` leaves the RFC 3986 unreserved set AND the
/// sub-delims plus ':' and '@' raw, percent-encoding the rest in uppercase.
/// The wider set is what makes `wallpaper (1).jpg` and `a+b.png` match.
fn file_uri(p: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut s = String::from("file://");
    for &b in p.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

// ----------------------------------------------------------------- the tree

/// An XML plist, only as much of one as this store needs. Text is kept
/// VERBATIM — a huge `<integer>` the store really carries would not survive
/// a trip through i64, and a date must come back out spelled as it went in.
#[derive(Clone, Debug, PartialEq)]
enum Plist {
    /// Order preserved, so an untouched tree round-trips byte-for-byte.
    Dict(Vec<(String, Plist)>),
    Array(Vec<Plist>),
    String(String),
    /// base64 as written, whitespace stripped.
    Data(String),
    Date(String),
    Integer(String),
    Real(String),
    Bool(bool),
}

impl Plist {
    fn get(&self, key: &str) -> Option<&Plist> {
        match self {
            Plist::Dict(e) => e.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Replace in place — keeping the key's position — or append.
    fn set(&mut self, key: &str, val: Plist) {
        if let Plist::Dict(e) = self {
            match e.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = val,
                None => e.push((key.to_string(), val)),
            }
        }
    }

    fn remove(&mut self, key: &str) {
        if let Plist::Dict(e) = self {
            e.retain(|(k, _)| k != key);
        }
    }
}

struct Xml<'a> {
    b: &'a [u8],
    i: usize,
}

/// Parse the XML plutil emits. Anything else — a truncated file, an element
/// this store has no business carrying, an entity that is not a character —
/// is an error, never a panic and never a guess.
fn parse_xml(text: &str) -> Result<Plist, String> {
    let mut p = Xml {
        b: text.as_bytes(),
        i: 0,
    };
    for prolog in ["<?xml", "<!DOCTYPE"] {
        p.ws();
        if p.b[p.i..].starts_with(prolog.as_bytes()) {
            p.tag()?;
        }
    }
    p.ws();
    if !p.eat("<plist") {
        return Err("not a plist".into());
    }
    p.tag()?;
    let v = p.value(0)?;
    p.ws();
    if !p.eat("</plist>") {
        return Err("missing </plist>".into());
    }
    p.ws();
    if p.i != p.b.len() {
        return Err("trailing content after </plist>".into());
    }
    Ok(v)
}

impl Xml<'_> {
    fn ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, s: &str) -> bool {
        let hit = self.b[self.i..].starts_with(s.as_bytes());
        self.i += if hit { s.len() } else { 0 };
        hit
    }

    /// Everything up to and including the next `>`, returned without it.
    fn tag(&mut self) -> Result<&str, String> {
        let start = self.i;
        while self.i < self.b.len() && self.b[self.i] != b'>' {
            self.i += 1;
        }
        if self.i == self.b.len() {
            return Err("unterminated tag".into());
        }
        self.i += 1;
        std::str::from_utf8(&self.b[start..self.i - 1]).map_err(|_| "tag is not text".into())
    }

    fn value(&mut self, depth: usize) -> Result<Plist, String> {
        if depth > MAX_DEPTH {
            return Err("nested too deep".into());
        }
        self.ws();
        if !self.eat("<") {
            return Err("expected an element".into());
        }
        let raw = self.tag()?.trim().to_string();
        let (name, empty) = match raw.strip_suffix('/') {
            Some(n) => (n.trim(), true),
            None => (raw.as_str(), false),
        };
        match (name, empty) {
            ("true", true) => Ok(Plist::Bool(true)),
            ("false", true) => Ok(Plist::Bool(false)),
            ("dict", true) => Ok(Plist::Dict(Vec::new())),
            ("array", true) => Ok(Plist::Array(Vec::new())),
            ("dict", false) => {
                let mut e = Vec::new();
                loop {
                    self.ws();
                    if self.eat("</dict>") {
                        return Ok(Plist::Dict(e));
                    }
                    if !self.eat("<key>") {
                        return Err("expected a <key> in a <dict>".into());
                    }
                    let k = self.text("</key>")?;
                    e.push((k, self.value(depth + 1)?));
                }
            }
            ("array", false) => {
                let mut v = Vec::new();
                loop {
                    self.ws();
                    if self.eat("</array>") {
                        return Ok(Plist::Array(v));
                    }
                    v.push(self.value(depth + 1)?);
                }
            }
            ("string", false) => Ok(Plist::String(self.text("</string>")?)),
            ("date", false) => Ok(Plist::Date(self.text("</date>")?)),
            ("integer", false) => Ok(Plist::Integer(self.text("</integer>")?)),
            ("real", false) => Ok(Plist::Real(self.text("</real>")?)),
            ("data", false) => Ok(Plist::Data(
                self.text("</data>")?.split_whitespace().collect::<String>(),
            )),
            _ => Err(format!("unexpected element <{name}>")),
        }
    }

    /// Text up to `close`, entities decoded. Element text never carries a
    /// raw `<`, so the first one must open the closing tag.
    fn text(&mut self, close: &str) -> Result<String, String> {
        let start = self.i;
        while self.i < self.b.len() && self.b[self.i] != b'<' {
            self.i += 1;
        }
        let raw = std::str::from_utf8(&self.b[start..self.i])
            .map_err(|_| "element text is not UTF-8".to_string())?
            .to_string();
        if !self.eat(close) {
            return Err(format!("expected {close}"));
        }
        unescape(&raw)
    }
}

fn unescape(s: &str) -> Result<String, String> {
    if !s.contains('&') {
        return Ok(s.to_string());
    }
    let mut out = String::new();
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let end = tail.find(';').ok_or("unterminated XML entity")?;
        match &tail[1..end] {
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "amp" => out.push('&'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            n => {
                let digits = n.strip_prefix('#').ok_or("unknown XML entity")?;
                let code = match digits.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16),
                    None => digits.parse(),
                }
                .map_err(|_| "malformed numeric XML entity".to_string())?;
                out.push(char::from_u32(code).ok_or("XML entity is not a character")?);
            }
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Emit plutil's own shape, so a tree we did not touch converts back to the
/// bytes it came from.
fn write_xml(v: &Plist) -> String {
    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n",
    );
    emit(v, 0, &mut s);
    s.push_str("</plist>\n");
    s
}

fn emit(v: &Plist, depth: usize, out: &mut String) {
    let pad = "\t".repeat(depth);
    let mut line = |body: &str| {
        out.push_str(&pad);
        out.push_str(body);
        out.push('\n');
    };
    match v {
        Plist::Bool(b) => line(if *b { "<true/>" } else { "<false/>" }),
        Plist::String(s) => line(&format!("<string>{}</string>", escape(s))),
        Plist::Date(s) => line(&format!("<date>{s}</date>")),
        Plist::Integer(s) => line(&format!("<integer>{s}</integer>")),
        Plist::Real(s) => line(&format!("<real>{s}</real>")),
        Plist::Data(b64) => {
            line("<data>");
            // Apple wraps base64 at column 76 counting each tab as eight, and
            // clamps the CONTENT's own indent at eight tabs — so a record
            // buried deeper than that still gets twelve characters a line,
            // indented less far than its own tags.
            let deep = depth.min(8);
            let chars: Vec<char> = b64.chars().collect();
            for chunk in chars.chunks(76 - 8 * deep) {
                out.push_str(&"\t".repeat(deep));
                out.extend(chunk.iter());
                out.push('\n');
            }
            out.push_str(&pad);
            out.push_str("</data>\n");
        }
        Plist::Array(a) if a.is_empty() => line("<array/>"),
        Plist::Array(a) => {
            line("<array>");
            for e in a {
                emit(e, depth + 1, out);
            }
            out.push_str(&pad);
            out.push_str("</array>\n");
        }
        Plist::Dict(e) if e.is_empty() => line("<dict/>"),
        Plist::Dict(e) => {
            line("<dict>");
            for (k, val) in e {
                out.push_str(&format!("{pad}\t<key>{}</key>\n", escape(k)));
                emit(val, depth + 1, out);
            }
            out.push_str(&pad);
            out.push_str("</dict>\n");
        }
    }
}

/// Strict base64, tolerating the line breaks the XML writer inserts.
fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let (mut out, mut acc, mut n, mut pad) = (Vec::new(), 0u32, 0u32, 0u32);
    for c in s.bytes() {
        let six = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                pad += 1;
                continue;
            }
            b' ' | b'\t' | b'\n' | b'\r' => continue,
            _ => return None,
        };
        if pad > 0 {
            return None;
        }
        acc = (acc << 6) | six as u32;
        n += 1;
        if n == 4 {
            out.extend_from_slice(&acc.to_be_bytes()[1..]);
            (acc, n) = (0, 0);
        }
    }
    if pad > 2 || (pad > 0 && n + pad != 4) {
        return None;
    }
    match n {
        0 => {}
        2 => out.push((acc >> 4) as u8),
        3 => out.extend_from_slice(&[(acc >> 10) as u8, (acc >> 2) as u8]),
        _ => return None,
    }
    Some(out)
}

/// `YYYY-MM-DDTHH:MM:SSZ` to UNIX seconds — the inverse of the civil-date
/// walk report.rs uses for the library table.
fn date_secs(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 20 || [b[4], b[7], b[13], b[16]] != *b"--::" {
        return None;
    }
    if b[10] != b'T' || b[19] != b'Z' {
        return None;
    }
    let num = |a: usize, z: usize| -> Option<i64> {
        let t = s.get(a..z)?;
        t.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| t.parse().ok())?
    };
    let (y, mo, d, h, mi, sec) = (
        num(0, 4)?,
        num(5, 7)?,
        num(8, 10)?,
        num(11, 13)?,
        num(14, 16)?,
        num(17, 19)?,
    );
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if mo > 2 { mo - 3 } else { mo + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * 86_400 + h * 3600 + mi * 60 + sec)
}

// -------------------------------------------------------------- the rewrite

/// Every value filed under a key named `Desktop`, anywhere — whatever its
/// type, so [`check_shape`] can refuse the ones we do not understand rather
/// than this walk hiding them.
fn desktops<'a>(v: &'a Plist, out: &mut Vec<&'a Plist>) {
    match v {
        Plist::Dict(e) => {
            for (k, val) in e {
                if k == "Desktop" {
                    out.push(val);
                }
                desktops(val, out);
            }
        }
        Plist::Array(a) => a.iter().for_each(|e| desktops(e, out)),
        _ => {}
    }
}

/// Does this record name our image? The helper writes the path inside the
/// nested binary plist under `Configuration`, so the URI must sit there as a
/// WHOLE string object — a bare substring would read a record naming
/// `…/new.jpg2` as one naming `…/new.jpg`. Older builds put the path in
/// `Files` as plain text instead, which is exact already.
fn names(rec: &Plist, uri: &str) -> bool {
    let choice = match rec.get("Content").and_then(|c| c.get("Choices")) {
        Some(Plist::Array(a)) => match a.first() {
            Some(c) => c,
            None => return false,
        },
        _ => return false,
    };
    if let Some(Plist::Data(b64)) = choice.get("Configuration")
        && let Some(bytes) = b64_decode(b64)
        && framed_string(&bytes, uri.as_bytes())
    {
        return true;
    }
    matches!(choice.get("Files"), Some(Plist::Array(fs))
        if fs.iter().any(|f| matches!(f.get("relative"), Some(Plist::String(r)) if r == uri)))
}

/// Does `blob` hold `want` as a COMPLETE binary-plist ASCII string object?
/// The length sits in the marker just before the bytes — `0x50|n` while it
/// fits, else its own integer — so a declared length equal to ours is what
/// proves the object ends where we do. (Percent-encoding leaves the URI
/// pure ASCII, so the UTF-16 markers cannot arise.)
fn framed_string(blob: &[u8], want: &[u8]) -> bool {
    let n = want.len();
    if n == 0 || blob.len() < n {
        return false;
    }
    let marker: Vec<u8> = match n {
        1..=14 => vec![0x50 | n as u8],
        15..=255 => vec![0x5F, 0x10, n as u8],
        _ if n <= u16::MAX as usize => {
            let [hi, lo] = (n as u16).to_be_bytes();
            vec![0x5F, 0x11, hi, lo]
        }
        _ => return false,
    };
    blob.windows(n)
        .enumerate()
        .any(|(at, w)| w == want && blob[..at].ends_with(&marker))
}

/// The freshest record of this image, or nothing. `after` is the helper's
/// start: with it, only a record the helper itself could have written
/// qualifies; without it, any record OF THE IMAGE will do — never another.
fn template(tree: &Plist, uri: &str, after: Option<i64>) -> Option<Plist> {
    let mut all = Vec::new();
    desktops(tree, &mut all);
    all.into_iter()
        .filter(|r| names(r, uri))
        .filter_map(|r| {
            let set = match r.get("LastSet") {
                Some(Plist::Date(d)) => date_secs(d),
                _ => None,
            };
            match (after, set) {
                (Some(floor), Some(t)) if t >= floor => Some((t, r)),
                (Some(_), _) => None,
                (None, t) => Some((t.unwrap_or(i64::MIN), r)),
            }
        })
        .max_by_key(|(t, _)| *t)
        .map(|(_, r)| r.clone())
}

/// Refuse before anything is written: every shape the rewrite goes on to
/// assume is checked here, so a store we do not recognise is one we leave
/// alone rather than half-convert.
fn check_shape(tree: &Plist) -> Result<(), String> {
    if !matches!(tree, Plist::Dict(_)) {
        return Err("the store's root is not a dictionary".into());
    }
    let odd = |v: &Plist| !matches!(v, Plist::Dict(_));
    if tree.get("AllSpacesAndDisplays").is_some_and(odd) {
        return Err("the store's all-Spaces slot is not a dictionary".into());
    }
    let mut slots = Vec::new();
    desktops(tree, &mut slots);
    if slots.into_iter().any(odd) {
        return Err("the store has a wallpaper slot that is not a dictionary".into());
    }
    Ok(())
}

/// Give every slot the template, and leave everything else exactly as it
/// was. Returns how many Desktop records were written.
fn rewrite(tree: &mut Plist, template: &Plist) -> Result<usize, String> {
    check_shape(tree)?;
    let mut n = 0;
    walk(tree, template, &mut n);
    // The slot System Settings' "Show on all Spaces" writes, and the one a
    // Space with no record of its own falls back to: seeding it is what
    // makes a Space created tomorrow inherit today's wallpaper.
    if let Plist::Dict(root) = tree
        && !root.iter().any(|(k, _)| k == "AllSpacesAndDisplays")
    {
        root.push(("AllSpacesAndDisplays".into(), Plist::Dict(Vec::new())));
    }
    if let Plist::Dict(root) = tree
        && let Some((_, all)) = root.iter_mut().find(|(k, _)| k == "AllSpacesAndDisplays")
        && matches!(all, Plist::Dict(_))
    {
        if all.get("Desktop").is_none() {
            all.set("Desktop", template.clone());
            n += 1;
        }
        if all.get("Idle").is_some() {
            all.set("Type", Plist::String("individual".into()));
        }
    }
    if n == 0 {
        return Err("the store holds no wallpaper slot to write".into());
    }
    Ok(n)
}

fn walk(v: &mut Plist, template: &Plist, n: &mut usize) {
    match v {
        Plist::Array(a) => a.iter_mut().for_each(|e| walk(e, template, n)),
        Plist::Dict(_) => {
            if let Plist::Dict(e) = v {
                e.iter_mut().for_each(|(_, val)| walk(val, template, n));
            }
            // A `linked` slot keeps ONE record for both the desktop and the
            // screensaver, so it has no Desktop key to replace. Split it:
            // the screensaver keeps the record it had, the desktop takes the
            // template.
            if matches!(v.get("Type"), Some(Plist::String(t)) if t == "linked") {
                if v.get("Idle").is_none()
                    && let Some(linked) = v.get("Linked").cloned()
                {
                    v.set("Idle", linked);
                }
                v.remove("Linked");
                v.set("Type", Plist::String("individual".into()));
                v.set("Desktop", template.clone());
            }
            // OVERLAY, not replace: a slot may carry keys this tool has never
            // heard of, and losing them is a change nobody asked for.
            if let Some(mut dest) = v.get("Desktop").cloned() {
                if let Plist::Dict(fields) = template {
                    for (k, val) in fields {
                        dest.set(k, val.clone());
                    }
                }
                v.set("Desktop", dest);
                *n += 1;
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------- the write

/// Replace the store through the audited descriptor: a fresh temp with the
/// original's mode, fsync, the identity of what we read re-checked, then
/// rename(2). Any failure unlinks the temp and the store is as it was. The
/// quarantine xattr is deliberately not carried over — the agent re-applies
/// its own.
fn replace_store(dirfd: &OwnedFd, was: &rustix::fs::Stat, bytes: &[u8]) -> Result<(), String> {
    let mut made = None;
    for seq in 0..8u32 {
        let name = format!(".Index.plist.theme.{}.{seq}", std::process::id());
        match rustix::fs::openat(
            dirfd,
            name.as_str(),
            OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW,
            Mode::from_raw_mode(was.st_mode & 0o777),
        ) {
            Ok(fd) => {
                made = Some((name, fd));
                break;
            }
            Err(Errno::EXIST) => continue,
            Err(e) => return Err(format!("cannot write into the store folder: {e}")),
        }
    }
    let (tmp, fd) = made.ok_or("the store folder is full of stale theme temporaries")?;
    let mut f = std::fs::File::from(fd);
    let done = (|| -> Result<(), String> {
        f.write_all(bytes)
            .map_err(|e| format!("cannot write the store: {e}"))?;
        f.sync_all().map_err(|e| format!("fsync failed: {e}"))?;
        let now = rustix::fs::statat(dirfd, "Index.plist", AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|e| format!("cannot re-check the store: {e}"))?;
        if (
            now.st_dev,
            now.st_ino,
            now.st_size,
            now.st_mtime,
            now.st_mtime_nsec,
        ) != (
            was.st_dev,
            was.st_ino,
            was.st_size,
            was.st_mtime,
            was.st_mtime_nsec,
        ) {
            return Err("the store changed underneath the sync".into());
        }
        rustix::fs::renameat(dirfd, tmp.as_str(), dirfd, "Index.plist")
            .map_err(|e| format!("cannot replace the store: {e}"))
    })();
    match done {
        Ok(()) => {
            let _ = rustix::fs::fsync(dirfd);
            Ok(())
        }
        Err(e) => {
            let _ = rustix::fs::unlinkat(dirfd, tmp.as_str(), AtFlags::empty());
            Err(e)
        }
    }
}

/// The user's own WallpaperAgent, by exact process name and our uid only —
/// no other process ever receives a signal from here.
fn agent_pids() -> Vec<Pid> {
    if !trusted_system_binary(PGREP) {
        return Vec::new();
    }
    let uid = rustix::process::getuid().as_raw().to_string();
    let Ok(out) = trusted_spawn(Path::new(PGREP))
        .args(["-x", "-u", &uid, "WallpaperAgent"])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit()))
        .filter_map(|l| l.parse().ok().and_then(Pid::from_raw))
        .collect()
}

/// SIGSTOP across the write so the agent cannot rewrite the store mid
/// transaction — and a guard rather than a pair of calls, because every
/// early exit owes it a SIGCONT.
struct Paused(Vec<Pid>);

impl Paused {
    fn new(pids: Vec<Pid>) -> Paused {
        for p in &pids {
            let _ = kill_process(*p, Signal::STOP);
        }
        Paused(pids)
    }

    fn release(mut self) {
        let pids = std::mem::take(&mut self.0);
        for p in &pids {
            let _ = kill_process(*p, Signal::CONT);
        }
        reload_agent(&pids);
    }
}

impl Drop for Paused {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = kill_process(*p, Signal::CONT);
        }
    }
}

/// Nothing documents a hot reload of the store, and every recipe that works
/// restarts the agent instead: launchd brings it straight back (keepalive 0,
/// on demand) and it reads what we just wrote. ESRCH is not a failure — the
/// agent may already be gone.
fn reload_agent(pids: &[Pid]) {
    for p in pids {
        let _ = kill_process(*p, Signal::TERM);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// plutil's own output for a tree carrying every value type this parser
    /// knows — generated by `plutil -convert xml1`, pasted verbatim, and the
    /// yardstick the writer is measured against. Tabs and the wrapped base64
    /// are load-bearing, `Deep` most of all: past eight levels Apple stops
    /// indenting the base64 with its own tags, which is exactly where a
    /// plausible writer reflows the whole store.
    const CANONICAL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Arr</key>
	<array>
		<string>x</string>
		<dict>
			<key>k</key>
			<string>v</string>
		</dict>
	</array>
	<key>Bytes</key>
	<data>
	ZmlsZTovLy9Vc2Vycy9leGFtcGxlL3dhbGxwYXBlcnMvdW5zcGxhc2gvYS12ZXJ5LWxv
	bmctcGhvdG8tbmFtZS5qcGc=
	</data>
	<key>D</key>
	<date>2026-09-03T06:44:55Z</date>
	<key>Deep</key>
	<array>
		<array>
			<array>
				<array>
					<array>
						<array>
							<array>
								<array>
									<data>
								ZmlsZTovLy9V
								c2Vycy9leGFt
								cGxlL3dhbGxw
								YXBlcnMvdW5z
								cGxhc2gvYS12
								ZXJ5LWxvbmct
								cGhvdG8tbmFt
								ZS5qcGc=
									</data>
								</array>
							</array>
						</array>
					</array>
				</array>
			</array>
		</array>
	</array>
	<key>Empty</key>
	<dict/>
	<key>EmptyArr</key>
	<array/>
	<key>EmptyData</key>
	<data>
	</data>
	<key>EmptyStr</key>
	<string></string>
	<key>Esc</key>
	<string>a &lt;b&gt; &amp; "c" 'd' é</string>
	<key>F</key>
	<false/>
	<key>Num</key>
	<integer>16172123445939666944</integer>
	<key>R</key>
	<real>1.5</real>
	<key>T</key>
	<true/>
</dict>
</plist>
"#;

    const NEW_URI: &str = "file:///Users/example/wallpapers/new.jpg";
    /// `bplist00` + the `0x5F 0x10 0x28` string marker + the URI — the
    /// framing the live store really uses, which `names` now demands.
    const NEW_B64: &str = "YnBsaXN0MDBfEChmaWxlOi8vL1VzZXJzL2V4YW1wbGUvd2FsbHBhcGVycy9uZXcuanBn";
    const OLD_B64: &str = "YnBsaXN0MDBfEChmaWxlOi8vL1VzZXJzL2V4YW1wbGUvd2FsbHBhcGVycy9vbGQuanBn";
    const AERIALS: &str = "com.apple.wallpaper.choice.aerials";

    fn d(items: Vec<(&str, Plist)>) -> Plist {
        Plist::Dict(items.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    fn s(v: &str) -> Plist {
        Plist::String(v.into())
    }

    /// A record shaped like the store's, under `provider`.
    fn rec(provider: &str, conf: &str, files: Plist, when: &str) -> Plist {
        let choice = d(vec![
            ("Configuration", Plist::Data(conf.into())),
            ("Files", files),
            ("Provider", s(provider)),
        ]);
        let content = d(vec![
            ("Choices", Plist::Array(vec![choice])),
            ("EncodedOptionValues", Plist::Data("ZmlsbA==".into())),
            ("Shuffle", s("$null")),
        ]);
        d(vec![
            ("Content", content),
            ("LastSet", Plist::Date(when.into())),
            ("LastUse", Plist::Date(when.into())),
        ])
    }

    const IMAGE: &str = "com.apple.wallpaper.choice.image";

    /// A wallpaper record: the path lives inside the Configuration blob.
    fn image(conf: &str, when: &str) -> Plist {
        rec(IMAGE, conf, Plist::Array(vec![]), when)
    }

    /// The screensaver record — a different provider and an empty
    /// configuration, the shape a wallpaper sync must never disturb.
    fn idle() -> Plist {
        rec(AERIALS, "", Plist::Array(vec![]), "2026-09-01T12:30:10Z")
    }

    fn linked() -> Plist {
        image(OLD_B64, "2026-08-01T00:00:00Z")
    }

    /// Two displays, three Spaces (one with its own per-display entry, one
    /// `linked`), a SystemDefault, an all-Spaces slot with only a
    /// screensaver, and unknown keys at two depths.
    fn store_fixture() -> Plist {
        let old = || image(OLD_B64, "2026-09-01T12:30:10Z");
        let slot = || {
            d(vec![
                ("Desktop", old()),
                ("Idle", idle()),
                ("Type", s("individual")),
            ])
        };
        let all = d(vec![("Idle", idle()), ("Type", s("idle"))]);
        let displays = d(vec![("DISPLAY-ONE", slot()), ("DISPLAY-TWO", slot())]);
        let two = d(vec![
            ("Default", slot()),
            ("Displays", d(vec![("DISPLAY-ONE", slot())])),
        ]);
        let three = d(vec![("Linked", linked()), ("Type", s("linked"))]);
        let spaces = d(vec![
            ("SPACE-ONE", d(vec![("Default", slot())])),
            ("SPACE-TWO", two),
            ("SPACE-THREE", d(vec![("Default", three)])),
        ]);
        let default = d(vec![
            ("Desktop", old()),
            ("Idle", idle()),
            ("Type", s("individual")),
            ("Unknown", s("keep me")),
        ]);
        d(vec![
            ("AllSpacesAndDisplays", all),
            ("Displays", displays),
            ("Spaces", spaces),
            ("SystemDefault", default),
            ("SomethingNew", s("keep me too")),
        ])
    }

    /// The writer must reproduce plutil's own bytes for a tree it did not
    /// touch — indentation, the base64 wrap, empty elements, entities. A
    /// writer that is merely "valid XML" would let plutil reflow the whole
    /// store on every set.
    #[test]
    fn an_untouched_tree_round_trips_byte_for_byte() {
        let tree = parse_xml(CANONICAL).expect("plutil's own output must parse");
        assert_eq!(write_xml(&tree), CANONICAL);
    }

    /// And plutil must accept what the writer emits — the half a fixture
    /// cannot prove on its own.
    #[test]
    fn plutil_accepts_what_the_writer_emits() {
        let tree = parse_xml(CANONICAL).unwrap();
        let bin = plutil("binary1", write_xml(&tree).as_bytes()).unwrap();
        let back = plutil("xml1", &bin).unwrap();
        assert_eq!(
            parse_xml(std::str::from_utf8(&back).unwrap()).unwrap(),
            tree
        );
    }

    #[test]
    fn rewrite_covers_every_slot_and_seeds_the_all_spaces_fallback() {
        let template = image(NEW_B64, "2026-09-03T06:44:55Z");
        let before = store_fixture();
        let mut tree = before.clone();
        let n = rewrite(&mut tree, &template).unwrap();

        // Six existing slots, the split `linked` one, and the fallback.
        assert_eq!(n, 8);
        let mut wrote = Vec::new();
        desktops(&tree, &mut wrote);
        assert_eq!(wrote.len(), 8);
        assert!(
            wrote.iter().all(|r| **r == template),
            "a slot kept its old image"
        );

        // The fallback a Space created later inherits.
        let all = tree.get("AllSpacesAndDisplays").unwrap();
        assert_eq!(all.get("Desktop"), Some(&template));
        assert_eq!(all.get("Type"), Some(&s("individual")));
        assert_eq!(all.get("Idle"), Some(&idle()));

        // Every screensaver survives, here and everywhere else.
        assert_eq!(
            write_xml(&tree).matches(AERIALS).count(),
            write_xml(&before).matches(AERIALS).count()
        );
        assert_eq!(
            tree.get("SystemDefault").unwrap().get("Idle"),
            Some(&idle())
        );

        // Unknown keys survive, at the root and inside a record.
        assert_eq!(tree.get("SomethingNew"), Some(&s("keep me too")));
        assert_eq!(
            tree.get("SystemDefault").unwrap().get("Unknown"),
            Some(&s("keep me"))
        );

        // The linked slot splits rather than losing its screensaver.
        let three = tree
            .get("Spaces")
            .unwrap()
            .get("SPACE-THREE")
            .unwrap()
            .get("Default")
            .unwrap();
        assert_eq!(three.get("Linked"), None);
        assert_eq!(three.get("Type"), Some(&s("individual")));
        assert_eq!(three.get("Idle"), Some(&linked()));
        assert_eq!(three.get("Desktop"), Some(&template));

        // A store with nothing to write is a refusal, not a silent no-op.
        assert!(rewrite(&mut s("not a store"), &template).is_err());
    }

    #[test]
    fn the_template_is_the_newest_record_of_this_image() {
        let slot = |v: Plist| d(vec![("Desktop", v)]);
        // Older builds record the path in Files as plain text instead of
        // inside the Configuration blob; both shapes must count.
        let by_files = |when: &str| {
            let files = Plist::Array(vec![d(vec![("relative", s(NEW_URI))])]);
            rec(IMAGE, "", files, when)
        };
        let floor = date_secs("2026-09-03T06:44:45Z").unwrap();
        let tree = d(vec![
            ("A", slot(image(NEW_B64, "2026-09-03T06:44:50Z"))),
            ("B", slot(image(NEW_B64, "2026-09-03T06:44:55Z"))),
            ("C", slot(by_files("2026-09-03T06:44:52Z"))),
            // A newer record of a DIFFERENT image must never win.
            ("D", slot(image(OLD_B64, "2026-09-03T06:44:59Z"))),
        ]);
        let won = template(&tree, NEW_URI, Some(floor)).unwrap();
        assert_eq!(
            won.get("LastSet"),
            Some(&Plist::Date("2026-09-03T06:44:55Z".into()))
        );

        // The Files shape on its own still qualifies.
        let only_files = slot(by_files("2026-09-03T06:44:52Z"));
        assert!(template(&only_files, NEW_URI, Some(floor)).is_some());

        // Another image never qualifies, whatever its date.
        let other = slot(image(OLD_B64, "2026-09-03T06:44:59Z"));
        assert!(template(&other, NEW_URI, Some(floor)).is_none());

        // A record older than the helper's run is skipped — until the
        // patience runs out and the date stops being a requirement.
        let stale = slot(image(NEW_B64, "2026-09-01T00:00:00Z"));
        assert!(template(&stale, NEW_URI, Some(floor)).is_none());
        assert!(template(&stale, NEW_URI, None).is_some());
    }

    /// Fail closed: only the DATE is ever relaxed. A record the helper
    /// plainly just wrote still loses if it names another image — copying
    /// somebody else's wallpaper across every Space is worse than refusing.
    #[test]
    fn a_foreign_record_is_never_the_template() {
        let (stale, fresh) = ("2026-09-01T00:00:00Z", "2026-09-03T06:44:55Z");
        let slot = |v: Plist| d(vec![("Desktop", v)]);
        let floor = date_secs("2026-09-03T06:44:45Z").unwrap();
        let tree = d(vec![
            ("Stale", slot(image(NEW_B64, stale))),
            ("Fresh", slot(image(OLD_B64, fresh))),
        ]);
        assert!(template(&tree, NEW_URI, Some(floor)).is_none());
        let ours = template(&tree, NEW_URI, None);
        assert_eq!(ours, Some(image(NEW_B64, stale)));
        let none = d(vec![("Fresh", slot(image(OLD_B64, fresh)))]);
        assert!(template(&none, NEW_URI, None).is_none());
    }

    /// The URI must sit in the blob as a WHOLE bplist string object. The
    /// length prefix is what proves it: a record naming a longer path that
    /// merely starts with ours is a different wallpaper.
    #[test]
    fn only_a_framed_string_object_attributes_a_record() {
        let uri = NEW_URI.as_bytes();
        let n = uri.len() as u8;
        let blob = |m: &[u8], tail: &[u8]| [b"bplist00".as_slice(), m, uri, tail].concat();
        assert!(framed_string(&blob(&[0x5F, 0x10, n], b""), uri));
        // One byte longer: the same prefix, a different file.
        assert!(!framed_string(&blob(&[0x5F, 0x10, n + 1], b"2"), uri));
        // Unframed, and framed with the wrong length marker.
        assert!(!framed_string(uri, uri));
        assert!(!framed_string(&blob(&[0x5F, 0x11, 0, n], b""), uri));
        // A short name carries its length in the marker itself.
        let short = b"file:///a.jpg";
        assert!(framed_string(&[&[0x5D][..], short].concat(), short));
        assert!(!framed_string(short, short));
    }

    /// A slot may carry fields this tool has never heard of — a future
    /// macOS key, a per-display tweak. The template overlays its own keys
    /// and leaves the rest standing.
    #[test]
    fn a_destination_record_keeps_the_keys_the_template_lacks() {
        let mut template = image(NEW_B64, "2026-09-03T06:44:55Z");
        template.remove("LastUse");
        let mut dest = image(OLD_B64, "2026-09-01T12:30:10Z");
        dest.set("Extra", s("mine"));
        let mut tree = d(vec![("SystemDefault", d(vec![("Desktop", dest.clone())]))]);
        rewrite(&mut tree, &template).unwrap();
        let got = tree.get("SystemDefault").unwrap().get("Desktop").unwrap();
        assert_eq!(got.get("Extra"), Some(&s("mine")));
        assert_eq!(got.get("LastUse"), dest.get("LastUse"));
        assert_eq!(got.get("Content"), template.get("Content"));
        assert_eq!(got.get("LastSet"), template.get("LastSet"));
    }

    /// A shape we do not recognise is refused whole — never half-converted.
    #[test]
    fn an_unrecognised_shape_is_refused_before_anything_is_written() {
        let template = image(NEW_B64, "2026-09-03T06:44:55Z");
        let slot = d(vec![("Desktop", template.clone())]);
        let cases = [
            // A root that is not a dictionary, nested Desktop and all.
            Plist::Array(vec![slot.clone()]),
            // An all-Spaces slot that is not a dictionary.
            d(vec![("AllSpacesAndDisplays", s("odd")), ("A", slot)]),
            // A Desktop that is not a record.
            d(vec![("SystemDefault", d(vec![("Desktop", s("odd"))]))]),
        ];
        for mut case in cases {
            let before = case.clone();
            assert!(rewrite(&mut case, &template).is_err());
            assert_eq!(case, before, "a refusal changed the tree");
        }
    }

    #[test]
    fn file_uri_percent_encodes_apples_way() {
        assert_eq!(
            file_uri(Path::new("/Users/example/plain-path_1.jpg")),
            "file:///Users/example/plain-path_1.jpg"
        );
        assert_eq!(
            file_uri(Path::new("/Users/example/a b.jpg")),
            "file:///Users/example/a%20b.jpg"
        );
        // Byte-faithful. Foundation spells non-ASCII decomposed, so a
        // composed name matches no record and the sync fails closed: the
        // active Space is set, the others are not, and the exit says so.
        assert_eq!(
            file_uri(Path::new("/Users/example/café.jpg")),
            "file:///Users/example/caf%C3%A9.jpg"
        );
        // CFURL keeps the sub-delims raw: only the space and the '%' move.
        assert_eq!(
            file_uri(Path::new("/tmp/a+b (1)&c%.png")),
            "file:///tmp/a+b%20(1)&c%25.png"
        );
        assert_eq!(file_uri(Path::new("/x/#a?.jpg")), "file:///x/%23a%3F.jpg");
    }

    #[test]
    fn base64_decodes_wrapped_text_and_refuses_junk() {
        assert!(b64_decode(NEW_B64).unwrap().ends_with(NEW_URI.as_bytes()));
        assert_eq!(b64_decode("aGVs\n\t bG8=").unwrap(), b"hello");
        assert_eq!(b64_decode("").unwrap(), b"");
        for bad in ["a", "!!!!", "aGVsbG8=x", "====", "aG=l", "aGVsbG8-"] {
            assert!(b64_decode(bad).is_none(), "accepted {bad:?}");
        }
    }

    #[test]
    fn dates_parse_as_utc_seconds() {
        assert_eq!(date_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(date_secs("2026-09-03T06:44:55Z"), Some(1_788_417_895));
        for bad in [
            "",
            "2026-09-03T06:44:55",
            "2026-13-03T06:44:55Z",
            "2026-09-03 06:44:55Z",
            "202X-09-03T06:44:55Z",
            "2026-09-03T06.44.55Z",
        ] {
            assert_eq!(date_secs(bad), None, "accepted {bad:?}");
        }
    }

    /// The store is Apple's file, not ours: every malformed shape must come
    /// back as an error. Nothing here may panic or run off the stack.
    #[test]
    fn hostile_xml_is_refused_never_panics() {
        let deep = format!(
            "<plist version=\"1.0\">{}{}</plist>",
            "<array>".repeat(200),
            "</array>".repeat(200)
        );
        let cases = [
            "",
            "<plist",
            "<plist version=\"1.0\">",
            "<plist version=\"1.0\"><dict>",
            "<plist version=\"1.0\"><dict><key>a</key></dict></plist>",
            "<plist version=\"1.0\"><array></dict></plist>",
            "<plist version=\"1.0\"><frobnicate/></plist>",
            "<plist version=\"1.0\"><string>&#99999999999;</string></plist>",
            "<plist version=\"1.0\"><string>&#xZZ;</string></plist>",
            "<plist version=\"1.0\"><string>&nope;</string></plist>",
            "<plist version=\"1.0\"><string>&amp</string></plist>",
            "<plist version=\"1.0\"><string>x</string></plist>junk",
            deep.as_str(),
        ];
        for bad in cases {
            assert!(parse_xml(bad).is_err(), "accepted {bad:?}");
        }
        // A surrogate is a number but not a character.
        assert!(parse_xml("<plist version=\"1.0\"><string>&#xD800;</string></plist>").is_err());
    }
}

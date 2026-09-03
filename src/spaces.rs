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
//!
//! Why launchd NAMES the agent here but never signals it: under System
//! Integrity Protection macOS refuses both service-level primitives for an
//! Apple LaunchAgent. Measured on macOS 26.6.2:
//!
//! ```text
//! $ launchctl kill SIGSTOP gui/501/com.apple.wallpaper.agent
//! Not privileged to signal service.
//! $ launchctl kickstart -k gui/501/com.apple.wallpaper.agent
//! Could not kickstart service "com.apple.wallpaper.agent": 150:
//! Operation not permitted while System Integrity Protection is engaged
//! ```
//!
//! `launchctl print` still answers, and kill(2) from the same uid still
//! reaches the pid it names. So launchd is the IDENTITY and the kernel is
//! the SIGNAL path: stop the pid launchd named, verify from the process
//! table that it really stopped, and re-ask launchd for that identity
//! before every later signal. Binding a pid any harder than that needs
//! privileges this tool does not have and will not ask for — which is also
//! why a pid is never carried forward on trust.

use crate::save::{trusted_spawn, trusted_system_binary};
use rustix::fs::{AtFlags, Mode, OFlags};
use rustix::io::Errno;
use rustix::process::{Pid, Signal, kill_process};
use std::cell::Cell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime};

const STORE: &str = "Library/Application Support/com.apple.wallpaper/Store/Index.plist";
const PLUTIL: &str = "/usr/bin/plutil";
const LAUNCHCTL: &str = "/bin/launchctl";
const LS: &str = "/bin/ls";
const PS: &str = "/bin/ps";
/// The agent is a launchd SERVICE — `gui/<uid>/com.apple.wallpaper.agent`,
/// a LaunchAgent running WallpaperAgent.app. Coordinating with the service
/// rather than with a pid is what makes the handshake atomic: launchd
/// relaunches it on demand, so a pid is only ever a snapshot.
const SERVICE: &str = "com.apple.wallpaper.agent";
/// The store is a handful of kilobytes; four megabytes is a ceiling no
/// legitimate one approaches, and the XML expansion gets its own.
const MAX_STORE: u64 = 4 * 1024 * 1024;
const MAX_XML: u64 = 32 * 1024 * 1024;
/// The helper's own write may still be in flight when we look: the agent
/// persists it asynchronously. Twenty 50 ms looks is a second of patience.
const TRIES: u32 = 20;
/// Deepest nesting either reader will follow. The real store is eight levels
/// and a Configuration blob three; the cap is what keeps a malformed file —
/// or a reference cycle, which only a binary plist can express — from
/// recursing off the stack.
const MAX_DEPTH: usize = 64;
/// Ceilings for the nested binary plists: those blobs are a few hundred
/// bytes, so both sit orders of magnitude above anything legitimate.
const MAX_OBJECTS: usize = 4096;
const MAX_ITEMS: usize = 1024;
/// Objects one SYNC may spend decoding, across every blob it looks at and
/// every retry it makes. Depth and object count bound a blob's SHAPE but not
/// the WORK it asks for: references may be shared, so a blob whose every
/// level points at the same few objects re-walks them once per path —
/// exponential while still shallow, small and cycle-free — and a store full
/// of such blobs multiplies that again. A legitimate plist visits each
/// object about once, so sixteen visits apiece is generous cover for shared
/// keys and values; each blob is additionally held to that much of its OWN
/// object count, so one blob cannot spend the whole sync's allowance.
const MAX_EVALS: usize = MAX_OBJECTS * 16;
const EVALS_PER_OBJECT: usize = 16;

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
    sync_with(&Launchd, &has_acl, &dirfd, &path, &uri, cutoff, name)
}

/// The transaction proper, over its two seams — the agent and the ACL probe
/// — so the whole sequence has tests that neither reach launchd nor go near
/// a real store.
fn sync_with(
    agent: &dyn Agent,
    probe: &dyn Fn(&Path) -> Result<bool, String>,
    dirfd: &OwnedFd,
    path: &Path,
    uri: &str,
    cutoff: i64,
    name: &str,
) -> Result<(), String> {
    // The agent has to be RUNNING for the helper's record to reach the store
    // at all, so it is paused only after that record is found — and the
    // waiting is the point: `load` can spend a second looking, which is a
    // second for the instance to change. So the identity is read HERE, with
    // the STOP one statement away, and it is the only read there is.
    let (was, template) = load(dirfd, uri, cutoff, name)?;
    let paused = match Paused::new(agent, agent.pid()?) {
        Ok(guard) => guard,
        // A pause that could not be confirmed may still owe a resume; when
        // it does it comes back as a guard, and it is released like any
        // other rather than dropped on the floor.
        Err((why, None)) => return Err(why),
        Err((why, Some(guard))) => return finish(guard, Err(why)),
    };
    // The instance everything after this binds to: a DIFFERENT one at the
    // rename means the agent died and relaunched mid-flight, and a fresh
    // agent holds the old store in memory.
    let instance = paused.instance();
    // Everything from here to the rename is one fallible block, so there is
    // exactly ONE exit — through `finish`, which always releases the guard
    // and never lets a teardown failure hide what really went wrong.
    let primary = (|| -> Result<(), String> {
        // Read AGAIN behind the pause: whatever the agent wrote between the
        // two reads is a store we never inspected.
        let (bytes, now, src) = read_store(dirfd)?;
        if !same_file(&now, &was) {
            return Err("the store changed while pausing the agent".into());
        }
        copyable(path, &src, probe, &now)?;
        // Transactional in memory: the rewrite is a local tree, so a refusal
        // anywhere in it means nothing was ever handed to the writer.
        let mut tree = parse_store(&bytes)?;
        rewrite(&mut tree, &template)?;
        let out = plutil("binary1", write_xml(&tree).as_bytes())?;
        // Re-asked with the rename one statement away, because everything
        // above took time somebody else could have used.
        let before_rename = || {
            if agent.pid()? != instance {
                return Err("the wallpaper agent restarted during the sync".into());
            }
            copyable(path, &src, probe, &now)
        };
        replace_store(dirfd, &src, &now, &out, &before_rename)
    })();
    finish(paused, primary)
}

/// May this store still be replaced faithfully? The ACL probe takes a PATH,
/// so its answer is only worth having while that path still names the file
/// the descriptor holds — otherwise it describes some other file entirely.
/// The metadata the replacement copies is re-checked in the same breath.
fn copyable(
    path: &Path,
    fd: &OwnedFd,
    probe: &dyn Fn(&Path) -> Result<bool, String>,
    was: &rustix::fs::Stat,
) -> Result<(), String> {
    let acl = probe(path)?;
    let named = rustix::fs::stat(path).map_err(|e| format!("cannot re-check the store: {e}"))?;
    let held = rustix::fs::fstat(fd).map_err(|e| format!("cannot re-check the store: {e}"))?;
    if !same_object(&named, &held) {
        return Err("the store moved while it was being checked".into());
    }
    if (held.st_mode & 0o7777, held.st_gid, held.st_flags)
        != (was.st_mode & 0o7777, was.st_gid, was.st_flags)
    {
        return Err("the store's mode, group or flags changed during the sync".into());
    }
    match uncopyable(held.st_flags, acl) {
        Some(why) => Err(why.into()),
        None => Ok(()),
    }
}

/// One file by identity alone — what makes a path's answer belong to a
/// descriptor.
fn same_object(a: &rustix::fs::Stat, b: &rustix::fs::Stat) -> bool {
    (a.st_dev, a.st_ino) == (b.st_dev, b.st_ino)
}

/// Release the guard and report BOTH halves. A teardown failure may never
/// replace the reason we were tearing down, and may never be swallowed
/// either: a store written behind an agent that would not resume is still a
/// desktop that does not repaint.
fn finish(paused: Paused, primary: Result<(), String>) -> Result<(), String> {
    match (primary, paused.release()) {
        (Ok(()), cleanup) => cleanup,
        (Err(e), Ok(())) => Err(e),
        (Err(e), Err(c)) => Err(format!(
            "{e}; and the wallpaper agent could not be resumed: {c}"
        )),
    }
}

/// Metadata a replacement could not carry, or nothing. BSD flags need
/// chflags and an ACL needs acl(3): neither has a safe binding here, and
/// this crate denies unsafe code. Losing either silently would be the real
/// failure, so the store is refused instead — the honest alternative to
/// unsafe FFI. A stock store carries neither. Pure over its inputs so both
/// arms are testable without root, the way `fd_custody_ok` is.
fn uncopyable(flags: u32, acl: bool) -> Option<&'static str> {
    match (flags != 0, acl) {
        (true, _) => Some("the store carries BSD flags this tool does not copy"),
        (_, true) => Some("the store carries an ACL this tool does not copy"),
        _ => None,
    }
}

/// Is an ACL attached? macOS gives up this answer grudgingly: the
/// `com.apple.system.Security` pseudo-attribute is EPERM to read whether or
/// not an ACL exists (measured both ways, so it can distinguish nothing),
/// `flistxattr` never lists it, and acl(3) needs unsafe FFI. What is left
/// is Apple's own interrogator, reached exactly as the library audit
/// reaches it — absolute path, root-owned binary, empty environment — which
/// prints one numbered ACE line per entry. The PATH here is advisory only:
/// it decides whether to proceed, never where to write, and the replacement
/// stays bound to the descriptor the store was read through.
fn has_acl(path: &Path) -> Result<bool, String> {
    if !trusted_system_binary(LS) {
        return Err("no trusted /bin/ls to check the store for an ACL".into());
    }
    let out = trusted_spawn(Path::new(LS))
        .arg("-lde")
        .arg(path)
        .output()
        .map_err(|e| format!("cannot check the store for an ACL: {e}"))?;
    if !out.status.success() {
        return Err("cannot check the store for an ACL: ls -lde failed".into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).lines().any(ace_line))
}

/// `N: <who> allow|deny <perms>` — the shape `ls -lde` gives each entry,
/// and the one thing that separates an ACL'd file from a plain one.
fn ace_line(l: &str) -> bool {
    matches!(l.trim_start().split_once(':'),
        Some((n, _)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The same file, by identity AND content age: nothing about the store may
/// have moved between the read we based a rewrite on and the write.
fn same_file(a: &rustix::fs::Stat, b: &rustix::fs::Stat) -> bool {
    (a.st_dev, a.st_ino, a.st_size, a.st_mtime, a.st_mtime_nsec)
        == (b.st_dev, b.st_ino, b.st_size, b.st_mtime, b.st_mtime_nsec)
}

/// The store's bytes as a tree: Apple's converter, then our own parser.
fn parse_store(bytes: &[u8]) -> Result<Plist, String> {
    let xml = plutil("xml1", bytes)?;
    parse_xml(std::str::from_utf8(&xml).map_err(|_| "plutil emitted no text")?)
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
) -> Result<(rustix::fs::Stat, Plist), String> {
    // ONE allowance for the whole sync — every attempt, every record, every
    // blob draws on it, so twenty-one retries over a hostile store cannot
    // multiply the bound by twenty-one.
    let budget = Cell::new(MAX_EVALS);
    for attempt in 0..=TRIES {
        let (bytes, st, _) = read_store(dirfd)?;
        let tree = parse_store(&bytes)?;
        if let Some(t) = template(&tree, uri, Some(cutoff), &budget) {
            return Ok((st, t));
        }
        if attempt == TRIES {
            // Only the DATE is ever relaxed: an older record of this image
            // may carry a run-behind fill mode, but it is still this image.
            // Fail closed rather than copy somebody else's wallpaper.
            let t = template(&tree, uri, None, &budget).ok_or_else(|| {
                format!("the wallpaper helper did not record {name} in the store")
            })?;
            return Ok((st, t));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("the store could not be read".into())
}

/// The store's bytes, the identity the rename will be checked against, and
/// the descriptor itself — the replacement copies its metadata from that fd,
/// not from a second lookup of the name.
fn read_store(dirfd: &OwnedFd) -> Result<(Vec<u8>, rustix::fs::Stat, OwnedFd), String> {
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
    // Judged HERE, before the agent is ever paused, so a store we cannot
    // faithfully replace costs nothing to refuse. (The ACL half of the
    // question needs a path and is asked once, in `sync_all_spaces`.)
    if let Some(why) = uncopyable(st.st_flags, false) {
        return Err(why.into());
    }
    let mut bytes = Vec::new();
    let mut f = std::fs::File::from(fd);
    (&mut f)
        .take(MAX_STORE)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read the store: {e}"))?;
    Ok((bytes, st, OwnedFd::from(f)))
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

    fn get_mut(&mut self, key: &str) -> Option<&mut Plist> {
        match self {
            Plist::Dict(e) => e.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
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

/// base64 for the reader below: a `<data>` object decoded out of a nested
/// plist is held the way the XML tree holds every other one.
fn b64_encode(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            match i > c.len() {
                true => out.push('='),
                false => out.push(A[(n >> (18 - 6 * i)) as usize & 63] as char),
            }
        }
    }
    out
}

/// Big-endian integer of up to eight bytes, or nothing.
fn be(b: &[u8]) -> Option<usize> {
    if b.len() > 8 {
        return None;
    }
    b.iter()
        .try_fold(0usize, |v, &x| v.checked_mul(256)?.checked_add(x as usize))
}

/// A bounded reader for the `bplist00` blobs NESTED inside the store. The
/// Configuration of a wallpaper choice is one, and reading its fields is the
/// only honest way to know a record chooses our image rather than merely
/// mentioning it.
///
/// Bounded everywhere, because this is somebody else's file: the trailer,
/// every table offset and every object reference are range-checked against
/// the blob, the graph is followed only to [`MAX_DEPTH`] so a cycle
/// terminates, and container counts are capped. Types this tree cannot hold
/// exactly — null, uid, fill, which XML plists have no syntax for — refuse
/// rather than decode into a lie. A refusal is never a match, which is the
/// direction that fails closed.
fn bplist<'a>(blob: &'a [u8], budget: &'a Cell<usize>) -> Option<Plist> {
    if !blob.starts_with(b"bplist00") || blob.len() < 40 {
        return None;
    }
    let trailer = &blob[blob.len() - 32..];
    let (off_size, ref_size) = (trailer[6] as usize, trailer[7] as usize);
    let (count, top, table) = (
        be(&trailer[8..16])?,
        be(&trailer[16..24])?,
        be(&trailer[24..32])?,
    );
    if !(1..=8).contains(&off_size) || !(1..=8).contains(&ref_size) {
        return None;
    }
    if count > MAX_OBJECTS || top >= count || table < 8 {
        return None;
    }
    let end = table.checked_add(count.checked_mul(off_size)?)?;
    if end > blob.len() - 32 {
        return None;
    }
    let offsets: Option<Vec<usize>> = (0..count)
        .map(|i| be(&blob[table + i * off_size..table + (i + 1) * off_size]))
        .collect();
    let offsets = offsets?;
    Bin {
        b: blob,
        offsets,
        ref_size,
        mine: Cell::new(count.checked_mul(EVALS_PER_OBJECT)?),
        shared: budget,
    }
    .object(top, 0)
}

struct Bin<'a> {
    b: &'a [u8],
    offsets: Vec<usize>,
    ref_size: usize,
    /// This blob's own allowance, its object count times
    /// [`EVALS_PER_OBJECT`] — so one blob cannot spend the sync's.
    mine: Cell<usize>,
    /// What is left of [`MAX_EVALS`] for the whole sync.
    shared: &'a Cell<usize>,
}

impl Bin<'_> {
    fn object(&self, idx: usize, depth: usize) -> Option<Plist> {
        if depth > MAX_DEPTH {
            return None;
        }
        // One budget for the entire decode, not per branch: that is what
        // makes a fan-out blob cost arithmetic rather than exponential work.
        // And one for the whole sync above it, so a STORE full of such blobs
        // cannot multiply the bound by its slot count.
        self.mine.set(self.mine.get().checked_sub(1)?);
        self.shared.set(self.shared.get().checked_sub(1)?);
        let at = *self.offsets.get(idx)?;
        let marker = *self.b.get(at)?;
        let low = (marker & 0x0F) as usize;
        match marker >> 4 {
            // Singletons: only the two booleans have a home in this tree.
            0x0 => match marker {
                0x08 => Some(Plist::Bool(false)),
                0x09 => Some(Plist::Bool(true)),
                _ => None,
            },
            // Widths are 2^nnnn; the eight-byte form is the signed one.
            0x1 => {
                let n = 1usize << low.min(4);
                let v = be(self.b.get(at + 1..at + 1 + n)?)?;
                Some(Plist::Integer(if n == 8 {
                    (v as i64).to_string()
                } else {
                    v.to_string()
                }))
            }
            0x2 => match self.b.get(at + 1..at + 1 + (1usize << low.min(4)))? {
                b if b.len() == 4 => Some(Plist::Real(
                    f32::from_be_bytes(b.try_into().ok()?).to_string(),
                )),
                b if b.len() == 8 => Some(Plist::Real(
                    f64::from_be_bytes(b.try_into().ok()?).to_string(),
                )),
                _ => None,
            },
            // Seconds since 2001-01-01, spelled the way the XML tree spells
            // every other date.
            0x3 if marker == 0x33 => {
                let secs = f64::from_be_bytes(self.b.get(at + 1..at + 9)?.try_into().ok()?);
                iso_date(secs as i64 + 978_307_200)
            }
            0x4 => {
                let (n, body) = self.count(at, low)?;
                Some(Plist::Data(b64_encode(self.b.get(body..body + n)?)))
            }
            0x5 => {
                let (n, body) = self.count(at, low)?;
                let raw = self.b.get(body..body + n)?;
                raw.is_ascii()
                    .then(|| String::from_utf8(raw.to_vec()).ok())?
                    .map(Plist::String)
            }
            0x6 => {
                let (n, body) = self.count(at, low)?;
                let raw = self.b.get(body..body + n.checked_mul(2)?)?;
                let units: Vec<u16> = raw
                    .chunks(2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect();
                String::from_utf16(&units).ok().map(Plist::String)
            }
            // Arrays and sets alike are ordered lists to this tree.
            0xA | 0xC => {
                let (n, body) = self.count(at, low)?;
                let refs = self.b.get(body..body + n.checked_mul(self.ref_size)?)?;
                refs.chunks(self.ref_size)
                    .map(|r| self.object(be(r)?, depth + 1))
                    .collect::<Option<Vec<_>>>()
                    .map(Plist::Array)
            }
            0xD => {
                let (n, body) = self.count(at, low)?;
                let width = n.checked_mul(self.ref_size)?;
                let keys = self.b.get(body..body + width)?;
                let vals = self.b.get(body + width..body + width.checked_mul(2)?)?;
                let mut out = Vec::new();
                for (k, v) in keys.chunks(self.ref_size).zip(vals.chunks(self.ref_size)) {
                    match self.object(be(k)?, depth + 1)? {
                        Plist::String(name) => out.push((name, self.object(be(v)?, depth + 1)?)),
                        _ => return None,
                    }
                }
                Some(Plist::Dict(out))
            }
            _ => None,
        }
    }

    /// A sized object's element count and where its body starts: `nnnn`, or
    /// a spelled-out integer object when `nnnn` is 0xF.
    fn count(&self, at: usize, low: usize) -> Option<(usize, usize)> {
        let (n, body) = if low != 0x0F {
            (low, at + 1)
        } else {
            let m = *self.b.get(at + 1)?;
            if m >> 4 != 0x1 {
                return None;
            }
            let w = 1usize << (m & 0x0F).min(4);
            (be(self.b.get(at + 2..at + 2 + w)?)?, at + 2 + w)
        };
        (n <= MAX_ITEMS).then_some((n, body))
    }
}

/// UNIX seconds as `YYYY-MM-DDTHH:MM:SSZ` — the inverse of [`date_secs`],
/// so a date read out of a nested plist reads like every other one here.
fn iso_date(t: i64) -> Option<Plist> {
    let (days, rest) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    Some(Plist::Date(format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )))
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

/// The slots this store documents, and ONLY those: `Displays/<uuid>`,
/// `Spaces/<uuid>/Default`, `Spaces/<uuid>/Displays/<uuid>`,
/// `SystemDefault` and `AllSpacesAndDisplays`. Scoping by PATH rather than
/// by the presence of a `Desktop` key is what keeps this tool out of
/// subtrees it does not own — a future macOS state that happens to be
/// shaped like a slot is not one, and rewriting it would be a change
/// nobody asked for. Whatever a slot's `Desktop` turns out to be is
/// returned as it is, so [`check_shape`] can refuse what it does not
/// understand rather than this walk hiding it.
fn slots(tree: &Plist) -> Vec<&Plist> {
    let mut out = Vec::new();
    out.extend(tree.get("SystemDefault"));
    out.extend(tree.get("AllSpacesAndDisplays"));
    if let Some(Plist::Dict(displays)) = tree.get("Displays") {
        out.extend(displays.iter().map(|(_, v)| v));
    }
    if let Some(Plist::Dict(spaces)) = tree.get("Spaces") {
        for (_, space) in spaces {
            out.extend(space.get("Default"));
            if let Some(Plist::Dict(per)) = space.get("Displays") {
                out.extend(per.iter().map(|(_, v)| v));
            }
        }
    }
    out
}

/// The same five paths, for the rewrite. A visitor rather than a list
/// because each slot is borrowed mutably in turn, and they may not be
/// borrowed all at once.
fn for_each_slot(tree: &mut Plist, mut f: impl FnMut(&mut Plist)) {
    for key in ["SystemDefault", "AllSpacesAndDisplays"] {
        if let Some(slot) = tree.get_mut(key) {
            f(slot);
        }
    }
    if let Some(Plist::Dict(displays)) = tree.get_mut("Displays") {
        displays.iter_mut().for_each(|(_, v)| f(v));
    }
    if let Some(Plist::Dict(spaces)) = tree.get_mut("Spaces") {
        for (_, space) in spaces.iter_mut() {
            if let Some(slot) = space.get_mut("Default") {
                f(slot);
            }
            if let Some(Plist::Dict(per)) = space.get_mut("Displays") {
                per.iter_mut().for_each(|(_, v)| f(v));
            }
        }
    }
}

/// The `Desktop` record of every documented slot — the only records this
/// tool reads a template from or writes one into.
fn desktops(tree: &Plist) -> Vec<&Plist> {
    slots(tree)
        .into_iter()
        .filter_map(|s| s.get("Desktop"))
        .collect()
}

/// Does this record name our image? The `Configuration` blob is a nested
/// binary plist, and the answer is read from its FIELDS — an `imageFile`
/// choice whose `url.relative` IS our URI — never from the bytes it happens
/// to contain. A record can mention a path in a field that does not choose
/// it, so anything short of parsing attributes the wrong wallpaper. Older
/// builds put the path in `Files` as plain text instead, already exact.
fn names<'a>(
    rec: &'a Plist,
    uri: &str,
    memo: &mut HashMap<&'a str, bool>,
    budget: &Cell<usize>,
) -> bool {
    let choice = match rec.get("Content").and_then(|c| c.get("Choices")) {
        Some(Plist::Array(a)) => match a.first() {
            Some(c) => c,
            None => return false,
        },
        _ => return false,
    };
    // Slots repeat the same blob across every Space and display, so one scan
    // parses each distinct Configuration once however many carry it.
    if let Some(Plist::Data(b64)) = choice.get("Configuration") {
        let hit = match memo.get(b64.as_str()) {
            Some(&known) => known,
            None => {
                let hit = chooses(b64, uri, budget);
                memo.insert(b64.as_str(), hit);
                hit
            }
        };
        if hit {
            return true;
        }
    }
    matches!(choice.get("Files"), Some(Plist::Array(fs))
        if fs.iter().any(|f| matches!(f.get("relative"), Some(Plist::String(r)) if r == uri)))
}

/// Does this Configuration blob CHOOSE `uri`? A blob that does not contain
/// the bytes anywhere cannot name them, and that test costs a scan rather
/// than a parse — so it stands in front of the decode as a gate. The decode
/// is still what decides.
fn chooses(b64: &str, uri: &str, budget: &Cell<usize>) -> bool {
    let Some(bytes) = b64_decode(b64) else {
        return false;
    };
    if !bytes.windows(uri.len()).any(|w| w == uri.as_bytes()) {
        return false;
    }
    let Some(conf) = bplist(&bytes, budget) else {
        return false;
    };
    matches!(conf.get("type"), Some(Plist::String(t)) if t == "imageFile")
        && matches!(conf.get("url").and_then(|u| u.get("relative")),
                    Some(Plist::String(r)) if r == uri)
}

/// The freshest record of this image, or nothing. `after` is the helper's
/// start: with it, only a record the helper itself could have written
/// qualifies; without it, any record OF THE IMAGE will do — never another.
///
/// The filters run cheapest-first, and that ordering is load-bearing: the
/// date is a field already in hand, attribution is a parse of somebody
/// else's bytes. A store whose records are all stale must cost no decodes
/// at all.
fn template(tree: &Plist, uri: &str, after: Option<i64>, budget: &Cell<usize>) -> Option<Plist> {
    let mut memo = HashMap::new();
    desktops(tree)
        .into_iter()
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
        .filter(|(_, r)| names(r, uri, &mut memo, budget))
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
    if desktops(tree).into_iter().any(odd) {
        return Err("the store has a wallpaper slot that is not a dictionary".into());
    }
    Ok(())
}

/// Give every slot the template, and leave everything else exactly as it
/// was. Returns how many Desktop records were written.
fn rewrite(tree: &mut Plist, template: &Plist) -> Result<usize, String> {
    check_shape(tree)?;
    let mut n = 0;
    for_each_slot(tree, |slot| rewrite_slot(slot, template, &mut n));
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
        let seeded = all.get("Desktop").is_none();
        if seeded {
            all.set("Desktop", template.clone());
            n += 1;
        }
        // A slot we just gave a desktop is no longer the "idle" shape, with
        // or without an Idle record beside it — leaving Type as "idle" is
        // what made the seeded wallpaper invisible. A slot that already had
        // a Desktop and no Idle keeps whatever Type it had: not ours to
        // invent.
        if seeded || all.get("Idle").is_some() {
            all.set("Type", Plist::String("individual".into()));
        }
    }
    if n == 0 {
        return Err("the store holds no wallpaper slot to write".into());
    }
    Ok(n)
}

/// What a wallpaper change actually consists of. Everything else a record
/// carries — `LastUse` included, which is the agent's business and not
/// ours — belongs to whoever put it there.
const OVERLAID: [&str; 2] = ["Content", "LastSet"];

fn rewrite_slot(slot: &mut Plist, template: &Plist, n: &mut usize) {
    // A `linked` slot keeps ONE record for both the desktop and the
    // screensaver, so it has no Desktop key to replace. Split it: the
    // screensaver keeps the record it had, the desktop takes the template
    // whole, there being no destination record to preserve.
    if matches!(slot.get("Type"), Some(Plist::String(t)) if t == "linked") {
        if slot.get("Idle").is_none()
            && let Some(linked) = slot.get("Linked").cloned()
        {
            slot.set("Idle", linked);
        }
        slot.remove("Linked");
        slot.set("Type", Plist::String("individual".into()));
        slot.set("Desktop", template.clone());
    }
    // OVERLAY, and only the two fields that ARE the wallpaper: a record may
    // carry state this tool has never heard of, and replacing it wholesale
    // would rewrite facts that were never ours to state.
    if let Some(mut dest) = slot.get("Desktop").cloned() {
        for key in OVERLAID {
            if let Some(v) = template.get(key) {
                dest.set(key, v.clone());
            }
        }
        slot.set("Desktop", dest);
        *n += 1;
    }
}

// ---------------------------------------------------------------- the write

/// Replace the store through the audited descriptor: a fresh temp carrying
/// the original's mode, group and extended attributes, fsync, the identity
/// of what we read re-checked, then rename(2) and an fsync of the folder.
/// Any failure unlinks the temp and the store is as it was.
///
/// ACLs and BSD flags are deliberately NOT carried: neither has a safe
/// binding here and this crate denies unsafe code, so copying them would
/// cost the guarantee that buys everything else. The store carries neither
/// on a stock system — and a refusal is not the failure mode, an
/// unannounced one is, which is why every attribute copy below is fatal.
fn replace_store(
    dirfd: &OwnedFd,
    src: &OwnedFd,
    was: &rustix::fs::Stat,
    bytes: &[u8],
    before_rename: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
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
    let name = tmp.as_str();
    let done = (move || -> Result<(), String> {
        // Group FIRST: a chown may clear setuid/setgid bits, so the mode has
        // to be the LAST of the two if it is to stick. The owner stays us;
        // a refusal is only tolerable when the group is already right.
        if let Err(e) = rustix::fs::fchown(&fd, None, Some(rustix::fs::Gid::from_raw(was.st_gid)))
            && !(e == Errno::PERM && rustix::fs::fstat(&fd).map(|s| s.st_gid) == Ok(was.st_gid))
        {
            return Err(format!("cannot set the store's group: {e}"));
        }
        // umask filters open(2)'s mode but never fchmod(2), so the exact
        // permission bits are set through the descriptor.
        rustix::fs::fchmod(&fd, Mode::from_raw_mode(was.st_mode & 0o7777))
            .map_err(|e| format!("cannot set the store's mode: {e}"))?;
        // Neither call is taken on trust: the replacement must ALREADY be
        // the file it is about to become before the rename makes it so.
        let got = rustix::fs::fstat(&fd).map_err(|e| format!("cannot re-check the temp: {e}"))?;
        if (got.st_mode & 0o7777, got.st_gid) != (was.st_mode & 0o7777, was.st_gid) {
            return Err("the replacement did not take the store's mode and group".into());
        }
        copy_xattrs(src, &fd)?;
        let mut f = std::fs::File::from(fd);
        f.write_all(bytes)
            .map_err(|e| format!("cannot write the store: {e}"))?;
        f.sync_all().map_err(|e| format!("fsync failed: {e}"))?;
        let now = rustix::fs::statat(dirfd, "Index.plist", AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|e| format!("cannot re-check the store: {e}"))?;
        if !same_file(&now, was) {
            return Err("the store changed underneath the sync".into());
        }
        // The caller's last word, with the rename one statement away.
        before_rename()?;
        rustix::fs::renameat(dirfd, name, dirfd, "Index.plist")
            .map_err(|e| format!("cannot replace the store: {e}"))
    })();
    match done {
        // The rename is only durable once the DIRECTORY is synced; a crash
        // in between can leave the entry pointing at neither file.
        Ok(()) => rustix::fs::fsync(dirfd)
            .map_err(|e| format!("the store was replaced but its folder did not sync: {e}")),
        Err(e) => {
            let _ = rustix::fs::unlinkat(dirfd, tmp.as_str(), AtFlags::empty());
            Err(e)
        }
    }
}

/// Carry every extended attribute across to the replacement — macOS keeps
/// the quarantine flag here, and a store that quietly loses its attributes
/// is not the file we promised to replace. Bounded buffers, so an
/// attribute set larger than anything legitimate refuses (ERANGE) rather
/// than growing; and every failure is fatal BEFORE the rename, never a
/// silent half-copy.
fn copy_xattrs(from: &OwnedFd, to: &OwnedFd) -> Result<(), String> {
    let mut names = [0u8; 4096];
    let n = rustix::fs::flistxattr(from, &mut names[..])
        .map_err(|e| format!("cannot list the store's attributes: {e}"))?;
    let mut value = [0u8; 64 * 1024];
    for raw in names[..n].split(|b| *b == 0).filter(|s| !s.is_empty()) {
        let name = std::str::from_utf8(raw)
            .map_err(|_| "the store has an attribute name that is not text".to_string())?;
        let len = rustix::fs::fgetxattr(from, name, &mut value[..])
            .map_err(|e| format!("cannot read the store's {name}: {e}"))?;
        rustix::fs::fsetxattr(to, name, &value[..len], rustix::fs::XattrFlags::empty())
            .map_err(|e| format!("cannot carry over the store's {name}: {e}"))?;
    }
    Ok(())
}

/// The wallpaper agent, split the way SIP forces it to be split: launchd
/// owns the identity, the kernel owns the signals. Behind one seam, so
/// every failure path below has a test that runs no launchctl, spawns no
/// ps, and signals nothing.
trait Agent {
    /// launchd's own answer for the service's pid, or None when it is not
    /// running the agent. Re-asked before every signal rather than carried
    /// forward: a pid is a snapshot, and launchd relaunches on demand.
    fn pid(&self) -> Result<Option<i32>, String>;
    /// SIGSTOP this exact pid.
    fn pause(&self, pid: i32) -> Result<(), String>;
    /// SIGCONT this exact pid.
    fn resume(&self, pid: i32) -> Result<(), String>;
    /// SIGTERM this exact pid and wait for it to go. launchd starts the
    /// service again on demand, and the new one reads the store off disk —
    /// which is the whole point of the restart.
    fn reload(&self, pid: i32) -> Result<(), String>;
    /// Is this pid ACTUALLY stopped? A STOP that returned Ok says only that
    /// the signal was accepted, and the store has to hold still.
    fn stopped(&self, pid: i32) -> Result<bool, String>;
}

struct Launchd;

impl Launchd {
    fn target() -> String {
        format!("gui/{}/{SERVICE}", rustix::process::getuid().as_raw())
    }

    /// One launchctl run: absolute path, root-owned binary, empty
    /// environment — the boundary every trusted child here starts from.
    fn run(args: &[&str]) -> Result<std::process::Output, String> {
        if !trusted_system_binary(LAUNCHCTL) {
            return Err("no trusted /bin/launchctl".into());
        }
        trusted_spawn(Path::new(LAUNCHCTL))
            .args(args)
            .output()
            .map_err(|e| format!("cannot run launchctl: {e}"))
    }

    /// One signal to the pid launchd named — never to a name, never to a
    /// list. ESRCH is not a failure here: the instance we were told about
    /// may have gone already, and the caller's identity checks are what
    /// notice that.
    fn send(pid: i32, sig: Signal, what: &str) -> Result<(), String> {
        let Some(p) = Pid::from_raw(pid) else {
            return Err(format!(
                "cannot {what} the wallpaper agent: {pid} is not a pid"
            ));
        };
        match kill_process(p, sig) {
            Ok(()) | Err(Errno::SRCH) => Ok(()),
            Err(e) => Err(format!(
                "cannot {what} the wallpaper agent (pid {pid}): {e}"
            )),
        }
    }
}

impl Agent for Launchd {
    fn pid(&self) -> Result<Option<i32>, String> {
        let out = Launchd::run(&["print", &Launchd::target()])?;
        // 113 is launchctl's "could not find service in domain": a Mac with
        // no wallpaper agent has nothing to coordinate with, which is an
        // answer rather than a fault. Every other refusal is a fault.
        if out.status.code() == Some(113) {
            return Ok(None);
        }
        if !out.status.success() {
            return Err(format!(
                "cannot read the wallpaper agent: {}",
                first_line(&out.stderr)
            ));
        }
        // `pid = N` on its own line while the service runs, and no such line
        // at all once its state is `not running`.
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| l.trim().strip_prefix("pid = "))
            .and_then(|n| n.trim().parse().ok()))
    }

    fn pause(&self, pid: i32) -> Result<(), String> {
        Launchd::send(pid, Signal::STOP, "pause")
    }

    fn resume(&self, pid: i32) -> Result<(), String> {
        Launchd::send(pid, Signal::CONT, "resume")
    }

    fn reload(&self, pid: i32) -> Result<(), String> {
        Launchd::send(pid, Signal::TERM, "restart")?;
        // Signal 0 until it answers ESRCH: an agent still alive is one that
        // never re-read the store, and could yet write over it from memory.
        for _ in 0..20 {
            match Pid::from_raw(pid) {
                Some(p) if rustix::process::test_kill_process(p).is_ok() => {}
                _ => return Ok(()),
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(format!(
            "the wallpaper agent did not exit after SIGTERM (pid {pid})"
        ))
    }

    /// `ps -o stat=` prints the state code; `T` in the first column is a
    /// stopped process (measured on macOS 26: `TN` stopped, `SN` running).
    fn stopped(&self, pid: i32) -> Result<bool, String> {
        if !trusted_system_binary(PS) {
            return Err("no trusted /bin/ps".into());
        }
        let out = trusted_spawn(Path::new(PS))
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .map_err(|e| format!("cannot run ps: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "the wallpaper agent (pid {pid}) is no longer there"
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .trim_start()
            .starts_with('T'))
    }
}

/// The first line a child gave as its reason, for a message that has to
/// carry it.
fn first_line(err: &[u8]) -> String {
    String::from_utf8_lossy(err)
        .lines()
        .next()
        .unwrap_or("no reason given")
        .trim()
        .to_string()
}

/// SIGSTOP across the write, so the agent cannot rewrite the store mid
/// transaction. One service and one stop, so a failed pause has nothing to
/// roll back — and a guard, because the resume is owed on every path.
/// A pause that did not take: why, and — when the compensating resume did
/// not land either — the guard still owing one, so the debt is the caller's
/// to settle rather than something this function walked away from.
type PauseFailed<'a> = (String, Option<Paused<'a>>);

struct Paused<'a> {
    agent: &'a dyn Agent,
    /// The pid we actually stopped — the one every later signal must still
    /// find launchd running. Taken the moment `release` accepts
    /// responsibility for reporting, so Drop is left holding only the panic.
    held: Option<i32>,
}

impl<'a> Paused<'a> {
    /// `running` is the service's pid as it stood a moment ago. None means
    /// launchd is not running the agent — it starts on demand and keeps no
    /// process alive between times — so there is nothing that could rewrite
    /// the store underneath us and nothing to restart into what we write.
    /// Pausing it would only fail, and failing there would cost a sync that
    /// had nothing to coordinate in the first place.
    fn new(agent: &'a dyn Agent, running: Option<i32>) -> Result<Paused<'a>, PauseFailed<'a>> {
        let Some(pid) = running else {
            return Ok(Paused { agent, held: None });
        };
        if let Err(e) = agent.pause(pid) {
            // Nothing stopped, so nothing is owed.
            return Err((e, None));
        }
        let mut guard = Paused {
            agent,
            held: Some(pid),
        };
        // A STOP that was ACCEPTED is not a process that stopped. The whole
        // transaction rests on the store holding still, so ask the process
        // table what state it actually reached — and on ANY answer but yes,
        // including one we could not read, put it back the way we found it
        // rather than walking away from a frozen agent.
        let confirmed = agent.stopped(pid);
        if matches!(confirmed, Ok(true)) {
            return Ok(guard);
        }
        let why = confirmed.err().map_or_else(
            || "the wallpaper agent did not stop".to_string(),
            |e| format!("cannot confirm the wallpaper agent stopped: {e}"),
        );
        match agent.resume(pid) {
            // Put back cleanly: the debt is settled, only the refusal
            // travels on.
            Ok(()) => {
                guard.held = None;
                Err((why, None))
            }
            // It is still stopped and we could not fix it, so the guard
            // keeps the pid and goes back to the caller still owing a
            // resume — the one thing that must not be dropped here.
            Err(e) => Err((
                format!("{why}; and it could not be resumed: {e}"),
                Some(guard),
            )),
        }
    }

    /// The instance this transaction is bound to, or None if there was no
    /// agent to pause.
    fn instance(&self) -> Option<i32> {
        self.held
    }

    /// Resume, then let the service restart itself into the file we wrote.
    /// BOTH are attempted whatever the first one does — a resume that failed
    /// must not cost the restart, and neither error may hide the other.
    fn release(mut self) -> Result<(), String> {
        // Nothing was ever paused, so nothing is owed and nothing needs
        // restarting: the next agent launchd starts reads the store fresh.
        let Some(pid) = self.held.take() else {
            return Ok(());
        };
        // The identity is re-asked before the CONT — but the CONT goes out
        // either way, and first: an agent left frozen is a desktop that
        // never repaints again, while a CONT to a pid that is not stopped
        // does nothing at all.
        let ours = self.agent.pid();
        let resumed = self.agent.resume(pid);
        if !matches!(&ours, Ok(Some(p)) if *p == pid) {
            let why = ours
                .err()
                .unwrap_or_else(|| "the wallpaper agent restarted during the sync".into());
            return Err(match resumed {
                Ok(()) => why,
                Err(e) => format!("{why}; and {e}"),
            });
        }
        // And re-asked again before the restart, because the resume above
        // took time somebody else could have used.
        let reloaded = match self.agent.pid() {
            Ok(Some(p)) if p == pid => self.agent.reload(pid),
            Ok(_) => Err("the wallpaper agent restarted during the sync".into()),
            Err(e) => Err(e),
        };
        match (resumed, reloaded) {
            (Ok(()), r) => r,
            (Err(e), Ok(())) => Err(e),
            (Err(e), Err(r)) => Err(format!("{e}; and {r}")),
        }
    }
}

/// Best effort, for a PANIC only: every ordinary exit goes through
/// [`finish`], which releases explicitly and reports what failed. An
/// unwinding stack has nobody left to report to, so it just tries.
impl Drop for Paused<'_> {
    fn drop(&mut self) {
        if let Some(pid) = self.held {
            let _ = self.agent.resume(pid);
        }
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

    // The Configuration fixtures are REAL binary plists: each was written
    // once as XML, converted with `plutil -convert binary1`, and pasted here
    // base64. Their shape is the live store's own, verified read-only —
    // `{type: "imageFile", url: {relative: <the URI>}}`.
    const NEW_B64: &str = concat!(
        "YnBsaXN0MDDSAQIDBFR0eXBlU3VybFlpbWFnZUZpbGXRBQZYcmVsYXRpdmVfEC",
        "hmaWxlOi8vL1VzZXJzL2V4YW1wbGUvd2FsbHBhcGVycy9uZXcuanBnCA0SFiAj",
        "LAAAAAAAAAEBAAAAAAAAAAcAAAAAAAAAAAAAAAAAAABX",
    );
    const OLD_B64: &str = concat!(
        "YnBsaXN0MDDSAQIDBFR0eXBlU3VybFlpbWFnZUZpbGXRBQZYcmVsYXRpdmVfEC",
        "hmaWxlOi8vL1VzZXJzL2V4YW1wbGUvd2FsbHBhcGVycy9vbGQuanBnCA0SFiAj",
        "LAAAAAAAAAEBAAAAAAAAAAcAAAAAAAAAAAAAAAAAAABX",
    );
    /// `{ActualTarget: file:///different.jpg, type: imageFile,
    /// url: {relative: file:///different.jpg}, unrelated: <NEW_URI>}` — our
    /// URI is in there, but not as the thing this choice chooses.
    const DECOY_B64: &str = concat!(
        "YnBsaXN0MDDUAQIDBAUIBwlTdXJsVHR5cGVcQWN0dWFsVGFyZ2V0WXVucmVsYX",
        "RlZNEGB1hyZWxhdGl2ZV8QFWZpbGU6Ly8vZGlmZmVyZW50LmpwZ1lpbWFnZUZp",
        "bGVfEChmaWxlOi8vL1VzZXJzL2V4YW1wbGUvd2FsbHBhcGVycy9uZXcuanBnCB",
        "EVGicxND1VXwAAAAAAAAEBAAAAAAAAAAoAAAAAAAAAAAAAAAAAAACK",
    );
    /// The same shape with `url.relative` written as DATA holding the URI's
    /// bytes: the right field, the wrong kind of object.
    const INDATA_B64: &str = concat!(
        "YnBsaXN0MDDSAQIDBFR0eXBlU3VybFlpbWFnZUZpbGXRBQZYcmVsYXRpdmVPEC",
        "hmaWxlOi8vL1VzZXJzL2V4YW1wbGUvd2FsbHBhcGVycy9uZXcuanBnCA0SFiAj",
        "LAAAAAAAAAEBAAAAAAAAAAcAAAAAAAAAAAAAAAAAAABX",
    );
    const AERIALS: &str = "com.apple.wallpaper.choice.aerials";

    /// A sync's worth of decoding allowance, for a test that only needs one.
    fn budget() -> Cell<usize> {
        Cell::new(MAX_EVALS)
    }

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

    /// A store holding these records as `Spaces/<uuid>/Default/Desktop` —
    /// the documented shape, which since round 6 is the only one the walk
    /// looks at. A record parked under an undocumented key is invisible to
    /// it by design.
    fn spaces_with(records: Vec<Plist>) -> Plist {
        d(vec![(
            "Spaces",
            Plist::Dict(
                records
                    .into_iter()
                    .enumerate()
                    .map(|(i, r)| {
                        (
                            format!("SPACE-{i}"),
                            d(vec![("Default", d(vec![("Desktop", r)]))]),
                        )
                    })
                    .collect(),
            ),
        )])
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
        let wrote = desktops(&tree);
        assert_eq!(wrote.len(), 8);
        // Every slot now shows the template's wallpaper — its Content and
        // the date it was set. What else each record carries is its own.
        assert!(
            wrote
                .iter()
                .all(|r| r.get("Content") == template.get("Content")
                    && r.get("LastSet") == template.get("LastSet")),
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
        // Older builds record the path in Files as plain text instead of
        // inside the Configuration blob; both shapes must count.
        let by_files = |when: &str| {
            let files = Plist::Array(vec![d(vec![("relative", s(NEW_URI))])]);
            rec(IMAGE, "", files, when)
        };
        let floor = date_secs("2026-09-03T06:44:45Z").unwrap();
        let tree = spaces_with(vec![
            image(NEW_B64, "2026-09-03T06:44:50Z"),
            image(NEW_B64, "2026-09-03T06:44:55Z"),
            by_files("2026-09-03T06:44:52Z"),
            // A newer record of a DIFFERENT image must never win.
            image(OLD_B64, "2026-09-03T06:44:59Z"),
        ]);
        let won = template(&tree, NEW_URI, Some(floor), &budget()).unwrap();
        assert_eq!(
            won.get("LastSet"),
            Some(&Plist::Date("2026-09-03T06:44:55Z".into()))
        );

        // The Files shape on its own still qualifies.
        let only_files = spaces_with(vec![by_files("2026-09-03T06:44:52Z")]);
        assert!(template(&only_files, NEW_URI, Some(floor), &budget()).is_some());

        // Another image never qualifies, whatever its date.
        let other = spaces_with(vec![image(OLD_B64, "2026-09-03T06:44:59Z")]);
        assert!(template(&other, NEW_URI, Some(floor), &budget()).is_none());

        // A record older than the helper's run is skipped — until the
        // patience runs out and the date stops being a requirement.
        let stale = spaces_with(vec![image(NEW_B64, "2026-09-01T00:00:00Z")]);
        assert!(template(&stale, NEW_URI, Some(floor), &budget()).is_none());
        assert!(template(&stale, NEW_URI, None, &budget()).is_some());

        // And a record parked outside the documented slots is not a
        // candidate at all, however well it matches.
        let elsewhere = d(vec![(
            "FutureAppleState",
            d(vec![("Desktop", image(NEW_B64, "2026-09-03T06:44:55Z"))]),
        )]);
        assert!(template(&elsewhere, NEW_URI, None, &budget()).is_none());
    }

    /// Fail closed: only the DATE is ever relaxed. A record the helper
    /// plainly just wrote still loses if it names another image — copying
    /// somebody else's wallpaper across every Space is worse than refusing.
    #[test]
    fn a_foreign_record_is_never_the_template() {
        let (stale, fresh) = ("2026-09-01T00:00:00Z", "2026-09-03T06:44:55Z");
        let floor = date_secs("2026-09-03T06:44:45Z").unwrap();
        let tree = spaces_with(vec![image(NEW_B64, stale), image(OLD_B64, fresh)]);
        assert!(template(&tree, NEW_URI, Some(floor), &budget()).is_none());
        let ours = template(&tree, NEW_URI, None, &budget());
        assert_eq!(ours, Some(image(NEW_B64, stale)));
        let none = spaces_with(vec![image(OLD_B64, fresh)]);
        assert!(template(&none, NEW_URI, None, &budget()).is_none());
    }

    /// Attribution reads the Configuration's FIELDS. Mentioning our URI
    /// somewhere in the blob is not choosing it, and neither is carrying it
    /// in the right field as the wrong kind of object.
    #[test]
    fn only_the_chosen_url_attributes_a_record() {
        assert!(chooses(NEW_B64, NEW_URI, &budget()));
        assert!(!chooses(OLD_B64, NEW_URI, &budget()));
        // A valid plist naming a different file, with our URI sitting in an
        // unrelated field.
        assert!(!chooses(DECOY_B64, NEW_URI, &budget()));
        // url.relative holding the URI's BYTES rather than the string.
        assert!(!chooses(INDATA_B64, NEW_URI, &budget()));
        // And through the record, memo and all.
        let slot = rec(IMAGE, NEW_B64, Plist::Array(vec![]), "2026-09-03T06:44:55Z");
        assert!(names(&slot, NEW_URI, &mut HashMap::new(), &budget()));
        // The decoy really does contain our URI — the old substring test
        // would have taken it.
        assert!(
            b64_decode(DECOY_B64)
                .unwrap()
                .windows(NEW_URI.len())
                .any(|w| w == NEW_URI.as_bytes())
        );
    }

    /// The blob is somebody else's file, so every malformed one refuses —
    /// and none of them may panic or recurse off the stack.
    #[test]
    fn hostile_bplists_refuse_without_panicking() {
        let good = b64_decode(NEW_B64).unwrap();
        assert!(bplist(&good, &budget()).is_some());
        let patch = |at: usize, with: &[u8]| {
            let mut b = good.clone();
            b[at..at + with.len()].copy_from_slice(with);
            b
        };
        let n = good.len();
        let cases: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"bplist00".to_vec(),
            // Truncated trailer, and a body cut off mid-object.
            good[..n - 10].to_vec(),
            good[..12].to_vec(),
            // An offset table pointing past the end of the blob.
            patch(n - 8, &[0xFF; 8]),
            // The first object's offset, past the end.
            patch(n - 33, &[0xFE]),
            // An object count, and a top-object index, beyond any ceiling.
            patch(n - 24, &[0xFF; 8]),
            patch(n - 16, &[0xFF; 8]),
            // Nonsense offset and reference widths.
            patch(n - 26, &[0]),
            patch(n - 25, &[0xFF]),
            // A dict of one entry whose value is the dict itself.
            [
                b"bplist00".as_slice(),
                &[0xD1, 0x01, 0x00, 0x51, b'a'],
                &[8, 11],
                &[0, 0, 0, 0, 0, 0, 0, 1, 1],
                &[0, 0, 0, 0, 0, 0, 0, 2],
                &[0, 0, 0, 0, 0, 0, 0, 0],
                &[0, 0, 0, 0, 0, 0, 0, 13],
            ]
            .concat(),
        ];
        for bad in cases {
            assert!(
                bplist(&bad, &budget()).is_none(),
                "accepted {} bytes",
                bad.len()
            );
        }
    }

    /// A wallpaper change is `Content` and `LastSet`. Everything else a
    /// destination record carries — its own `LastUse`, and any field this
    /// tool has never heard of — stays where it was.
    #[test]
    fn only_content_and_lastset_are_overlaid_onto_a_slot() {
        // The template is the REAL shape the helper writes, LastUse and all.
        // The previous version of this test deleted LastUse from it so the
        // assertion would pass; that hid exactly the bug this now catches,
        // and shaping a fixture to fit an assertion is not a thing to do.
        let template = image(NEW_B64, "2026-09-03T06:44:55Z");
        assert!(
            template.get("LastUse").is_some(),
            "the fixture is not the real template shape"
        );
        let mut dest = image(OLD_B64, "2026-09-01T12:30:10Z");
        dest.set("Extra", s("mine"));
        dest.set("LastUse", Plist::Date("2026-08-01T00:00:00Z".into()));
        let mut tree = d(vec![("SystemDefault", d(vec![("Desktop", dest.clone())]))]);
        rewrite(&mut tree, &template).unwrap();

        let got = tree.get("SystemDefault").unwrap().get("Desktop").unwrap();
        assert_eq!(got.get("Content"), template.get("Content"));
        assert_eq!(got.get("LastSet"), template.get("LastSet"));
        // The record's own state survives, INCLUDING a LastUse the template
        // also carries and differs on.
        assert_eq!(got.get("Extra"), Some(&s("mine")));
        assert_eq!(got.get("LastUse"), dest.get("LastUse"));
        assert_ne!(got.get("LastUse"), template.get("LastUse"));

        // The two slots with no destination record take the template whole.
        assert_eq!(
            tree.get("AllSpacesAndDisplays").unwrap().get("Desktop"),
            Some(&template)
        );
        let mut split = d(vec![(
            "SystemDefault",
            d(vec![("Linked", linked()), ("Type", s("linked"))]),
        )]);
        rewrite(&mut split, &template).unwrap();
        assert_eq!(
            split.get("SystemDefault").unwrap().get("Desktop"),
            Some(&template)
        );
    }

    /// The walk is scoped to the slot PATHS the store documents. A subtree
    /// that merely looks like a slot is not one, and an unknown key beside
    /// a real slot is nobody's business but its owner's.
    #[test]
    fn only_the_documented_slots_are_touched() {
        let template = image(NEW_B64, "2026-09-03T06:44:55Z");
        // Shaped exactly like a linked slot, and parked where no slot lives.
        let future = d(vec![
            ("Desktop", d(vec![("Sentinel", s("keep"))])),
            ("Linked", d(vec![("Sentinel", s("also keep"))])),
            ("Type", s("linked")),
        ]);
        let space = d(vec![
            (
                "Default",
                d(vec![("Desktop", image(OLD_B64, "2026-09-01T12:30:10Z"))]),
            ),
            ("Unknown", s("keep me")),
        ]);
        let mut tree = d(vec![
            ("FutureAppleState", future.clone()),
            ("Spaces", d(vec![("SPACE-ONE", space)])),
        ]);
        rewrite(&mut tree, &template).unwrap();

        // Byte-identical: not split, not seeded, not counted.
        assert_eq!(tree.get("FutureAppleState"), Some(&future));
        let one = tree.get("Spaces").unwrap().get("SPACE-ONE").unwrap();
        assert_eq!(one.get("Unknown"), Some(&s("keep me")));
        // While the real slot beside it IS rewritten.
        let real = one.get("Default").unwrap().get("Desktop").unwrap();
        assert_eq!(real.get("Content"), template.get("Content"));
        assert_eq!(real.get("LastSet"), template.get("LastSet"));
    }

    /// `levels` arrays, each holding `fan` references to the next, with a
    /// string at the bottom. Built by hand because the shape that costs
    /// exponential work — small, shallow, cycle-free, and all fan-out — is
    /// not one plutil would ever write.
    /// `bottom` is the string the deepest level holds — a caller varies it
    /// to make blobs identical in cost but distinct in text, or to put the
    /// URI's bytes inside one so the substring gate lets it reach a decode.
    fn fanout(levels: usize, fan: usize, bottom: &str) -> Vec<u8> {
        let mut b: Vec<u8> = b"bplist00".to_vec();
        let mut offsets = Vec::new();
        for i in 0..levels {
            offsets.push(b.len());
            match fan {
                0..15 => b.push(0xA0 | fan as u8),
                _ => b.extend_from_slice(&[0xAF, 0x10, fan as u8]),
            }
            b.extend(std::iter::repeat_n(i as u8 + 1, fan));
        }
        offsets.push(b.len());
        let s = bottom.as_bytes();
        match s.len() {
            0..15 => b.push(0x50 | s.len() as u8),
            _ => b.extend_from_slice(&[0x5F, 0x10, s.len() as u8]),
        }
        b.extend_from_slice(s);
        let table = b.len();
        for o in &offsets {
            b.extend_from_slice(&(*o as u16).to_be_bytes());
        }
        b.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 2, 1]);
        b.extend_from_slice(&(offsets.len() as u64).to_be_bytes());
        b.extend_from_slice(&0u64.to_be_bytes());
        b.extend_from_slice(&(table as u64).to_be_bytes());
        b
    }

    /// Shared references make the walk exponential without making the blob
    /// deep, large or cyclic, so the depth and object caps never see it. The
    /// evaluation budget is what refuses — and it must not refuse an
    /// ordinary chain of the same depth.
    #[test]
    fn a_fan_out_blob_exhausts_the_budget_instead_of_the_machine() {
        // The builder really does emit decodable blobs, fan-out and all —
        // so the refusal below is the budget, not a malformed fixture.
        let small = bplist(&fanout(2, 3, "x"), &budget()).unwrap();
        assert!(matches!(&small, Plist::Array(a) if a.len() == 3));
        assert!(bplist(&fanout(6, 32, "x"), &budget()).is_none());
        let chain = bplist(&fanout(6, 1, "x"), &budget()).unwrap();
        assert_eq!(
            chain,
            (0..6).fold(Plist::String("x".into()), |v, _| Plist::Array(vec![v]))
        );
        // Its OWN allowance stops it long before the sync's: seven objects
        // buy 112 evaluations, so one blob cannot spend the store's.
        let shared = budget();
        assert!(bplist(&fanout(6, 32, "x"), &shared).is_none());
        assert!(MAX_EVALS - shared.get() <= 7 * EVALS_PER_OBJECT);
    }

    /// A store can multiply a bounded blob by its slot count, so the whole
    /// scan is bounded too — and the cheap filters must run first, or a
    /// store of stale records would pay for a parse it never needed.
    #[test]
    fn a_store_full_of_hostile_records_is_bounded_and_decodes_nothing_stale() {
        // Each slot carries a DISTINCT fan-out blob, so the memo cannot mask
        // the bound; 512 of them, far past any real store. Each holds the
        // URI's bytes at the bottom, so the substring gate lets every one
        // through to a decode that then has to be stopped.
        let hostile = |i: usize| b64_encode(&fanout(6, 32, &format!("{NEW_URI}{i}")));
        let store = |when: &str, blob: &dyn Fn(usize) -> String| {
            spaces_with(
                (0..512)
                    .map(|i| rec(IMAGE, &blob(i), Plist::Array(vec![]), when))
                    .collect(),
            )
        };
        let floor = date_secs("2026-09-03T06:44:45Z").unwrap();

        let fresh = store("2026-09-03T06:44:55Z", &hostile);
        let spent = budget();
        let started = std::time::Instant::now();
        assert!(template(&fresh, NEW_URI, Some(floor), &spent).is_none());
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "the scan was not bounded: {:?}",
            started.elapsed()
        );
        assert!(spent.get() < MAX_EVALS, "nothing was decoded at all");

        // The same store, stale. The date filter comes first, so the
        // allowance is untouched: not one blob was parsed.
        let stale = store("2026-09-01T00:00:00Z", &hostile);
        let untouched = budget();
        assert!(template(&stale, NEW_URI, Some(floor), &untouched).is_none());
        assert_eq!(untouched.get(), MAX_EVALS, "a stale record was decoded");

        // And identical blobs are parsed once however many slots carry them.
        let same = store("2026-09-03T06:44:55Z", &|_| {
            b64_encode(&fanout(6, 32, NEW_URI))
        });
        let memoized = budget();
        assert!(template(&same, NEW_URI, Some(floor), &memoized).is_none());
        assert!(
            MAX_EVALS - memoized.get() < (MAX_EVALS - spent.get()) / 100,
            "the memo did not collapse identical blobs"
        );
    }

    /// A launchd that runs nothing: it logs the operations asked of it and
    /// fails whichever SET of them a test names, so two failing at once is
    /// as easy to stage as one.
    struct Fake {
        fails: Vec<&'static str>,
        pids: std::cell::RefCell<Vec<Option<i32>>>,
        /// What the process table says after a STOP.
        stops: bool,
        log: std::cell::RefCell<Vec<(&'static str, i32)>>,
    }

    impl Fake {
        /// `pids` is answered in order, the last value repeating — so a test
        /// stages "the same instance throughout" or "it restarted".
        fn new(pids: &[Option<i32>], fails: &[&'static str]) -> Fake {
            Fake {
                fails: fails.to_vec(),
                pids: std::cell::RefCell::new(pids.to_vec()),
                stops: true,
                log: std::cell::RefCell::new(Vec::new()),
            }
        }
        /// A STOP the kernel accepts and the process never obeys.
        fn that_never_stops(mut self) -> Fake {
            self.stops = false;
            self
        }
        fn did(&self, op: &'static str, pid: i32) -> Result<(), String> {
            self.log.borrow_mut().push((op, pid));
            match self.fails.contains(&op) {
                true => Err(format!("cannot {op} the wallpaper agent: refused")),
                false => Ok(()),
            }
        }
        fn ops(&self) -> Vec<&'static str> {
            self.log.borrow().iter().map(|(op, _)| *op).collect()
        }
        /// Every pid this agent was ever asked to signal.
        fn signalled(&self) -> Vec<i32> {
            self.log
                .borrow()
                .iter()
                .filter(|(op, _)| *op != "pid")
                .map(|(_, p)| *p)
                .collect()
        }
    }

    impl Agent for Fake {
        fn pid(&self) -> Result<Option<i32>, String> {
            self.log.borrow_mut().push(("pid", 0));
            if self.fails.contains(&"pid") {
                return Err("cannot read the wallpaper agent: refused".into());
            }
            let mut q = self.pids.borrow_mut();
            Ok(match q.len() {
                0 => None,
                1 => q[0],
                _ => q.remove(0),
            })
        }
        fn pause(&self, pid: i32) -> Result<(), String> {
            self.did("pause", pid)
        }
        fn resume(&self, pid: i32) -> Result<(), String> {
            self.did("resume", pid)
        }
        fn reload(&self, pid: i32) -> Result<(), String> {
            self.did("restart", pid)
        }
        fn stopped(&self, pid: i32) -> Result<bool, String> {
            self.did("stopped", pid)?;
            Ok(self.stops)
        }
    }

    /// A store on disk, in a directory of our own, holding one Desktop
    /// record that names NEW_URI. Never the live store.
    fn scratch_store(tag: &str) -> (std::path::PathBuf, rustix::fd::OwnedFd) {
        let dir = std::env::temp_dir().join(format!("theme-spaces-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let tree = d(vec![(
            "SystemDefault",
            d(vec![("Desktop", image(NEW_B64, "2026-09-03T06:44:55Z"))]),
        )]);
        let bytes = plutil("binary1", write_xml(&tree).as_bytes()).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        std::fs::write(dir.join("Index.plist"), &bytes).unwrap();
        let fd = crate::update::open_chain_nofollow(&dir).unwrap();
        (dir, fd)
    }

    fn floor() -> i64 {
        date_secs("2026-09-03T06:44:45Z").unwrap()
    }

    /// A guard the test expects to have been taken. (`Paused` carries a
    /// borrowed agent, so it has no Debug to unwrap through.)
    fn paused<'a>(agent: &'a dyn Agent, pid: Option<i32>) -> Paused<'a> {
        match Paused::new(agent, pid) {
            Ok(g) => g,
            Err((why, _)) => panic!("the agent should have paused: {why}"),
        }
    }

    /// The whole transaction, against a store of our own and a launchd that
    /// does nothing: it pauses, writes, and resumes and restarts the service
    /// — in that order.
    #[test]
    fn a_clean_sync_pauses_writes_then_resumes_and_restarts() {
        let (dir, fd) = scratch_store("clean");
        let path = dir.join("Index.plist");
        let before = std::fs::read(&path).unwrap();
        let agent = Fake::new(&[Some(7)], &[]);
        let quiet = |_: &Path| Ok(false);
        sync_with(&agent, &quiet, &fd, &path, NEW_URI, floor(), "new.jpg").unwrap();
        // launchd is asked who the agent IS before every signal, the STOP is
        // verified against the process table, and the identity is re-checked
        // before the rename, before the resume and before the restart.
        assert_eq!(
            agent.ops(),
            [
                "pid", "pause", "stopped", "pid", "pid", "resume", "pid", "restart"
            ],
            "the service was not coordinated in order"
        );
        // And every one of those signals went to the pid launchd named.
        assert!(agent.signalled().iter().all(|p| *p == 7));
        // The store really was rewritten, and it still parses.
        let after = std::fs::read(&path).unwrap();
        assert_ne!(after, before);
        assert!(parse_store(&after).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// launchd starts the agent on demand and keeps nothing alive between
    /// times, so a store synced while it is idle has nobody to coordinate
    /// with: the pause, the resume and the restart are all skipped, and the
    /// write still lands. Pausing a service that is not running would only
    /// fail, and that failure would have cost the whole sync.
    #[test]
    fn an_idle_agent_is_coordinated_with_by_leaving_it_alone() {
        let (dir, fd) = scratch_store("idle");
        let path = dir.join("Index.plist");
        let before = std::fs::read(&path).unwrap();
        // A launchd that refuses every operation on an idle service, which
        // is what the real one does: `kill SIGSTOP` on a service with no
        // process fails. The sync must succeed anyway, by not asking.
        let agent = Fake::new(&[None], &["pause", "resume", "restart"]);
        let quiet = |_: &Path| Ok(false);
        sync_with(&agent, &quiet, &fd, &path, NEW_URI, floor(), "new.jpg").unwrap();
        assert_eq!(agent.ops(), ["pid", "pid"], "an idle service was disturbed");
        let after = std::fs::read(&path).unwrap();
        assert_ne!(after, before, "the store was not written");
        assert!(parse_store(&after).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// And an agent that appears while we work is the same race as one that
    /// restarted: it read the old store on the way up.
    #[test]
    fn an_agent_that_appears_mid_sync_abandons_the_transaction() {
        let (dir, fd) = scratch_store("appeared");
        let path = dir.join("Index.plist");
        let before = std::fs::read(&path).unwrap();
        let agent = Fake::new(&[None, Some(7)], &[]);
        let quiet = |_: &Path| Ok(false);
        let e = sync_with(&agent, &quiet, &fd, &path, NEW_URI, floor(), "new.jpg").unwrap_err();
        assert!(e.contains("restarted during the sync"), "{e}");
        assert_eq!(std::fs::read(&path).unwrap(), before, "it renamed anyway");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A service that restarted mid-flight holds the OLD store in memory, so
    /// the rename must not happen — and the guard is still released.
    #[test]
    fn a_restarted_agent_abandons_the_transaction() {
        let (dir, fd) = scratch_store("raced");
        let path = dir.join("Index.plist");
        let before = std::fs::read(&path).unwrap();
        // Same instance while we set up, a different one at the rename.
        let agent = Fake::new(&[Some(7), Some(9)], &[]);
        let quiet = |_: &Path| Ok(false);
        let e = sync_with(&agent, &quiet, &fd, &path, NEW_URI, floor(), "new.jpg").unwrap_err();
        assert!(e.contains("restarted during the sync"), "{e}");
        assert_eq!(std::fs::read(&path).unwrap(), before, "it renamed anyway");
        assert!(
            agent.ops().contains(&"resume"),
            "the guard was not released"
        );
        // The STOP binds to the pid read immediately before it: the FIRST
        // agent call of the sync IS that read, so there is no earlier answer
        // for it to have gone stale against while `load` was waiting.
        assert_eq!(agent.ops()[..2], ["pid", "pause"]);
        assert_eq!(agent.signalled()[0], 7);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The ACL probe is asked again with the rename one statement away, so a
    /// store that grows an ACL mid-sync is still refused.
    #[test]
    fn the_pre_rename_check_refuses_an_acl_that_appeared() {
        let (dir, fd) = scratch_store("acl");
        let path = dir.join("Index.plist");
        let before = std::fs::read(&path).unwrap();
        let asked = Cell::new(0);
        let flips = |_: &Path| {
            asked.set(asked.get() + 1);
            Ok(asked.get() > 1)
        };
        let agent = Fake::new(&[Some(7)], &[]);
        let e = sync_with(&agent, &flips, &fd, &path, NEW_URI, floor(), "new.jpg").unwrap_err();
        assert!(e.contains("ACL"), "{e}");
        assert_eq!(asked.get(), 2, "the probe did not run twice");
        assert_eq!(std::fs::read(&path).unwrap(), before, "it renamed anyway");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Teardown attempts EVERY step and reports every failure: a resume that
    /// fails must not cost the restart, and neither may hide the other.
    #[test]
    fn a_release_that_fails_twice_reports_both() {
        let agent = Fake::new(&[Some(7)], &["resume", "restart"]);
        let e = paused(&agent, Some(7)).release().unwrap_err();
        assert!(e.contains("cannot resume"), "{e}");
        assert!(e.contains("cannot restart"), "{e}");
        assert_eq!(
            agent.ops(),
            ["pause", "stopped", "pid", "resume", "pid", "restart"]
        );

        // One failing on its own still surfaces, from either side.
        let agent = Fake::new(&[Some(7)], &["restart"]);
        assert!(
            paused(&agent, Some(7))
                .release()
                .unwrap_err()
                .contains("cannot restart")
        );
        let agent = Fake::new(&[Some(7)], &["resume"]);
        assert!(
            paused(&agent, Some(7))
                .release()
                .unwrap_err()
                .contains("cannot resume")
        );

        // A pause that fails yields no guard, so nothing is ever written.
        let agent = Fake::new(&[Some(7)], &["pause"]);
        assert!(Paused::new(&agent, Some(7)).is_err());
        assert_eq!(agent.ops(), ["pause"]);
    }

    /// A STOP the kernel accepted is not a process that stopped, and the
    /// whole transaction rests on the store holding still. An agent that
    /// never reaches the stopped state is resumed and refused — nothing is
    /// written behind a process that is still running.
    #[test]
    fn a_stop_that_was_accepted_but_not_obeyed_is_refused() {
        let agent = Fake::new(&[Some(7)], &[]).that_never_stops();
        let Err((e, owed)) = Paused::new(&agent, Some(7)) else {
            panic!("a process that never stopped must not yield a guard");
        };
        assert!(e.contains("did not stop"), "{e}");
        // The compensating CONT landed, so nothing is still owed.
        assert!(owed.is_none());
        assert_eq!(agent.ops(), ["pause", "stopped", "resume"]);

        // An answer we could not READ is treated the same way: we stopped
        // it, so it gets put back whatever went wrong.
        let agent = Fake::new(&[Some(7)], &["stopped"]);
        let Err((e, owed)) = Paused::new(&agent, Some(7)) else {
            panic!("an unreadable process state must not yield a guard");
        };
        assert!(e.contains("cannot confirm"), "{e}");
        assert!(owed.is_none());
        assert_eq!(agent.ops(), ["pause", "stopped", "resume"]);

        // And when the compensating CONT does NOT land, the debt travels
        // with the guard rather than being dropped here: the caller resumes
        // it, and Drop tries once more behind that.
        let agent = Fake::new(&[Some(7)], &["stopped", "resume"]).that_never_stops();
        let Err((e, owed)) = Paused::new(&agent, Some(7)) else {
            panic!("a stop that could not be undone must not yield a guard");
        };
        assert!(e.contains("cannot confirm"), "{e}");
        assert!(e.contains("could not be resumed"), "{e}");
        drop(owed.expect("the resume debt must come back with the guard"));
        assert_eq!(
            agent.ops(),
            ["pause", "stopped", "resume", "resume"],
            "Drop did not retry the resume"
        );

        // And it costs the sync rather than the store.
        let (dir, fd) = scratch_store("nostop");
        let path = dir.join("Index.plist");
        let before = std::fs::read(&path).unwrap();
        let agent = Fake::new(&[Some(7)], &[]).that_never_stops();
        let quiet = |_: &Path| Ok(false);
        let e = sync_with(&agent, &quiet, &fd, &path, NEW_URI, floor(), "new.jpg").unwrap_err();
        assert!(e.contains("did not stop"), "{e}");
        assert_eq!(std::fs::read(&path).unwrap(), before, "it wrote anyway");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A teardown failure never replaces the reason we were tearing down,
    /// and a clean run never swallows one.
    #[test]
    fn finish_reports_both_halves() {
        let agent = Fake::new(&[Some(7)], &["resume"]);
        let g = paused(&agent, Some(7));
        let both = finish(g, Err("the write failed".into())).unwrap_err();
        assert!(
            both.starts_with("the write failed; and the wallpaper agent could not be resumed: "),
            "{both}"
        );

        let agent = Fake::new(&[Some(7)], &["resume"]);
        let g = paused(&agent, Some(7));
        assert!(finish(g, Ok(())).unwrap_err().contains("cannot resume"));

        let agent = Fake::new(&[Some(7)], &[]);
        let g = paused(&agent, Some(7));
        assert!(finish(g, Ok(())).is_ok());
    }

    /// A path's answer belongs to a descriptor only while the two are the
    /// same file.
    #[test]
    fn same_object_is_identity_not_spelling() {
        let (dir, _fd) = scratch_store("ident");
        let path = dir.join("Index.plist");
        let twin = dir.join("Twin.plist");
        std::fs::copy(&path, &twin).unwrap();
        let a = rustix::fs::stat(&path).unwrap();
        let b = rustix::fs::stat(&path).unwrap();
        let other = rustix::fs::stat(&twin).unwrap();
        assert!(same_object(&a, &b));
        // Byte-identical content, same directory, different file.
        assert!(!same_object(&a, &other));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Metadata this tool cannot carry is refused, never lost quietly.
    #[test]
    fn uncopyable_metadata_is_refused() {
        assert_eq!(uncopyable(0, false), None);
        assert!(uncopyable(0, true).unwrap().contains("ACL"));
        // UF_IMMUTABLE, and any other flag.
        assert!(uncopyable(0x2, false).unwrap().contains("BSD flags"));
        assert!(uncopyable(0x40000, false).is_some());
        // Flags are named first when a store carries both.
        assert!(uncopyable(0x2, true).unwrap().contains("BSD flags"));

        // The ACE lines `ls -lde` prints, and the header line it prints
        // first — which carries a ':' of its own in the timestamp.
        assert!(ace_line(" 0: group:everyone deny delete"));
        assert!(ace_line("12: user:samuel allow read"));
        assert!(!ace_line(
            "-rw-r--r--@ 1 samuel  staff  6595 Sep  3 01:47 Index.plist"
        ));
        assert!(!ace_line(""));
        assert!(!ace_line("no colon here"));
    }

    /// A store with no all-Spaces slot gets one, and it must say what it is:
    /// a seeded Desktop left under Type "idle" never shows.
    #[test]
    fn a_seeded_all_spaces_slot_says_it_is_individual() {
        let template = image(NEW_B64, "2026-09-03T06:44:55Z");
        let mut tree = d(vec![(
            "SystemDefault",
            d(vec![("Desktop", template.clone())]),
        )]);
        rewrite(&mut tree, &template).unwrap();
        let all = tree.get("AllSpacesAndDisplays").unwrap();
        assert_eq!(all.get("Desktop"), Some(&template));
        assert_eq!(all.get("Type"), Some(&s("individual")));
        assert_eq!(all.get("Idle"), None);
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
    fn base64_round_trips_and_refuses_junk() {
        // Both directions, against a real plutil-written blob.
        assert_eq!(b64_encode(&b64_decode(NEW_B64).unwrap()), NEW_B64);
        assert_eq!(b64_decode("aGVs\n\t bG8=").unwrap(), b"hello");
        assert_eq!(b64_decode("").unwrap(), b"");
        for (bytes, text) in [
            (&b""[..], ""),
            (b"a", "YQ=="),
            (b"ab", "YWI="),
            (b"abc", "YWJj"),
        ] {
            assert_eq!(b64_encode(bytes), text);
            assert_eq!(b64_decode(text).unwrap(), bytes);
        }
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

//! Incremental search facts. Records never select files: the live library
//! supplies every path, and file identity plus palette/zone changes invalidate.
use crate::{config::Config, store, ui::display_text};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

type Facts = Vec<(String, String)>;
const NAME: &str = "library-index-v1";
const MAX: usize = 16 * 1024 * 1024;
pub struct Index {
    entries: BTreeMap<String, Facts>,
    seen: BTreeSet<String>,
    scope: String,
    dirty: bool,
}

pub enum Lookup {
    Cached(Facts),
    Missing(Missing),
}

pub struct Missing {
    key: String,
    path: std::path::PathBuf,
    before: Option<String>,
}

pub(crate) fn file_identity(path: &Path) -> Option<String> {
    let st = path.metadata().ok()?;
    Some(format!(
        "{}:{}:{}:{}:{}:{}:{}",
        st.dev(),
        st.ino(),
        st.len(),
        st.mtime(),
        st.mtime_nsec(),
        st.ctime(),
        st.ctime_nsec()
    ))
}

fn hash_zone(hash: &mut Sha256, zone: Option<&std::ffi::OsStr>) {
    match zone {
        Some(zone) => {
            hash.update([1]);
            hash.update(zone.as_bytes());
            #[cfg(target_os = "macos")]
            hash_zone_files(hash, zone);
        }
        None => hash.update([0]),
    }
}

// Match tz-rs's fixed file search roots. Tracking all candidates is
// conservative when a file appears, disappears, or changes under the same TZ.
#[cfg(target_os = "macos")]
fn hash_zone_files(hash: &mut Sha256, zone: &std::ffi::OsStr) {
    let raw = zone.as_bytes();
    if raw.is_empty() || raw == b"localtime" {
        return; // /etc/localtime is tracked separately.
    }
    let raw = raw.strip_prefix(b":").unwrap_or(raw);
    if raw.is_empty() {
        return;
    }
    let path = Path::new(std::ffi::OsStr::from_bytes(raw));
    let mut include = |path: &Path| {
        hash.update([0]);
        hash.update(file_identity(path).unwrap_or_default());
    };
    if path.is_absolute() {
        include(path);
    } else {
        for root in ["/usr/share/zoneinfo", "/share/zoneinfo", "/etc/zoneinfo"] {
            include(&Path::new(root).join(path));
        }
    }
}

/// Shared by date formatting and the index so a changed zone cannot label
/// newly cached facts with dates from an older parsed timezone.
pub(crate) fn timezone_fingerprint(zone: Option<&std::ffi::OsStr>) -> String {
    let mut hash = Sha256::new();
    hash_zone(&mut hash, zone);
    hash.update(file_identity(Path::new("/etc/localtime")).unwrap_or_default());
    format!("{:x}", hash.finalize())
}

impl Index {
    pub fn open(cfg: &Config) -> Self {
        let mut hash = Sha256::new();
        for root in &cfg.wallpaper_dirs {
            hash.update(root.as_os_str().as_bytes());
            hash.update([0]);
        }
        hash.update(timezone_fingerprint(std::env::var_os("TZ").as_deref()));
        let scope = format!("{:x}", hash.finalize());
        let fields = store::read(cfg, NAME, MAX).and_then(|b| store::unpack(&b));
        let entries = fields
            .as_deref()
            .and_then(|f| decode(f, &scope))
            .unwrap_or_default();
        Self {
            entries,
            seen: BTreeSet::new(),
            scope,
            dirty: false,
        }
    }

    /// Capture each missing record's identity before collecting batch metadata.
    /// Warm records need only this one lookup and never enter the batch.
    pub fn lookup(&mut self, path: &Path, scheme: &[String]) -> Lookup {
        let mut hash = Sha256::new();
        hash.update(&self.scope);
        hash.update(path.as_os_str().as_bytes());
        let before = file_identity(path);
        hash.update(before.as_deref().unwrap_or_default());
        for color in scheme {
            hash.update(color);
            hash.update([0]);
        }
        let key = format!("{:x}", hash.finalize());
        self.seen.insert(key.clone());
        if let Some(facts) = self.entries.get(&key) {
            return Lookup::Cached(facts.clone());
        }
        Lookup::Missing(Missing {
            key,
            path: path.to_path_buf(),
            before,
        })
    }

    pub fn store(&mut self, missing: Missing, facts: Facts) -> Facts {
        if missing.before.is_some() && missing.before == file_identity(&missing.path) {
            self.entries.insert(missing.key, facts.clone());
            self.dirty = true;
        }
        facts
    }

    #[cfg(test)]
    pub fn facts(
        &mut self,
        path: &Path,
        scheme: &[String],
        build: impl FnOnce() -> Facts,
    ) -> Facts {
        match self.lookup(path, scheme) {
            Lookup::Cached(facts) => facts,
            Lookup::Missing(missing) => self.store(missing, build()),
        }
    }

    pub fn finish(mut self, cfg: &Config) -> Result<usize, String> {
        if cfg.no_apply {
            return Ok(0);
        }
        let old = self.entries.len();
        self.entries.retain(|key, _| self.seen.contains(key));
        if !self.dirty && old == self.entries.len() {
            return Ok(self.entries.len());
        }
        let (bytes, retained) = encode_bounded(self.scope, self.entries, MAX);
        store::write(cfg, NAME, &bytes)?;
        Ok(retained)
    }
}

/// Keep useful warm records even when a library exceeds the on-disk bound.
/// The same bound applies before serialization, and matches the reader's field cap.
fn encode_bounded(scope: String, entries: BTreeMap<String, Facts>, max: usize) -> (Vec<u8>, usize) {
    let mut size = store::pack(&[]).len() + 8 + scope.len();
    let mut fields = vec![scope];
    let mut retained = 0;
    for (key, facts) in entries {
        let mut record = vec![key, facts.len().to_string()];
        for (name, value) in facts {
            record.push(name);
            record.push(value);
        }
        let needed: usize = record.iter().map(|s| 8 + s.len()).sum();
        if size + needed > max || fields.len() + record.len() > 500_000 {
            continue;
        }
        size += needed;
        fields.extend(record);
        retained += 1;
    }
    (store::pack(&fields), retained)
}

fn decode(fields: &[String], scope: &str) -> Option<BTreeMap<String, Facts>> {
    if fields.first()?.as_str() != scope {
        return None;
    }
    let mut rest = &fields[1..];
    let mut entries = BTreeMap::new();
    while !rest.is_empty() {
        let key = rest.first()?;
        if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let n: usize = rest.get(1)?.parse().ok()?;
        if n > 32 {
            return None;
        }
        let end = 2 + 2 * n;
        let pairs = rest.get(2..end)?;
        let mut facts = Vec::new();
        for p in pairs.as_chunks::<2>().0 {
            if p[0].len() > 32 || p[1].len() > 8192 {
                return None;
            }
            facts.push((display_text(&p[0]), display_text(&p[1])));
        }
        if entries.insert(key.clone(), facts).is_some() {
            return None;
        }
        rest = &rest[end..];
    }
    Some(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> (Config, std::path::PathBuf) {
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("index-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let path = base.join("image.png");
        std::fs::write(&path, b"image identity").unwrap();
        let cfg = Config {
            wallpaper_dirs: vec![base.clone()],
            wallpaper_dirs_display: base.display().to_string(),
            cache_dir: base.join("cache"),
            kitty_dir: base.join("kitty"),
            current: base.join("kitty/current-theme.conf"),
            formats: vec!["png".into()],
            contrast: 7.0,
            no_apply: false,
        };
        (cfg, path)
    }

    #[test]
    fn warm_index_skips_fact_collection_and_invalidates_changes() {
        let (cfg, path) = fixture("warm");
        let mut first = Index::open(&cfg);
        let facts = vec![("title".into(), "original".into())];
        assert_eq!(first.facts(&path, &[], || facts.clone()), facts);
        first.finish(&cfg).unwrap();
        assert!(cfg.cache_dir.join(NAME).is_file());
        let mut second = Index::open(&cfg);
        assert_eq!(
            second.facts(&path, &[], || panic!("warm lookup repeated metadata work")),
            facts
        );
        std::fs::write(&path, b"changed image identity and size").unwrap();
        let updated = vec![("title".into(), "updated".into())];
        assert_eq!(second.facts(&path, &[], || updated.clone()), updated);
        let colored = vec![("colors".into(), "blue".into())];
        assert_eq!(
            second.facts(&path, &["abcdef".into()], || colored.clone()),
            colored
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn changed_file_between_batch_lookup_and_store_is_not_cached() {
        let (cfg, path) = fixture("batch-changed");
        let mut index = Index::open(&cfg);
        let Lookup::Missing(missing) = index.lookup(&path, &[]) else {
            panic!("new fixture unexpectedly cached");
        };
        std::fs::write(&path, b"replacement during metadata batch").unwrap();
        index.store(missing, vec![("source".into(), "stale metadata".into())]);
        assert!(index.entries.is_empty());
        assert!(matches!(index.lookup(&path, &[]), Lookup::Missing(_)));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn no_apply_does_not_publish_an_index() {
        let (mut cfg, path) = fixture("no-apply");
        cfg.no_apply = true;
        let mut index = Index::open(&cfg);
        index.facts(&path, &[], || vec![("title".into(), "one".into())]);
        index.finish(&cfg).unwrap();
        assert!(!cfg.cache_dir.exists());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn absent_and_empty_timezone_have_different_scopes() {
        let mut absent = Sha256::new();
        let mut empty = Sha256::new();
        hash_zone(&mut absent, None);
        hash_zone(&mut empty, Some(std::ffi::OsStr::new("")));
        assert_ne!(absent.finalize(), empty.finalize());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn changed_explicit_timezone_file_invalidates_scope() {
        let (_, path) = fixture("zone-file");
        let scope = || {
            let mut hash = Sha256::new();
            hash_zone(&mut hash, Some(path.as_os_str()));
            hash.finalize()
        };
        let before = scope();
        std::fs::write(&path, b"different timezone file identity").unwrap();
        assert_ne!(before, scope());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn oversized_library_retains_a_readable_bounded_subset() {
        let entries: BTreeMap<_, _> = (0..20)
            .map(|i| {
                (
                    format!("{i:064x}"),
                    vec![("title".into(), "sea view".into())],
                )
            })
            .collect();
        let (bytes, retained) = encode_bounded("scope".into(), entries, 512);
        assert!(retained > 0 && retained < 20);
        assert!(bytes.len() <= 512);
        assert_eq!(
            decode(&store::unpack(&bytes).unwrap(), "scope")
                .unwrap()
                .len(),
            retained
        );
    }

    #[test]
    fn cache_shape_and_scope_are_strict() {
        let key = "a".repeat(64);
        let fields = vec![
            "scope".into(),
            key.clone(),
            "1".into(),
            "title".into(),
            "sea\nview".into(),
        ];
        assert_eq!(decode(&fields, "scope").unwrap()[&key][0].0, "title");
        assert!(decode(&fields, "other roots or timezone").is_none());
        assert!(decode(&fields[..4], "scope").is_none());
        let mut oversized = fields;
        oversized[2] = "999999999999999999999".into();
        assert!(decode(&oversized, "scope").is_none());
    }
    #[test]
    fn subsecond_rewrites_change_identity() {
        let dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/index-identity-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}", std::process::id()));
        std::fs::write(&path, b"one").unwrap();
        let before = file_identity(&path).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let modified =
            file.metadata().unwrap().modified().unwrap() + std::time::Duration::from_nanos(1000);
        file.set_modified(modified).unwrap();
        assert_ne!(before, file_identity(&path).unwrap());
        std::fs::remove_file(path).unwrap();
    }
}

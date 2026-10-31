//! arca-index-redb — the redb impl of [`arca_index::Index`].
//!
//! Layout under the index dir (`$ARCA_HOME`, else
//! `$XDG_DATA_HOME/arca`, else `~/.local/share/arca`):
//!
//! - `index.redb` — one `snapshots` table: canonical repo path →
//!   JSON [`RepoSnapshot`].
//! - `repos.toml` — the registry (`[[repo]] path name`), kept as a
//!   plain file so it stays human-inspectable.
//!
//! Everything here is a derived cache — `rm -rf` the dir and
//! `arca index rebuild` derives it back from the worktrees.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use arca_index::{Error, Index, RepoEntry, RepoSnapshot};
use camino::{Utf8Path, Utf8PathBuf};
use redb::{Database, ReadableDatabase, TableDefinition};

/// `snapshots` table — repo path → JSON [`RepoSnapshot`].
const SNAPSHOTS: TableDefinition<&str, &str> = TableDefinition::new("snapshots");

/// The registry file name inside the index dir.
const REGISTRY: &str = "repos.toml";

/// The redb file name inside the index dir.
const DB: &str = "index.redb";

/// Total time a contended index lock is retried before giving up —
/// lets concurrent `arca` processes queue instead of crash.
const LOCK_RETRY_BUDGET: Duration = Duration::from_secs(2);

/// First backoff after a busy lock; doubles each retry.
const LOCK_RETRY_MIN: Duration = Duration::from_millis(10);

/// Longest backoff between retries.
const LOCK_RETRY_MAX: Duration = Duration::from_millis(320);

/// The index dir: `$ARCA_HOME` → `$XDG_DATA_HOME/arca` →
/// `~/.local/share/arca`. The `$ARCA_HOME` override is how tests
/// (and unusual setups) relocate the whole cache.
pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("ARCA_HOME") {
        return PathBuf::from(d);
    }
    if let Some(d) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(d).join("arca");
    }
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".local/share/arca")
}

/// `repos.toml` on disk.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Registry {
    #[serde(default)]
    repo: Vec<RepoEntry>,
}

/// Open `index.redb` at `path`, retrying on lock contention
/// ([`redb::DatabaseError::DatabaseAlreadyOpen`]) with exponential
/// backoff until `budget` is spent. Anything else — corrupt file,
/// IO, upgrade required — fails immediately.
fn open_db(path: &Utf8Path, budget: Duration) -> Result<Database, Error> {
    let start = Instant::now();
    let mut backoff = LOCK_RETRY_MIN;
    let mut attempt = 0u64;
    loop {
        match Database::create(path.as_std_path()) {
            Ok(db) => return Ok(db),
            Err(redb::DatabaseError::DatabaseAlreadyOpen) => {
                let left = budget.saturating_sub(start.elapsed());
                if left.is_zero() {
                    return Err(Error::Backend(format!(
                        "index busy — another arca process holds {DB} (waited ~{:.1}s)",
                        budget.as_secs_f64()
                    )));
                }
                // counter-based stagger — spreads contenders, no rand dep
                let jitter = Duration::from_millis((attempt % 4) * 5);
                std::thread::sleep((backoff + jitter).min(left));
                backoff = (backoff * 2).min(LOCK_RETRY_MAX);
                attempt += 1;
            }
            Err(e) => return Err(Error::Backend(format!("open {path}: {e}"))),
        }
    }
}

/// redb-backed index rooted at an index dir.
pub struct RedbIndex {
    dir: Utf8PathBuf,
}

impl RedbIndex {
    /// Open the default index ([`data_dir`]), creating it.
    pub fn open() -> Result<Self, Error> {
        let dir = Utf8PathBuf::from_path_buf(data_dir())
            .map_err(|p| Error::Backend(format!("{}: non-utf8 index dir", p.display())))?;
        Self::open_at(dir)
    }

    /// Open an index rooted at `dir`, creating it when missing.
    pub fn open_at(dir: impl Into<Utf8PathBuf>) -> Result<Self, Error> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(RedbIndex { dir })
    }

    /// The index dir.
    pub fn dir(&self) -> &Utf8Path {
        &self.dir
    }

    fn registry_path(&self) -> Utf8PathBuf {
        self.dir.join(REGISTRY)
    }

    fn db_path(&self) -> Utf8PathBuf {
        self.dir.join(DB)
    }

    fn load_registry(&self) -> Result<Registry, Error> {
        match std::fs::read_to_string(self.registry_path()) {
            Ok(src) => toml::from_str(&src).map_err(|e| Error::Codec(format!("{REGISTRY}: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
            Err(e) => Err(Error::Io(e)),
        }
    }

    fn save_registry(&self, reg: &Registry) -> Result<(), Error> {
        let src = toml::to_string(reg).map_err(|e| Error::Codec(format!("{REGISTRY}: {e}")))?;
        let path = self.registry_path();
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, src)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    fn db(&self) -> Result<Database, Error> {
        open_db(&self.db_path(), LOCK_RETRY_BUDGET)
    }

    fn write_snapshot(&self, path: &Utf8Path, value: Option<&str>) -> Result<(), Error> {
        let db = self.db()?;
        let tx = db
            .begin_write()
            .map_err(|e| Error::Backend(format!("begin_write: {e}")))?;
        {
            let mut t = tx
                .open_table(SNAPSHOTS)
                .map_err(|e| Error::Backend(format!("open snapshots: {e}")))?;
            match value {
                Some(v) => {
                    t.insert(path.as_str(), v)
                        .map_err(|e| Error::Backend(format!("insert: {e}")))?;
                }
                None => {
                    t.remove(path.as_str())
                        .map_err(|e| Error::Backend(format!("remove: {e}")))?;
                }
            }
        }
        tx.commit()
            .map_err(|e| Error::Backend(format!("commit: {e}")))
    }
}

impl Index for RedbIndex {
    fn register(&self, repo: &RepoEntry) -> Result<(), Error> {
        let mut reg = self.load_registry()?;
        match reg.repo.iter_mut().find(|r| r.path == repo.path) {
            Some(r) => r.name = repo.name.clone(),
            None => reg.repo.push(repo.clone()),
        }
        self.save_registry(&reg)
    }

    fn remove(&self, path: &Utf8Path) -> Result<bool, Error> {
        let mut reg = self.load_registry()?;
        let before = reg.repo.len();
        reg.repo.retain(|r| r.path != path);
        if reg.repo.len() == before {
            return Ok(false);
        }
        self.save_registry(&reg)?;
        self.write_snapshot(path, None)?;
        Ok(true)
    }

    fn repos(&self) -> Result<Vec<RepoEntry>, Error> {
        let mut repos = self.load_registry()?.repo;
        repos.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(repos)
    }

    fn snapshot(&self, path: &Utf8Path) -> Result<Option<RepoSnapshot>, Error> {
        let db = self.db()?;
        let tx = db
            .begin_read()
            .map_err(|e| Error::Backend(format!("begin_read: {e}")))?;
        let t = match tx.open_table(SNAPSHOTS) {
            Ok(t) => t,
            Err(_) => return Ok(None), // no table → no snapshots yet
        };
        let Some(v) = t
            .get(path.as_str())
            .map_err(|e| Error::Backend(format!("get: {e}")))?
        else {
            return Ok(None);
        };
        serde_json::from_str(v.value())
            .map(Some)
            .map_err(|e| Error::Codec(format!("snapshot {path}: {e}")))
    }

    fn save_snapshot(&self, snapshot: &RepoSnapshot) -> Result<(), Error> {
        let json = serde_json::to_string(snapshot)
            .map_err(|e| Error::Codec(format!("snapshot {}: {e}", snapshot.path)))?;
        self.write_snapshot(&snapshot.path, Some(&json))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use arca_core::{Item, ItemId, ItemKind, Status};
    use std::collections::BTreeSet;

    fn idx() -> (tempfile::TempDir, RedbIndex) {
        let d = tempfile::tempdir().unwrap();
        let dir = Utf8PathBuf::from_path_buf(d.path().join("arca")).unwrap();
        (d, RedbIndex::open_at(dir).unwrap())
    }

    fn entry(n: &str) -> RepoEntry {
        RepoEntry::new(Utf8PathBuf::from(format!("/repos/{n}")), n)
    }

    fn snap(e: &RepoEntry, head: &str) -> RepoSnapshot {
        RepoSnapshot {
            path: e.path.clone(),
            name: e.name.clone(),
            head_commit: head.into(),
            scanned_at: "2026-02-09T00:00:00Z".into(),
            items: vec![Item::new(ItemId::new(ItemKind::Task, 1), "x").status(Status::InProgress)],
            archived: BTreeSet::new(),
        }
    }

    #[test]
    fn register_list_remove_cycle() {
        let (_d, i) = idx();
        assert!(i.repos().unwrap().is_empty());
        i.register(&entry("b")).unwrap();
        i.register(&entry("a")).unwrap();
        // re-register updates the name, doesn't duplicate
        i.register(&RepoEntry::new(Utf8PathBuf::from("/repos/b"), "b-renamed"))
            .unwrap();
        let repos = i.repos().unwrap();
        assert_eq!(repos.len(), 2);
        // sorted by path
        assert_eq!(repos[0].path.as_str(), "/repos/a");
        assert_eq!(repos[1].name, "b-renamed");
        // registry persists on disk as plain toml
        let src = std::fs::read_to_string(i.registry_path()).unwrap();
        assert!(src.contains("/repos/a"));
        // remove drops the entry + its snapshot
        i.save_snapshot(&snap(&repos[1], "h1")).unwrap();
        assert!(i.remove(&repos[1].path).unwrap());
        assert!(!i.remove(&repos[1].path).unwrap());
        assert_eq!(i.repos().unwrap().len(), 1);
        assert!(i.snapshot(&repos[1].path).unwrap().is_none());
    }

    #[test]
    fn snapshot_roundtrip_and_overwrite() {
        let (_d, i) = idx();
        let e = entry("x");
        assert!(i.snapshot(&e.path).unwrap().is_none());
        i.save_snapshot(&snap(&e, "h1")).unwrap();
        let got = i.snapshot(&e.path).unwrap().unwrap();
        assert_eq!(got.head_commit, "h1");
        assert_eq!(got.items.len(), 1);
        // overwrite — freshness check sees the new head
        i.save_snapshot(&snap(&e, "h2")).unwrap();
        assert_eq!(i.snapshot(&e.path).unwrap().unwrap().head_commit, "h2");
        // survives a reopen
        let i2 = RedbIndex::open_at(i.dir().to_path_buf()).unwrap();
        assert_eq!(i2.snapshot(&e.path).unwrap().unwrap().head_commit, "h2");
    }

    #[test]
    fn data_dir_env_override() {
        // ARCA_HOME wins over everything
        let d = tempfile::tempdir().unwrap();
        let ds = d.path().to_str().unwrap();
        temp_env::with_var("ARCA_HOME", Some(ds), || {
            assert_eq!(data_dir(), d.path());
        });
        temp_env::with_vars(
            [("ARCA_HOME", None::<&str>), ("XDG_DATA_HOME", Some(ds))],
            || {
                assert_eq!(data_dir(), d.path().join("arca"));
            },
        );
    }

    #[test]
    fn malformed_registry_is_codec_error() {
        let (_d, i) = idx();
        std::fs::write(i.registry_path(), "nope = [").unwrap();
        assert!(matches!(i.repos(), Err(Error::Codec(_))));
        assert!(matches!(i.register(&entry("x")), Err(Error::Codec(_))));
    }

    #[test]
    fn data_dir_falls_back_to_home_then_dot() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().to_str().unwrap();
        // no ARCA_HOME/XDG → ~/.local/share/arca
        temp_env::with_vars(
            [
                ("ARCA_HOME", None::<&str>),
                ("XDG_DATA_HOME", None::<&str>),
                ("HOME", Some(home)),
            ],
            || {
                assert_eq!(data_dir(), d.path().join(".local/share/arca"));
            },
        );
        // HOME unset too → ./.local/share/arca
        temp_env::with_vars(
            [
                ("ARCA_HOME", None::<&str>),
                ("XDG_DATA_HOME", None::<&str>),
                ("HOME", None::<&str>),
            ],
            || {
                assert_eq!(data_dir(), PathBuf::from("./.local/share/arca"));
            },
        );
    }

    #[test]
    fn contended_open_retries_until_free() {
        let (_d, i) = idx();
        // a second "arca" holds the lock for a beat, then drops
        let holder = Database::create(i.db_path().as_std_path()).unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(80));
            drop(holder);
        });
        // write queues behind the holder instead of dying
        let e = entry("busy");
        i.save_snapshot(&snap(&e, "h1")).unwrap();
        release.join().unwrap();
        assert_eq!(i.snapshot(&e.path).unwrap().unwrap().head_commit, "h1");
    }

    #[test]
    fn contended_open_reports_index_busy() {
        let (_d, i) = idx();
        let _holder = Database::create(i.db_path().as_std_path()).unwrap();
        // test-scale budget — same path, no 2s wait
        let err = match open_db(&i.db_path(), Duration::from_millis(50)) {
            Ok(_) => panic!("expected busy error"),
            Err(e) => e,
        };
        match err {
            Error::Backend(msg) => assert!(msg.contains("index busy"), "got: {msg}"),
            e => panic!("expected backend error, got {e:?}"),
        }
    }

    #[test]
    fn unreadable_registry_is_io_error() {
        let (_d, i) = idx();
        // repos.toml as a directory — exists but can't be read
        std::fs::create_dir(i.registry_path()).unwrap();
        assert!(matches!(i.repos(), Err(Error::Io(_))));
    }
}

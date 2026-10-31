//! Index port — the derived, disposable cache that powers
//! `arca org` views.
//!
//! Markdown in `project/` stays canonical; a [`RepoSnapshot`] is a
//! frozen scan of one repo's ledger keyed by the project-branch
//! HEAD commit it was taken at. Freshness is a one-line check —
//! `snapshot.head_commit == git rev-parse HEAD` — so callers can
//! rebuild exactly the stale repos. Deleting the whole index is
//! always safe: `arca index rebuild` derives it back.
//!
//! Impls: `arca-index-redb` (`~/.local/share/arca/`).

use std::collections::BTreeSet;

use arca_core::{Item, ItemId};
use camino::{Utf8Path, Utf8PathBuf};

/// A registered repo — an entry in the index's registry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RepoEntry {
    /// Canonical repo-root path — the registry key.
    pub path: Utf8PathBuf,
    /// Display name (project `meta.toml` name at registration).
    pub name: String,
}

impl RepoEntry {
    /// New registry entry.
    pub fn new(path: Utf8PathBuf, name: impl Into<String>) -> Self {
        RepoEntry {
            path,
            name: name.into(),
        }
    }
}

/// A frozen scan of one repo's `project/` ledger.
///
/// `items` holds every item found — TODO/BACKLOG lines and
/// file-backed metas, live and archived. `archived` marks which ids
/// live under `<dir>/archive/` so views can split "resolved" from
/// "live".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RepoSnapshot {
    /// Canonical repo-root path — the snapshot key.
    pub path: Utf8PathBuf,
    /// Project name (from `meta.toml` / `TODO.md`).
    pub name: String,
    /// The project-worktree `HEAD` this scan was taken at —
    /// freshness is `head_commit == current HEAD`.
    pub head_commit: String,
    /// RFC 3339 timestamp of the scan.
    pub scanned_at: String,
    /// Every item in the ledger (any status, live + archived).
    pub items: Vec<Item>,
    /// Ids whose files live under `archive/`.
    #[serde(default)]
    pub archived: BTreeSet<ItemId>,
}

impl RepoSnapshot {
    /// Items not archived — the live ledger. Nested children ride
    /// inside their parents in `items`; they flatten out here so
    /// org views count them like any item.
    pub fn live_items(&self) -> impl Iterator<Item = &Item> {
        self.items
            .iter()
            .flat_map(Item::subtree)
            .filter(|i| !self.archived.contains(&i.id))
    }
}

/// The index port — registry + snapshot store.
pub trait Index {
    /// Register (or re-register) a repo root.
    fn register(&self, repo: &RepoEntry) -> Result<(), Error>;

    /// Unregister a repo and drop its snapshot. `Ok(true)` when it
    /// was registered.
    fn remove(&self, path: &Utf8Path) -> Result<bool, Error>;

    /// Every registered repo, sorted by path.
    fn repos(&self) -> Result<Vec<RepoEntry>, Error>;

    /// The cached snapshot for `path`, `None` when absent.
    fn snapshot(&self, path: &Utf8Path) -> Result<Option<RepoSnapshot>, Error>;

    /// Write (or overwrite) a repo's snapshot.
    fn save_snapshot(&self, snapshot: &RepoSnapshot) -> Result<(), Error>;
}

/// Port error — impls map backend failures into these variants.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Filesystem failure (registry file, db path).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// The snapshot couldn't be (de)serialized.
    #[error("snapshot codec: {0}")]
    Codec(String),

    /// Backend failure (redb, …).
    #[error("index: {0}")]
    Backend(String),
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use arca_core::{ItemId, ItemKind, Status};

    #[test]
    fn entry_and_snapshot_serde() {
        let e = RepoEntry::new(Utf8PathBuf::from("/repo"), "arca");
        let snap = RepoSnapshot {
            path: e.path.clone(),
            name: "arca".into(),
            head_commit: "deadbeef".into(),
            scanned_at: "2026-02-09T00:00:00Z".into(),
            items: vec![
                Item::new(ItemId::new(ItemKind::Task, 1), "open"),
                Item::new(ItemId::new(ItemKind::Issue, 2), "done").status(Status::Done),
                Item::new(ItemId::new(ItemKind::Issue, 3), "arch"),
            ],
            archived: BTreeSet::from([ItemId::new(ItemKind::Issue, 3)]),
        };
        let json = serde_json::to_string(&snap).unwrap();
        let back: RepoSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, snap);
        // live = non-archived (done still counts — views filter it)
        let live: Vec<ItemId> = back.live_items().map(|i| i.id).collect();
        assert_eq!(
            live,
            [
                ItemId::new(ItemKind::Task, 1),
                ItemId::new(ItemKind::Issue, 2)
            ]
        );
    }

    #[test]
    fn error_display() {
        assert_eq!(Error::Codec("x".into()).to_string(), "snapshot codec: x");
        assert_eq!(Error::Backend("y".into()).to_string(), "index: y");
    }
}

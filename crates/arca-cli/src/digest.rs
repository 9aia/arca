//! Ledger scans + counts — the shared read path for the view
//! verbs (`status`, `board`, `report`, `index rebuild`, `org`).
//!
//! A [`Scan`] flattens a worktree into one item list plus the set
//! of archived ids; [`Counts`] buckets it into the numbers every
//! view renders. Snapshots in `arca-index` carry the same pair, so
//! org views re-run the identical counting offline.

use std::collections::BTreeSet;

use arca_core::{Item, ItemId, ItemKind, Status};
use arca_store::Store;
use arca_store_md::MdStore;

/// File-backed kinds, in ledger order.
pub const FILE_KINDS: [ItemKind; 4] = [
    ItemKind::Issue,
    ItemKind::Proposal,
    ItemKind::Adr,
    ItemKind::Plan,
];

/// One worktree scanned into a flat item list.
pub struct Scan {
    /// Project name (`meta.toml` → `TODO.md` title → dir name).
    pub name: String,
    /// Every item — TODO/BACKLOG lines + file metas, live + archived.
    pub items: Vec<Item>,
    /// Ids whose files live under `archive/`.
    pub archived: BTreeSet<ItemId>,
}

impl Scan {
    /// Non-archived items (any status) — nested children count as
    /// items everywhere, so the whole tree flattens into views.
    pub fn live(&self) -> impl Iterator<Item = &Item> {
        self.items
            .iter()
            .flat_map(Item::subtree)
            .filter(|i| !self.archived.contains(&i.id))
    }

    /// Live items with an active status — not done, not deferred.
    /// These are the items `status`, `board`, `report`, and the
    /// prompt view care about.
    pub fn active(&self) -> impl Iterator<Item = &Item> {
        self.live()
            .filter(|i| i.status != Status::Done && i.status != Status::Deferred)
    }

    /// Live items in one status.
    pub fn in_status(&self, status: Status) -> Vec<&Item> {
        self.live().filter(|i| i.status == status).collect()
    }
}

/// Scan a worktree's whole ledger through the store.
pub fn scan(store: &MdStore) -> Result<Scan, String> {
    let name = store.meta().map_err(|e| e.to_string())?.name;
    let mut items = Vec::new();
    items.extend(store.todo().map_err(|e| e.to_string())?.items().cloned());
    items.extend(store.backlog().map_err(|e| e.to_string())?.items().cloned());
    let mut archived = BTreeSet::new();
    for kind in FILE_KINDS {
        for f in store.list_items(kind).map_err(|e| e.to_string())? {
            items.push(f.meta);
        }
        for f in store.list_archived(kind).map_err(|e| e.to_string())? {
            archived.insert(f.meta.id);
            items.push(f.meta);
        }
    }
    Ok(Scan {
        name,
        items,
        archived,
    })
}

/// A live-vs-resolved count for one file kind.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct OpenResolved {
    /// Live and not `done`.
    pub open: usize,
    /// `done` or archived.
    pub resolved: usize,
}

/// The numbers every view renders.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    /// Tasks in TODO.md, by active status.
    pub tasks_open: usize,
    /// Tasks owned but not started.
    pub tasks_claimed: usize,
    /// Tasks being worked.
    pub tasks_in_progress: usize,
    /// Tasks stuck on a dep.
    pub tasks_blocked: usize,
    /// Deferred tasks (BACKLOG.md).
    pub backlog: usize,
    /// Issues.
    pub issues: OpenResolved,
    /// Proposals.
    pub proposals: OpenResolved,
    /// ADRs.
    pub adrs: OpenResolved,
    /// Plans.
    pub plans: OpenResolved,
}

impl Counts {
    /// Fold another repo's counts in — org totals.
    pub fn add(&mut self, o: &Counts) {
        self.tasks_open += o.tasks_open;
        self.tasks_claimed += o.tasks_claimed;
        self.tasks_in_progress += o.tasks_in_progress;
        self.tasks_blocked += o.tasks_blocked;
        self.backlog += o.backlog;
        self.issues.open += o.issues.open;
        self.issues.resolved += o.issues.resolved;
        self.proposals.open += o.proposals.open;
        self.proposals.resolved += o.proposals.resolved;
        self.adrs.open += o.adrs.open;
        self.adrs.resolved += o.adrs.resolved;
        self.plans.open += o.plans.open;
        self.plans.resolved += o.plans.resolved;
    }

    /// `{open}o` or `{open}o/{resolved}r` — the compact per-kind
    /// fragment used by `--prompt` and org lines.
    fn or_short(c: OpenResolved) -> String {
        if c.resolved > 0 {
            format!("{}o/{}r", c.open, c.resolved)
        } else {
            format!("{}o", c.open)
        }
    }

    /// `tasks 3o/1c/2ip/1b backlog 4 issues 2o/1r p 1o pl 1o adr 2o`
    /// — the counts one-liner shared by human + prompt + org.
    pub fn oneline(&self) -> String {
        format!(
            "tasks {o}o/{c}c/{i}ip/{b}b backlog {bl} issues {i2} p {p} pl {pl} adr {a}",
            o = self.tasks_open,
            c = self.tasks_claimed,
            i = self.tasks_in_progress,
            b = self.tasks_blocked,
            bl = self.backlog,
            i2 = Self::or_short(self.issues),
            p = Self::or_short(self.proposals),
            pl = Self::or_short(self.plans),
            a = Self::or_short(self.adrs),
        )
    }

    /// Structured form for `--json`.
    pub fn to_json(self) -> serde_json::Value {
        let pair = |c: OpenResolved| serde_json::json!({"open": c.open, "resolved": c.resolved});
        serde_json::json!({
            "tasks": {
                "open": self.tasks_open,
                "claimed": self.tasks_claimed,
                "in-progress": self.tasks_in_progress,
                "blocked": self.tasks_blocked,
            },
            "backlog": self.backlog,
            "issues": pair(self.issues),
            "proposals": pair(self.proposals),
            "plans": pair(self.plans),
            "adrs": pair(self.adrs),
        })
    }
}

/// Bucket a scan into [`Counts`]. Items live in `scan.items` /
/// `scan.archived` — the same shape a `RepoSnapshot` carries, so
/// this also counts org snapshots.
pub fn counts(scan: &Scan) -> Counts {
    let mut c = Counts::default();
    for i in scan.items.iter().flat_map(Item::subtree) {
        let archived = scan.archived.contains(&i.id);
        match i.id.kind() {
            ItemKind::Task => match i.status {
                Status::Open => c.tasks_open += 1,
                Status::Claimed => c.tasks_claimed += 1,
                Status::InProgress => c.tasks_in_progress += 1,
                Status::Blocked => c.tasks_blocked += 1,
                Status::Deferred => c.backlog += 1,
                Status::Done => {} // done tasks leave TODO.md
            },
            kind => {
                let slot = match kind {
                    ItemKind::Issue => &mut c.issues,
                    ItemKind::Proposal => &mut c.proposals,
                    ItemKind::Adr => &mut c.adrs,
                    // tasks took the first arm — only plans remain
                    ItemKind::Plan | ItemKind::Task => &mut c.plans,
                };
                if archived || i.status == Status::Done {
                    slot.resolved += 1;
                } else {
                    slot.open += 1;
                }
            }
        }
    }
    c
}

/// Sort key for view lists — critical first, then id.
pub fn pri_then_id(a: &Item, b: &Item) -> std::cmp::Ordering {
    b.priority.cmp(&a.priority).then_with(|| a.id.cmp(&b.id))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use arca_core::{ItemFile, ItemId, Priority};

    fn t(n: u32, s: Status) -> Item {
        Item::new(ItemId::new(ItemKind::Task, n), format!("t{n}")).status(s)
    }
    fn f(kind: ItemKind, n: u32, s: Status) -> ItemFile {
        ItemFile::new(Item::new(ItemId::new(kind, n), format!("{n}")).status(s))
    }

    fn scan_with(items: Vec<Item>, archived: &[ItemId]) -> Scan {
        Scan {
            name: "x".into(),
            items,
            archived: archived.iter().copied().collect(),
        }
    }

    #[test]
    fn counts_buckets_everything() {
        let s = scan_with(
            vec![
                t(1, Status::Open),
                t(2, Status::Claimed),
                t(3, Status::InProgress),
                t(4, Status::Blocked),
                t(5, Status::Deferred),
                t(6, Status::Done),
                f(ItemKind::Issue, 1, Status::Open).meta,
                f(ItemKind::Issue, 2, Status::Done).meta,
                f(ItemKind::Issue, 3, Status::Open).meta, // archived → resolved
                f(ItemKind::Proposal, 1, Status::Open).meta,
                f(ItemKind::Plan, 1, Status::Done).meta,
                f(ItemKind::Adr, 1, Status::Open).meta,
            ],
            &[ItemId::new(ItemKind::Issue, 3)],
        );
        let c = counts(&s);
        assert_eq!(c.tasks_open, 1);
        assert_eq!(c.tasks_claimed, 1);
        assert_eq!(c.tasks_in_progress, 1);
        assert_eq!(c.tasks_blocked, 1);
        assert_eq!(c.backlog, 1);
        assert_eq!(
            c.issues,
            OpenResolved {
                open: 1,
                resolved: 2
            }
        );
        assert_eq!(
            c.proposals,
            OpenResolved {
                open: 1,
                resolved: 0
            }
        );
        assert_eq!(
            c.plans,
            OpenResolved {
                open: 0,
                resolved: 1
            }
        );
        assert_eq!(
            c.adrs,
            OpenResolved {
                open: 1,
                resolved: 0
            }
        );
        // oneline + json
        let line = c.oneline();
        assert!(line.contains("tasks 1o/1c/1ip/1b"), "{line}");
        assert!(line.contains("issues 1o/2r"), "{line}");
        assert!(line.contains("p 1o"), "{line}");
        assert!(line.contains("pl 0o/1r"), "{line}");
        let j = c.to_json();
        assert_eq!(j["tasks"]["in-progress"], 1);
        assert_eq!(j["issues"]["resolved"], 2);
        // add folds totals
        let mut total = Counts::default();
        total.add(&c);
        total.add(&c);
        assert_eq!(total.tasks_open, 2);
        assert_eq!(total.issues.resolved, 4);
    }

    #[test]
    fn scan_views() {
        let s = scan_with(
            vec![
                t(1, Status::Open),
                t(2, Status::Done),
                t(3, Status::Deferred),
                t(4, Status::InProgress),
            ],
            &[],
        );
        assert_eq!(s.live().count(), 4);
        assert_eq!(s.active().count(), 2); // open + in-progress
        assert_eq!(s.in_status(Status::Open).len(), 1);
    }

    #[test]
    fn pri_then_id_sorts_crit_first() {
        let mut v = [
            Item::new(ItemId::new(ItemKind::Task, 3), "c").priority(Priority::Low),
            Item::new(ItemId::new(ItemKind::Task, 1), "a").priority(Priority::Critical),
            Item::new(ItemId::new(ItemKind::Task, 2), "b"),
        ];
        v.sort_by(pri_then_id);
        let ids: Vec<u32> = v.iter().map(|i| i.id.number()).collect();
        assert_eq!(ids, [1, 2, 3]);
    }

    #[test]
    fn scan_reads_a_real_store() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(dir.path().join("p")).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let store = MdStore::new(root);
        let mut todo = arca_core::TodoDoc::new("p");
        todo.sections.push(arca_core::Section {
            name: None,
            items: vec![t(1, Status::InProgress)],
        });
        store.save_todo(&todo).unwrap();
        store
            .save_item(&f(ItemKind::Issue, 1, Status::Open))
            .unwrap();
        let s = scan(&store).unwrap();
        assert_eq!(s.name, "p");
        assert_eq!(s.items.len(), 2);
        let c = counts(&s);
        assert_eq!(c.tasks_in_progress, 1);
        assert_eq!(c.issues.open, 1);
    }
}

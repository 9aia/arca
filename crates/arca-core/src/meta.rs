//! `meta.toml` model + the pure id-allocation logic.
//!
//! Counters are **persistent** — done tasks leave `TODO.md` and ids
//! must never be reused, so `TODO.md` alone can't tell you the next
//! `T-NNN`. [`Registry`] computes next ids from counters plus a set
//! of observed ids (reconcile = counter ≥ max-seen + 1), which is
//! what the store uses to recover from hand-edits.

use crate::{ItemId, ItemKind};

const fn one() -> u32 {
    1
}

/// Per-kind next-counters — `meta.toml`'s `[next]` table.
/// Missing keys default to 1 so a partial `[next]` survives a
/// hand-edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct NextCounters {
    /// Next `T-NNN`.
    #[serde(default = "one")]
    pub t: u32,
    /// Next `I-NNN`.
    #[serde(default = "one")]
    pub i: u32,
    /// Next `P-NNN`.
    #[serde(default = "one")]
    pub p: u32,
    /// Next `ADR-NNNN`.
    #[serde(default = "one")]
    pub adr: u32,
    /// Next `PL-NNN`.
    #[serde(default = "one")]
    pub pl: u32,
}

impl NextCounters {
    /// Counter for `kind`.
    pub fn get(&self, kind: ItemKind) -> u32 {
        match kind {
            ItemKind::Task => self.t,
            ItemKind::Issue => self.i,
            ItemKind::Proposal => self.p,
            ItemKind::Adr => self.adr,
            ItemKind::Plan => self.pl,
        }
    }

    /// Set counter for `kind`.
    pub fn set(&mut self, kind: ItemKind, n: u32) {
        match kind {
            ItemKind::Task => self.t = n,
            ItemKind::Issue => self.i = n,
            ItemKind::Proposal => self.p = n,
            ItemKind::Adr => self.adr = n,
            ItemKind::Plan => self.pl = n,
        }
    }
}

impl Default for NextCounters {
    /// Counters start at 1 — `T-000` is not a valid id.
    fn default() -> Self {
        NextCounters {
            t: 1,
            i: 1,
            p: 1,
            adr: 1,
            pl: 1,
        }
    }
}

/// `meta.toml` — the one piece of project state that isn't
/// derivable from the item files.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct ProjectMeta {
    /// Repo/project name — used in `# TODO — <name>` and views.
    pub name: String,
    /// Next-id counters per kind.
    #[serde(default)]
    pub next: NextCounters,
}

impl ProjectMeta {
    /// Fresh meta for `arca init`.
    pub fn new(name: impl Into<String>) -> Self {
        ProjectMeta {
            name: name.into(),
            next: NextCounters::default(),
        }
    }

    /// A [`Registry`] seeded from these counters.
    pub fn registry(&self) -> Registry {
        Registry {
            counters: self.next,
        }
    }
}

/// Id allocator — pure. Seed from [`ProjectMeta::registry`],
/// [`observe_all`][Registry::observe_all] every known id, then
/// [`alloc`][Registry::alloc]; take the bumped counters back out via
/// [`counters`][Registry::counters] to persist.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    counters: NextCounters,
}

impl Registry {
    /// The id `alloc` would return for `kind` without bumping.
    pub fn peek(&self, kind: ItemKind) -> ItemId {
        ItemId::new(kind, self.counters.get(kind))
    }

    /// Record an existing id — bumps its kind's counter past it.
    /// Never lowers a counter.
    pub fn observe(&mut self, id: ItemId) {
        let floor = id.number().saturating_add(1);
        let cur = self.counters.get(id.kind());
        if floor > cur {
            self.counters.set(id.kind(), floor);
        }
    }

    /// Record many existing ids — the reconcile pass after a scan.
    pub fn observe_all(&mut self, ids: impl IntoIterator<Item = ItemId>) {
        for id in ids {
            self.observe(id);
        }
    }

    /// Take the next id for `kind` and bump the counter.
    pub fn alloc(&mut self, kind: ItemKind) -> ItemId {
        let id = self.peek(kind);
        self.counters.set(kind, id.number().saturating_add(1));
        id
    }

    /// The counters to write back to `meta.toml`.
    pub fn counters(&self) -> NextCounters {
        self.counters
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn counters_start_at_one() {
        let n = NextCounters::default();
        for k in ItemKind::ALL {
            assert_eq!(n.get(k), 1);
        }
        let meta = ProjectMeta::new("arca");
        assert_eq!(meta.name, "arca");
        assert_eq!(meta.next, n);
    }

    #[test]
    fn alloc_bumps_per_kind() {
        let mut r = Registry::default();
        assert_eq!(r.alloc(ItemKind::Task), ItemId::new(ItemKind::Task, 1));
        assert_eq!(r.alloc(ItemKind::Task), ItemId::new(ItemKind::Task, 2));
        assert_eq!(r.alloc(ItemKind::Issue), ItemId::new(ItemKind::Issue, 1));
        assert_eq!(r.peek(ItemKind::Task), ItemId::new(ItemKind::Task, 3));
        assert_eq!(r.peek(ItemKind::Adr), ItemId::new(ItemKind::Adr, 1));
    }

    #[test]
    fn observe_raises_floor_never_lowers() {
        let mut r = Registry::default();
        r.observe(ItemId::new(ItemKind::Task, 40));
        assert_eq!(r.peek(ItemKind::Task), ItemId::new(ItemKind::Task, 41));
        // observing a lower id doesn't lower the counter
        r.observe(ItemId::new(ItemKind::Task, 5));
        assert_eq!(r.peek(ItemKind::Task), ItemId::new(ItemKind::Task, 41));
        // other kinds untouched
        assert_eq!(r.peek(ItemKind::Plan), ItemId::new(ItemKind::Plan, 1));
        // counters written back
        let c = r.counters();
        assert_eq!(c.t, 41);
        assert_eq!(c.pl, 1);
    }

    #[test]
    fn observe_all_reconciles() {
        let mut meta = ProjectMeta::new("x");
        meta.next.t = 2; // stale: file actually contains T-012
        let mut r = meta.registry();
        r.observe_all([
            ItemId::new(ItemKind::Task, 12),
            ItemId::new(ItemKind::Task, 3),
        ]);
        assert_eq!(r.alloc(ItemKind::Task), ItemId::new(ItemKind::Task, 13));
    }

    #[test]
    fn counter_get_set_each_kind() {
        let mut n = NextCounters::default();
        for (i, k) in ItemKind::ALL.into_iter().enumerate() {
            n.set(k, (i as u32) + 10);
        }
        for (i, k) in ItemKind::ALL.into_iter().enumerate() {
            assert_eq!(n.get(k), (i as u32) + 10);
        }
    }

    #[cfg(feature = "schema")]
    #[test]
    fn json_schema_compiles() {
        let schema = schemars::schema_for!(ProjectMeta);
        assert!(serde_json::to_value(&schema).is_ok());
    }
}

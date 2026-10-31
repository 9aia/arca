//! The work item — shared shape for TODO/backlog line items and
//! issue/proposal/adr/plan file metadata (`ItemFile` adds a body).

use std::collections::BTreeSet;

use crate::{Actor, ItemId, Priority, Status};

/// A work item's metadata.
///
/// `created` is `Option<Date>` — the spec grammar
/// (`- [ ] T-012 title · pri:high · #tag`) has no date slot, so a
/// missing `created` must stay representable; `arca check` can flag
/// it later. Emit always writes `created:` when present.
///
/// `note` is reserved for handoffs (`arca handoff --note`); kept in
/// the model now so phase 6 doesn't re-shape it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Item {
    /// Typed id — determines kind, sort, filename.
    pub id: ItemId,
    /// One-line summary. No `·`, no newlines in line-grammar
    /// contexts (enforced at emit, not here — `Item` is a value,
    /// not a validator).
    pub title: String,
    /// Lifecycle state — transitions are validated by the caller
    /// via [`Status::transition_to`].
    pub status: Status,
    /// `pri:` — defaults to [`Priority::Medium`].
    #[serde(default)]
    pub priority: Priority,
    /// `owner:` — `devin`, `human:luis`, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(with = "Option<String>"))]
    pub owner: Option<Actor>,
    /// `#label`s — a sorted set, duplicates impossible.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub labels: BTreeSet<String>,
    /// `blocks:` — ids this item blocks. Sorted set.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub blocks: BTreeSet<ItemId>,
    /// `created:` — `YYYY-MM-DD` when known.
    #[serde(default, with = "crate::date::opt")]
    #[cfg_attr(feature = "schema", schemars(with = "Option<String>"))]
    pub created: Option<time::Date>,
    /// Free-form note — handoff/resume payload (phase 6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// `superseded_by:` — the item that replaced this one (ADRs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<ItemId>,
    /// `supersedes:` — the item this one replaces (ADRs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<ItemId>,
    /// `epic:` — the epic (named delivery grouping) this item
    /// belongs to. Line token `epic:<name>`; a group name, not a
    /// label — one per item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epic: Option<String>,
    /// `milestone:` — the milestone this item targets. Line token
    /// `ms:<name>` (kept short — lines stay readable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone: Option<String>,
    /// Nested sub-items — the `  - [ ]` lines indented under this
    /// one in TODO.md/BACKLOG.md. Children are full items: same
    /// metadata, same state machine. Depth is arbitrary (one
    /// level is the recommended shape); `done`/`defer` act on the
    /// whole subtree. Empty for file-backed kinds — children only
    /// exist in the line grammar.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Item>,
}

/// The metadata half of an [`crate::ItemFile`] — identical shape to
/// [`Item`]; the alias exists so signatures read as "frontmatter,
/// not body".
pub type ItemMeta = Item;

/// Pre-order DFS over an item subtree — a stack of pending nodes,
/// children pushed reversed so they pop in file order.
struct Subtree<'a> {
    stack: Vec<&'a Item>,
}

impl<'a> Iterator for Subtree<'a> {
    type Item = &'a Item;

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.stack.pop()?;
        for c in item.children.iter().rev() {
            self.stack.push(c);
        }
        Some(item)
    }
}

impl Item {
    /// Minimal item: id + title, open, medium, nothing else.
    pub fn new(id: ItemId, title: impl Into<String>) -> Self {
        Item {
            id,
            title: title.into(),
            status: Status::Open,
            priority: Priority::Medium,
            owner: None,
            labels: BTreeSet::new(),
            blocks: BTreeSet::new(),
            created: None,
            note: None,
            superseded_by: None,
            supersedes: None,
            epic: None,
            milestone: None,
            children: Vec::new(),
        }
    }

    /// `self` plus every descendant, pre-order — the subtree
    /// flattened (each parent immediately before its children).
    /// Id lookups, dup checks, counts, and index scans all see
    /// nested items through this.
    pub fn subtree(&self) -> impl Iterator<Item = &Item> {
        Subtree { stack: vec![self] }
    }

    /// Every descendant (children, grandchildren, …), pre-order.
    pub fn descendants(&self) -> impl Iterator<Item = &Item> {
        self.subtree().skip(1)
    }

    /// Builder-style `status` setter (no transition validation —
    /// use [`Status::transition_to`] when advancing a real item).
    pub fn status(mut self, status: Status) -> Self {
        self.status = status;
        self
    }

    /// Builder-style `priority` setter.
    pub fn priority(mut self, p: Priority) -> Self {
        self.priority = p;
        self
    }

    /// Builder-style `owner` setter.
    pub fn owner(mut self, owner: Actor) -> Self {
        self.owner = Some(owner);
        self
    }

    /// Builder-style `created` setter.
    pub fn created(mut self, d: time::Date) -> Self {
        self.created = Some(d);
        self
    }

    /// Builder-style `note` setter.
    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// Add a `#label`.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.labels.insert(label.into());
        self
    }

    /// Add a `blocks:` dep.
    pub fn blocks(mut self, id: ItemId) -> Self {
        self.blocks.insert(id);
        self
    }

    /// Builder-style `superseded_by` setter (ADRs).
    pub fn superseded_by(mut self, id: ItemId) -> Self {
        self.superseded_by = Some(id);
        self
    }

    /// Builder-style `supersedes` setter (ADRs).
    pub fn supersedes(mut self, id: ItemId) -> Self {
        self.supersedes = Some(id);
        self
    }

    /// Builder-style `epic` setter.
    pub fn epic(mut self, name: impl Into<String>) -> Self {
        self.epic = Some(name.into());
        self
    }

    /// Builder-style `milestone` setter.
    pub fn milestone(mut self, name: impl Into<String>) -> Self {
        self.milestone = Some(name.into());
        self
    }

    /// Append a nested child (subtask) — line-grammar items only;
    /// file-backed kinds never carry children.
    pub fn child(mut self, child: Item) -> Self {
        self.children.push(child);
        self
    }

    /// `T-001 open pri:high owner:devin #core title` — the compact
    /// human list line.
    pub fn compact(&self) -> String {
        let mut s = format!("{} {} pri:{}", self.id, self.status, self.priority.token());
        if let Some(o) = &self.owner {
            s += &format!(" owner:{o}");
        }
        for l in &self.labels {
            s += &format!(" #{l}");
        }
        if let Some(e) = &self.epic {
            s += &format!(" epic:{e}");
        }
        if let Some(m) = &self.milestone {
            s += &format!(" ms:{m}");
        }
        if !self.title.is_empty() {
            s += &format!(" {}", self.title);
        }
        s
    }
}

/// Optional metadata applied to a freshly-created item — the parsed
/// `--pri/--owner/--label/--blocks/--epic/--milestone` bundle every
/// `add`/`new` front-end shares. Validated on `apply`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct NewItemMeta {
    /// `pri:` override — absent keeps the item default.
    pub priority: Option<Priority>,
    /// `owner:` actor.
    pub owner: Option<Actor>,
    /// `#label`s to add.
    pub labels: Vec<String>,
    /// `blocks:` ids.
    pub blocks: Vec<ItemId>,
    /// `epic:` group name.
    pub epic: Option<String>,
    /// `ms:` milestone name.
    pub milestone: Option<String>,
}

impl NewItemMeta {
    /// Apply the metadata to `item` — labels and group names are
    /// validated (invalid tokens can't be emitted by the file
    /// grammar).
    pub fn apply(&self, item: &mut Item) -> Result<(), String> {
        if let Some(p) = self.priority {
            item.priority = p;
        }
        if let Some(o) = &self.owner {
            item.owner = Some(o.clone());
        }
        for l in &self.labels {
            if !is_valid_label(l) {
                return Err(format!("invalid label {l:?}"));
            }
            item.labels.insert(l.clone());
        }
        for b in &self.blocks {
            item.blocks.insert(*b);
        }
        for (what, name) in [("epic", &self.epic), ("milestone", &self.milestone)] {
            if let Some(name) = name {
                if !is_valid_group_name(name) {
                    return Err(format!("invalid {what} name {name:?}"));
                }
            }
        }
        item.epic = self.epic.clone();
        item.milestone = self.milestone.clone();
        Ok(())
    }
}

/// Depth-tagged pre-order view — list UIs indent children under
/// their parent by it.
pub fn tree_view(items: &[Item]) -> Vec<(usize, &Item)> {
    fn go<'a>(item: &'a Item, depth: usize, out: &mut Vec<(usize, &'a Item)>) {
        out.push((depth, item));
        for c in &item.children {
            go(c, depth + 1, out);
        }
    }
    let mut out = Vec::new();
    for i in items {
        go(i, 0, &mut out);
    }
    out
}

/// A `#label` is `[A-Za-z0-9_-]+` — kept permissive on the model,
/// enforced at the file-grammar boundary.
pub fn is_valid_label(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// An `epic:`/`ms:` group name — `[A-Za-z0-9_.-]+`: the label
/// charset plus `.` for `1.0`-style milestones. Same boundary rule
/// as labels (the name must fit a `·`-separated line token).
pub fn is_valid_group_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{Actor, ItemId, ItemKind};

    #[test]
    fn compact_line() {
        let i = Item::new(ItemId::new(ItemKind::Task, 1), "the thing")
            .priority(Priority::High)
            .owner(Actor::new("devin").unwrap())
            .label("core")
            .epic("v2")
            .milestone("1.0");
        assert_eq!(
            i.compact(),
            "T-001 open pri:high owner:devin #core epic:v2 ms:1.0 the thing"
        );
        let bare = Item::new(ItemId::new(ItemKind::Task, 2), "");
        assert_eq!(bare.compact(), "T-002 open pri:med");
    }

    #[test]
    fn new_item_meta_validates() {
        let mut item = Item::new(ItemId::new(ItemKind::Task, 1), "x");
        NewItemMeta {
            labels: vec!["ok-label".into()],
            epic: Some("v2.0".into()),
            ..Default::default()
        }
        .apply(&mut item)
        .unwrap();
        assert!(item.labels.contains("ok-label"));
        assert!(NewItemMeta {
            labels: vec!["bad label".into()],
            ..Default::default()
        }
        .apply(&mut item)
        .is_err());
        assert!(NewItemMeta {
            milestone: Some("bad ms".into()),
            ..Default::default()
        }
        .apply(&mut item)
        .is_err());
    }

    #[test]
    fn tree_view_tags_depth() {
        let t = |n: u32| Item::new(ItemId::new(ItemKind::Task, n), format!("t{n}"));
        let items = vec![t(1).child(t(2).child(t(4))).child(t(3)), t(5)];
        let flat: Vec<(usize, u32)> = tree_view(&items)
            .into_iter()
            .map(|(d, i)| (d, i.id.number()))
            .collect();
        assert_eq!(flat, [(0, 1), (1, 2), (2, 4), (1, 3), (0, 5)]);
    }

    #[test]
    fn builder() {
        let id = ItemId::new(ItemKind::Task, 12);
        let d = crate::date::parse("2026-02-09").unwrap();
        let item = Item::new(id, "write phase 2")
            .status(Status::Claimed)
            .priority(Priority::High)
            .owner("devin".parse().unwrap())
            .created(d)
            .note("session 7")
            .label("core")
            .label("store")
            .blocks(ItemId::new(ItemKind::Task, 10));
        assert_eq!(item.id, id);
        assert_eq!(item.title, "write phase 2");
        assert_eq!(item.status, Status::Claimed);
        assert_eq!(item.priority, Priority::High);
        assert_eq!(item.owner.as_ref().map(Actor::as_str), Some("devin"));
        assert_eq!(item.created, Some(d));
        assert_eq!(item.note.as_deref(), Some("session 7"));
        assert!(item.labels.contains("core") && item.labels.contains("store"));
        assert!(item.blocks.contains(&ItemId::new(ItemKind::Task, 10)));
    }

    #[test]
    fn labels_and_blocks_are_sorted_sets() {
        let mut item = Item::new(ItemId::new(ItemKind::Task, 1), "x");
        item.labels.insert("b".into());
        item.labels.insert("a".into());
        item.labels.insert("a".into());
        item.blocks.insert(ItemId::new(ItemKind::Task, 3));
        item.blocks.insert(ItemId::new(ItemKind::Task, 2));
        let labels: Vec<&String> = item.labels.iter().collect();
        assert_eq!(labels, ["a", "b"]);
        assert_eq!(item.labels.len(), 2);
        let first = item.blocks.iter().next().unwrap();
        assert_eq!(*first, ItemId::new(ItemKind::Task, 2));
    }

    #[test]
    fn label_validation() {
        for ok in ["a", "core-2", "x_y", "A1"] {
            assert!(is_valid_label(ok), "{ok}");
        }
        for bad in ["", "a b", "a/b", "a·b", "é"] {
            assert!(!is_valid_label(bad), "{bad}");
        }
    }

    #[test]
    fn group_name_validation() {
        for ok in ["v2", "q3-launch", "1.0", "alpha_beta", "A1.x"] {
            assert!(is_valid_group_name(ok), "{ok}");
        }
        for bad in ["", "a b", "a/b", "a·b", "a:b", "é"] {
            assert!(!is_valid_group_name(bad), "{bad}");
        }
    }

    #[test]
    fn serde_skips_defaults() {
        let item = Item::new(ItemId::new(ItemKind::Task, 1), "t");
        let json = serde_json::to_value(&item).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("owner"));
        assert!(!obj.contains_key("labels"));
        assert!(!obj.contains_key("blocks"));
        assert!(!obj.contains_key("note"));
        assert!(!obj.contains_key("epic"));
        assert!(!obj.contains_key("milestone"));
        assert!(!obj.contains_key("children"));
        // full item round-trips — children included
        let full = Item::new(ItemId::new(ItemKind::Issue, 2), "t")
            .owner("devin".parse().unwrap())
            .created(crate::date::parse("2026-01-01").unwrap())
            .note("n")
            .label("l")
            .blocks(ItemId::new(ItemKind::Task, 1))
            .superseded_by(ItemId::new(ItemKind::Adr, 2))
            .supersedes(ItemId::new(ItemKind::Adr, 1))
            .epic("v2")
            .milestone("1.0")
            .child(
                Item::new(ItemId::new(ItemKind::Task, 7), "sub")
                    .child(Item::new(ItemId::new(ItemKind::Task, 8), "deep")),
            );
        let back: Item = serde_json::from_str(&serde_json::to_string(&full).unwrap()).unwrap();
        assert_eq!(back, full);
    }

    #[test]
    fn subtree_walks_preorder() {
        let t = |n: u32| Item::new(ItemId::new(ItemKind::Task, n), format!("t{n}"));
        let root = t(1).child(t(2).child(t(4))).child(t(3));
        let ids: Vec<u32> = root.subtree().map(|i| i.id.number()).collect();
        assert_eq!(ids, [1, 2, 4, 3]);
        let deep: Vec<u32> = root.descendants().map(|i| i.id.number()).collect();
        assert_eq!(deep, [2, 4, 3]);
        // a leaf subtree is just itself
        assert_eq!(t(9).subtree().count(), 1);
        assert_eq!(t(9).descendants().count(), 0);
    }

    #[cfg(feature = "schema")]
    #[test]
    fn json_schema_compiles() {
        let schema = schemars::schema_for!(Item);
        assert!(serde_json::to_value(&schema).is_ok());
    }
}

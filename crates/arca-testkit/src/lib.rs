//! arca-testkit — golden fixtures + conformance for the `project/`
//! file spec. `fixtures/` holds canonical and malformed files;
//! [`strat`] holds proptest strategies; later crates reuse both
//! (conformance is shared, not per-crate).
//!
//! Tests here freeze the spec:
//!
//! - insta snapshots — `emit(parse(fixture))` byte-for-byte
//! - proptest — `parse(emit(doc)) == doc` and emit idempotence

use std::path::{Path, PathBuf};

/// Absolute path to `fixtures/` (works from any test cwd).
pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

/// Read a fixture file, e.g. `fixture("todo/basic.md")`.
pub fn fixture(rel: &str) -> String {
    let path = fixtures_dir().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("fixture {rel} unreadable: {e}"))
}

/// Fixture paths (relative to `fixtures/`) under `dir`
/// (e.g. `"todo/invalid"`), sorted, `*.md`/`*.toml` only.
pub fn fixtures_under(dir: &str) -> Vec<String> {
    let root = fixtures_dir();
    let mut out = Vec::new();
    collect(&root, &root.join(dir), &mut out);
    out.sort();
    out
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, out);
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("md") | Some("toml")
        ) {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push(rel);
        }
    }
}

/// proptest strategies for `arca-core` models — constrained to what
/// the canonical grammar can express (see each fn's docs).
pub mod strat {
    use arca_core::{
        Actor, BacklogDoc, HandoffEntry, Item, ItemFile, ItemId, ItemKind, NextCounters, Priority,
        ProjectMeta, Section, Status, TodoDoc,
    };
    use proptest::prelude::*;
    use time::Date;

    /// Any kind.
    pub fn item_kind() -> impl Strategy<Value = ItemKind> {
        prop::sample::select(ItemKind::ALL.to_vec())
    }

    /// A file-backed kind (no `Task` — tasks live in TODO/BACKLOG).
    pub fn file_kind() -> impl Strategy<Value = ItemKind> {
        prop::sample::select(vec![
            ItemKind::Issue,
            ItemKind::Proposal,
            ItemKind::Adr,
            ItemKind::Plan,
        ])
    }

    /// `1..=9999` — padded ids stay in spec range.
    pub fn item_id(kind: ItemKind) -> impl Strategy<Value = ItemId> {
        (1u32..=9999).prop_map(move |n| ItemId::new(kind, n))
    }

    /// Any status.
    pub fn status() -> impl Strategy<Value = Status> {
        prop::sample::select(Status::ALL.to_vec())
    }

    /// Any priority.
    pub fn priority() -> impl Strategy<Value = Priority> {
        prop::sample::select(Priority::ALL.to_vec())
    }

    /// `[a-z0-9._-]{1,12}` optionally `human:`/`agent:` prefixed.
    pub fn actor() -> impl Strategy<Value = Actor> {
        (
            prop::sample::select(vec!["", "human:", "agent:"]),
            "[a-z0-9._-]{1,12}",
        )
            .prop_map(|(p, s)| Actor::new(&format!("{p}{s}")).expect("valid actor"))
    }

    /// `[a-z0-9_-]{1,10}` — a valid `#label`.
    pub fn label() -> impl Strategy<Value = String> {
        "[a-z0-9_-]{1,10}"
    }

    /// A `YYYY-MM-DD` date in the 2000s.
    pub fn date() -> impl Strategy<Value = Date> {
        (2000i32..=2099, 1u8..=12, 1u8..=28).prop_map(|(y, m, d)| {
            Date::from_calendar_date(y, time::Month::try_from(m).expect("1..=12"), d)
                .expect("valid date")
        })
    }

    /// Trimmed, nonempty, no `·`, no newlines — a legal title.
    /// Includes markdown specials to exercise inline escaping.
    pub fn title() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9*#_\\[\\](),.:' -]{1,60}".prop_map(|s| s.trim().to_string())
    }

    /// Optional trimmed section name (same rules as titles).
    pub fn section_name() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9 _.-]{1,20}"
            .prop_map(|s| s.trim().to_string())
            .prop_filter("nonempty after trim", |s| !s.is_empty())
    }

    /// `[a-z0-9._-]{1,12}` — a valid `epic:`/`ms:` group name.
    pub fn group_name() -> impl Strategy<Value = String> {
        "[a-z0-9._-]{1,12}"
    }

    /// A line-safe `note:` value — trimmed text without `·` or
    /// newlines (`src/x.rs:12`-style provenance strings).
    pub fn line_note() -> impl Strategy<Value = String> {
        "[a-z0-9 .:/_-]{1,40}".prop_map(|s| s.trim().to_string())
    }

    /// A leaf line item — full metadata, no children.
    fn leaf_item(forbid: &'static [Status]) -> impl Strategy<Value = Item> {
        (
            item_id(ItemKind::Task),
            title(),
            prop::sample::select(
                Status::ALL
                    .into_iter()
                    .filter(|s| !forbid.contains(s))
                    .collect::<Vec<_>>(),
            ),
            priority(),
            prop::option::of(actor()),
            prop::collection::btree_set(label(), 0..3),
            prop::collection::btree_set(item_id(ItemKind::Task), 0..3),
            prop::option::of(date()),
            prop::option::of(group_name()),
            prop::option::of(group_name()),
            prop::option::of(line_note()),
        )
            .prop_map(
                |(id, title, status, priority, owner, labels, blocks, created, epic, ms, note)| {
                    let mut item = Item::new(id, title).status(status).priority(priority);
                    item.owner = owner;
                    item.labels = labels;
                    item.blocks = blocks;
                    item.created = created;
                    item.epic = epic;
                    item.milestone = ms;
                    item.note = note.filter(|n| !n.is_empty());
                    item
                },
            )
    }

    /// A task item valid for a line grammar — notes are the
    /// trimmed `note:` token kind only, and status avoids `forbid`
    /// at every level. Children are full items too; generated
    /// depth is ≤ 2 (one nesting level — the recommended shape,
    /// deeper nesting is legal but rare).
    pub fn line_item(forbid: &'static [Status]) -> impl Strategy<Value = Item> {
        (
            leaf_item(forbid),
            prop::collection::vec(leaf_item(forbid), 0..3),
        )
            .prop_map(|(mut item, children)| {
                item.children = children;
                item
            })
    }

    /// Set `status` on a whole subtree (deferred propagates to
    /// children — BACKLOG.md is deferred-only).
    fn status_tree(item: &mut Item, status: Status) {
        item.status = status;
        for c in &mut item.children {
            status_tree(c, status);
        }
    }

    fn sections(items: impl Strategy<Value = Item>) -> impl Strategy<Value = Vec<Section>> {
        prop::collection::vec(
            (
                prop::option::of(section_name()),
                prop::collection::vec(items, 0..4),
            ),
            0..4,
        )
        .prop_map(|v| {
            let mut out: Vec<Section> = Vec::new();
            let mut first_unsectioned = true;
            for (name, items) in v {
                match name {
                    Some(n) => out.push(Section {
                        name: Some(n),
                        items,
                    }),
                    None => {
                        // unsectioned only legal at index 0, and
                        // needs items to be representable
                        if first_unsectioned && out.is_empty() && !items.is_empty() {
                            out.push(Section { name: None, items });
                            first_unsectioned = false;
                        }
                    }
                }
            }
            out
        })
    }

    /// A `TodoDoc` — statuses exclude `Done`, task ids only.
    pub fn todo_doc() -> impl Strategy<Value = TodoDoc> {
        ("[a-z0-9-]{1,16}", sections(line_item(&[Status::Done])))
            .prop_map(|(repo, sections)| TodoDoc { repo, sections })
    }

    /// A `BacklogDoc` — every item (children included) `Deferred`.
    pub fn backlog_doc() -> impl Strategy<Value = BacklogDoc> {
        sections(line_item(&[]).prop_map(|mut i| {
            status_tree(&mut i, Status::Deferred);
            i
        }))
        .prop_map(|sections| BacklogDoc { sections })
    }

    /// A `ProjectMeta` — arbitrary counters.
    pub fn project_meta() -> impl Strategy<Value = ProjectMeta> {
        (
            "[a-z0-9-]{1,16}",
            (1u32..=999, 1u32..=999, 1u32..=999, 1u32..=999, 1u32..=999),
        )
            .prop_map(|(name, (t, i, p, adr, pl))| ProjectMeta {
                name,
                next: NextCounters { t, i, p, adr, pl },
            })
    }

    /// A session id — `[a-z0-9:-]{1,12}` (no `·`, no newline).
    pub fn session() -> impl Strategy<Value = String> {
        "[a-z0-9:-]{1,12}"
    }

    /// A `HandoffEntry` — notes are plain text lines (never a `##`
    /// heading, which the grammar can't carry inside a note).
    pub fn handoff_entry() -> impl Strategy<Value = HandoffEntry> {
        (
            date(),
            actor(),
            prop::option::of(session()),
            prop::collection::vec("[a-z0-9.:]([a-z0-9 .:]{0,38}[a-z0-9.:])?", 0..4),
        )
            .prop_map(|(date, actor, session, lines)| HandoffEntry {
                date,
                actor,
                session,
                note: lines.join("\n"),
            })
    }

    /// An `ItemFile` — any status, note allowed, trimmed body of
    /// simple markdown lines.
    pub fn item_file() -> impl Strategy<Value = ItemFile> {
        (
            file_kind().prop_flat_map(item_id),
            title(),
            status(),
            priority(),
            prop::option::of(actor()),
            prop::collection::btree_set(label(), 0..3),
            prop::collection::btree_set(item_id(ItemKind::Task), 0..2),
            prop::option::of(date()),
            prop::option::of("[a-z0-9 .-]{0,40}"),
            prop::option::of(group_name()),
            prop::option::of(group_name()),
            prop::collection::vec("(#+ |[a-z0-9 .-]+|- \\[[ x]\\] )[a-z0-9 .-]{0,30}", 0..6),
        )
            .prop_map(
                |(
                    id,
                    title,
                    status,
                    priority,
                    owner,
                    labels,
                    blocks,
                    created,
                    note,
                    epic,
                    ms,
                    lines,
                )| {
                    let mut item = Item::new(id, title).status(status).priority(priority);
                    item.owner = owner;
                    item.labels = labels;
                    item.blocks = blocks;
                    item.created = created;
                    item.note = note;
                    item.epic = epic;
                    item.milestone = ms;
                    let mut file = ItemFile::new(item);
                    file.body = lines.join("\n\n").trim().to_string();
                    file
                },
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_exist() {
        assert!(fixtures_dir().is_dir());
        assert!(!fixture("todo/basic.md").is_empty());
        assert!(!fixtures_under("todo/invalid").is_empty());
        // missing dirs yield nothing, not an error
        assert!(fixtures_under("nonexistent-dir").is_empty());
    }
}

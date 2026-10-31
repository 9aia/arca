//! Item ops — the I/O shell around `arca-core`'s domain: store
//! open/save, the commit-per-mutation tail, item-file reads, and
//! the shared output verbs. The pure pieces (ledger doc surgery,
//! state walks, `compact`, `tree_view`, `require_kind`,
//! `NewItemMeta`) live in `arca-core` and are re-exported here so
//! call sites read `items::X` either way.

use std::path::Path;

use arca_core::{Item, ItemFile, ItemId, ItemKind, Status};
use arca_store::Store;
use arca_store_md::MdStore;
use serde_json::json;

use crate::discover;
use crate::ops;

pub use arca_core::{
    require_kind, status_path, targets, tree_view, walk_descendants_to, walk_to, Ledger,
    NewItemMeta, TaskLoc,
};

/// `T-001 open pri:high owner:devin #core title` — the compact
/// human list line. Thin wrapper over [`Item::compact`].
pub fn compact(item: &Item) -> String {
    item.compact()
}

/// Load both ledger docs through the store.
pub fn load_ledger(store: &MdStore) -> Result<Ledger, String> {
    Ok(Ledger::new(
        store.todo().map_err(|e| e.to_string())?,
        store.backlog().map_err(|e| e.to_string())?,
    ))
}

/// Write both ledger docs back through the store.
pub fn save_ledger(store: &MdStore, ledger: &Ledger) -> Result<(), String> {
    store.save_todo(&ledger.todo).map_err(|e| e.to_string())?;
    store
        .save_backlog(&ledger.backlog)
        .map_err(|e| e.to_string())
}

/// Load a file-backed item for mutation. Archived items are
/// refused — `save_item` writes to the live dir, so editing one
/// would silently un-archive it.
pub fn item_for_edit(store: &MdStore, id: ItemId) -> Result<ItemFile, String> {
    if store.is_archived(id).map_err(|e| e.to_string())? {
        return Err(format!("{id} is archived — mutating it is refused"));
    }
    store.item(id).map_err(|e| e.to_string())
}

/// Today's date — `created:` values and comment timestamps.
pub fn today() -> time::Date {
    time::OffsetDateTime::now_utc().date()
}

/// Resolve the project + open the store — every item verb's start.
pub fn open(
    dir: Option<&Path>,
    root: Option<&Path>,
) -> Result<(discover::Project, MdStore), String> {
    let proj = discover::discover(dir, root)?;
    let store = ops::store_at(&proj.worktree)?;
    Ok((proj, store))
}

/// The standard mutation tail — one atomic commit on the worktree.
pub fn commit(wt: &Path, msg: &str, no_commit: bool) -> Result<bool, String> {
    ops::autocommit(wt, msg, ops::commits_enabled(no_commit))
}

/// Standard mutation output: `--json` gets `payload` plus a
/// `committed` key; human output is one line.
pub fn report(
    mut payload: serde_json::Value,
    committed: bool,
    json_out: bool,
    human: &str,
) -> Result<i32, String> {
    if json_out {
        payload["committed"] = json!(committed);
        ops::print_json(&payload);
    } else {
        println!("{human}");
    }
    Ok(0)
}

/// `--status/--owner/--label` list filter + `--limit` — the parsed
/// form of the CLI's `ListFlags`, usable by any front-end.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// Keep only items with this status.
    pub status: Option<Status>,
    /// Keep only items owned by this actor string.
    pub owner: Option<String>,
    /// Keep only items carrying all of these labels.
    pub labels: Vec<String>,
    /// Keep only the first N items after filtering.
    pub limit: Option<usize>,
}

impl Filter {
    /// Does `item` pass the filters?
    pub fn matches(&self, item: &Item) -> bool {
        self.status.is_none_or(|s| item.status == s)
            && self
                .owner
                .as_deref()
                .is_none_or(|o| item.owner.as_ref().map(arca_core::Actor::as_str) == Some(o))
            && self.labels.iter().all(|l| item.labels.contains(l))
    }

    /// `limit` as a `.take()` bound.
    pub fn take(&self) -> usize {
        self.limit.unwrap_or(usize::MAX)
    }
}

/// `--json` → a bare array of item objects; else compact lines.
pub fn print_list(items: &[Item], json_out: bool) -> Result<i32, String> {
    if json_out {
        let v: Vec<serde_json::Value> = items
            .iter()
            .map(|i| serde_json::to_value(i).unwrap_or(json!(null)))
            .collect();
        ops::print_json(&json!(v));
    } else {
        for i in items {
            println!("{}", compact(i));
        }
    }
    Ok(0)
}

/// Like [`print_list`] for depth-tagged items — human output
/// indents children 2 spaces per level under their parent.
pub fn print_tree(pairs: &[(usize, &Item)], json_out: bool) -> Result<i32, String> {
    if json_out {
        let v: Vec<serde_json::Value> = pairs
            .iter()
            .map(|(_, i)| serde_json::to_value(i).unwrap_or(json!(null)))
            .collect();
        ops::print_json(&json!(v));
    } else {
        for (depth, i) in pairs {
            for _ in 0..*depth {
                print!("  ");
            }
            println!("{}", compact(i));
        }
    }
    Ok(0)
}

/// Input bundle for [`new_file`] — one per `<kind> new` call.
/// `word` is the commit-message noun (`chore: new issue I-001`).
pub struct NewItem<'a> {
    /// The kind of item to create.
    pub kind: ItemKind,
    /// One-line title (drives the filename slug).
    pub title: String,
    /// Body markdown — `None` → `template` fills in.
    pub body: Option<&'a str>,
    /// Optional metadata (`pri:`/`owner:`/`#label`/…).
    pub meta: &'a NewItemMeta,
    /// The kind's default body template.
    pub template: fn(&Item) -> String,
    /// Commit-message noun — `chore: new <word> <id>`.
    pub word: &'a str,
    /// `--json` output.
    pub json_out: bool,
    /// `--no-commit`.
    pub no_commit: bool,
}

/// The shared `<kind> new` body — alloc id → apply meta → body or
/// `template` → `save_item` → commit + report.
pub fn new_file(
    proj: &discover::Project,
    store: &MdStore,
    spec: &NewItem<'_>,
) -> Result<i32, String> {
    let id = store.alloc_id(spec.kind).map_err(|e| e.to_string())?;
    let mut item = Item::new(id, &spec.title).created(today());
    spec.meta.apply(&mut item)?;
    let body = spec
        .body
        .map(str::to_string)
        .unwrap_or_else(|| (spec.template)(&item));
    let file = ItemFile::new(item.clone()).body(body);
    store.save_item(&file).map_err(|e| e.to_string())?;
    let rel = item_rel_path(&item);
    let committed = commit(
        &proj.worktree,
        &format!("chore: new {} {id}", spec.word),
        spec.no_commit,
    )?;
    report(
        json!({"item": item, "path": rel}),
        committed,
        spec.json_out,
        &format!("created {id} {rel}"),
    )
}

/// The read behind [`list_files`] — live item files (plus
/// `archive/` entries when `include_archived`), filtered, as
/// data. Non-CLI front-ends (`arca serve`) shape it themselves.
pub fn query_files(
    store: &MdStore,
    kind: ItemKind,
    include_archived: bool,
    filter: &Filter,
) -> Result<Vec<Item>, String> {
    let mut found = store.list_items(kind).map_err(|e| e.to_string())?;
    if include_archived {
        found.extend(store.list_archived(kind).map_err(|e| e.to_string())?);
    }
    Ok(found
        .into_iter()
        .map(|f| f.meta)
        .filter(|i| filter.matches(i))
        .take(filter.take())
        .collect())
}

/// The shared `<kind> list` — live item files, plus `archive/`
/// entries when `include_archived`.
pub fn list_files(
    store: &MdStore,
    kind: ItemKind,
    include_archived: bool,
    filter: &Filter,
    json_out: bool,
) -> Result<i32, String> {
    let found = query_files(store, kind, include_archived, filter)?;
    print_list(&found, json_out)
}

/// `<dir>/<filename>` for a file-backed item.
pub fn item_rel_path(item: &Item) -> String {
    let dir = arca_store_md::item_dir(item.id.kind()).unwrap_or("");
    let name =
        arca_store_md::item_filename(&item.id, &item.title).unwrap_or_else(|| item.id.to_string());
    format!("{dir}/{name}")
}

/// Locate an item anywhere in the ledger — TODO.md, BACKLOG.md, or
/// an item file (live or `archive/`). Returns the item, its
/// location label, and the body for file-backed items.
pub fn find_item(
    store: &MdStore,
    id: ItemId,
) -> Result<Option<(Item, String, Option<String>)>, String> {
    let ledger = load_ledger(store)?;
    if let Some(loc) = ledger.locate(id) {
        let file = if loc.backlog { "BACKLOG.md" } else { "TODO.md" };
        return Ok(Some((ledger.item(&loc).clone(), file.to_string(), None)));
    }
    match store.item(id) {
        Ok(f) => {
            let mut loc = item_rel_path(&f.meta);
            if store.is_archived(id).map_err(|e| e.to_string())? {
                if let Some((d, n)) = loc.split_once('/') {
                    loc = format!("{d}/archive/{n}");
                }
            }
            Ok(Some((f.meta.clone(), loc, Some(f.body))))
        }
        Err(arca_store::Error::NotFound { .. }) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use arca_core::ItemKind;

    #[test]
    fn ledger_load_save_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(dir.path().join("p")).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let store = MdStore::new(root);
        let mut ledger = load_ledger(&store).unwrap();
        // repo name comes from the dir — sections start empty
        assert!(ledger.todo.sections.is_empty());
        ledger.put_todo(Item::new(ItemId::new(ItemKind::Task, 1), "x"), None);
        save_ledger(&store, &ledger).unwrap();
        let reloaded = load_ledger(&store).unwrap();
        assert!(reloaded.locate(ItemId::new(ItemKind::Task, 1)).is_some());
    }
}

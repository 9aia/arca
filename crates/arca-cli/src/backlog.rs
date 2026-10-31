//! `arca backlog add|promote|list` — deferred items in BACKLOG.md.

use std::path::PathBuf;

use arca_core::{Item, ItemId, ItemKind, Status};
use arca_store::Store;
use clap::Subcommand;
use serde_json::json;

use crate::items;

#[derive(Subcommand)]
pub enum Cmd {
    /// Add a deferred item — allocs a T-id straight into BACKLOG.md.
    Add {
        /// One-line title.
        title: String,
        /// Section to add under (created when absent; default: the
        /// first section, or an unsectioned run).
        #[arg(long)]
        section: Option<String>,
        #[command(flatten)]
        meta: items::MetaFlags,
    },
    /// Re-activate a parked item — `deferred → open`, move the line
    /// to TODO.md.
    Promote {
        /// Task id (`T-NNN`).
        id: ItemId,
    },
    /// List backlog items.
    List {
        #[command(flatten)]
        list: items::ListFlags,
    },
}

/// The `backlog add` mutation — allocs a T-id straight into
/// BACKLOG.md (backlog items are born deferred). Caller commits.
pub fn add_item(
    store: &arca_store_md::MdStore,
    title: &str,
    section: Option<&str>,
    meta: &arca_core::NewItemMeta,
) -> Result<Item, String> {
    let id = store.alloc_id(ItemKind::Task).map_err(|e| e.to_string())?;
    // created deferred — backlog items are born parked
    let mut item = Item::new(id, title)
        .status(Status::Deferred)
        .created(items::today());
    meta.apply(&mut item)?;
    let mut ledger = items::load_ledger(store)?;
    ledger.put_backlog(item.clone(), section);
    items::save_ledger(store, &ledger)?;
    Ok(item)
}

/// The `backlog promote` mutation — `deferred → open`, moves the
/// line (subtree included) into TODO.md. Caller commits.
pub fn promote_item(store: &arca_store_md::MdStore, id: ItemId) -> Result<Item, String> {
    items::require_kind(id, ItemKind::Task, "backlog promote")?;
    let mut ledger = items::load_ledger(store)?;
    let loc = ledger
        .locate(id)
        .ok_or_else(|| format!("{id}: no such task"))?;
    if !loc.backlog {
        return Err(format!("{id} isn't in BACKLOG.md — nothing to promote"));
    }
    let mut item = ledger.take(&loc);
    items::walk_to(&mut item, Status::Open)?;
    // the subtree unparks with its parent — deferred
    // children come back to open (TODO.md holds no
    // deferred items)
    items::walk_descendants_to(&mut item, Status::Open)?;
    ledger.put_todo(item.clone(), None);
    items::save_ledger(store, &ledger)?;
    Ok(item)
}

/// The `backlog` verb group.
pub fn run(
    cmd: Cmd,
    dir: Option<PathBuf>,
    root: Option<PathBuf>,
    json_out: bool,
    no_commit: bool,
) -> Result<i32, String> {
    let (proj, store) = items::open(dir.as_deref(), root.as_deref())?;
    match cmd {
        Cmd::Add {
            title,
            section,
            meta,
        } => {
            let item = add_item(&store, &title, section.as_deref(), &meta.to_meta())?;
            let committed = items::commit(
                &proj.worktree,
                &format!("chore: add {}", item.id),
                no_commit,
            )?;
            items::report(
                json!({"item": item, "file": "BACKLOG.md"}),
                committed,
                json_out,
                &format!("added {}", items::compact(&item)),
            )
        }
        Cmd::Promote { id } => {
            let item = promote_item(&store, id)?;
            let committed =
                items::commit(&proj.worktree, &format!("chore: promote {id}"), no_commit)?;
            items::report(
                json!({"item": item, "file": "TODO.md"}),
                committed,
                json_out,
                &format!("promoted {id} → TODO.md"),
            )
        }
        Cmd::List { list } => {
            let all = crate::todo::doc_items(&store, true)?;
            let found: Vec<(usize, &Item)> = items::tree_view(&all)
                .into_iter()
                .filter(|(_, i)| list.matches(i))
                .take(list.take())
                .collect();
            items::print_tree(&found, json_out)
        }
    }
}

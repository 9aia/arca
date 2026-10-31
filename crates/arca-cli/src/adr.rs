//! `arca adr new|list|supersede` — architecture decision records
//! in `adrs/NNNN-<slug>.md` (Context / Decision / Consequences).

use std::path::PathBuf;

use arca_core::{ItemId, ItemKind, Status};
use arca_store::Store;
use clap::Subcommand;
use serde_json::json;

use crate::items;
use crate::templates;

#[derive(Subcommand)]
pub enum Cmd {
    /// File an ADR — allocs an ADR-id, writes the file with
    /// frontmatter (status `open` = "proposed") + template.
    New {
        /// One-line title (drives the filename slug).
        title: String,
        #[command(flatten)]
        body: items::BodyFlags,
        #[command(flatten)]
        meta: items::MetaFlags,
    },
    /// List ADRs.
    List {
        #[command(flatten)]
        list: items::ListFlags,
    },
    /// Mark an ADR superseded by another — the old file gets
    /// `superseded_by:` + status `done`, the replacement gets a
    /// reciprocal `supersedes:` key. Both must already exist.
    Supersede {
        /// The ADR being replaced (`ADR-NNNN`).
        id: ItemId,
        /// The ADR replacing it (`ADR-NNNN`).
        #[arg(long)]
        by: ItemId,
    },
}

/// The `adr` verb group.
pub fn run(
    cmd: Cmd,
    dir: Option<PathBuf>,
    root: Option<PathBuf>,
    json_out: bool,
    no_commit: bool,
) -> Result<i32, String> {
    let (proj, store) = items::open(dir.as_deref(), root.as_deref())?;
    match cmd {
        Cmd::New { title, body, meta } => items::new_file(
            &proj,
            &store,
            &items::NewItem {
                kind: ItemKind::Adr,
                title,
                body: body.read()?.as_deref(),
                meta: &meta.to_meta(),
                template: templates::adr,
                word: "adr",
                json_out,
                no_commit,
            },
        ),
        Cmd::List { list } => {
            items::list_files(&store, ItemKind::Adr, false, &list.filter(), json_out)
        }
        Cmd::Supersede { id, by } => {
            items::require_kind(id, ItemKind::Adr, "adr supersede")?;
            items::require_kind(by, ItemKind::Adr, "adr supersede --by")?;
            if id == by {
                return Err(format!("{id} can't supersede itself"));
            }
            let mut old = items::item_for_edit(&store, id)?;
            let mut new = items::item_for_edit(&store, by)?;
            items::walk_to(&mut old.meta, Status::Done)?;
            old.meta.superseded_by = Some(by);
            new.meta.supersedes = Some(id);
            store.save_item(&old).map_err(|e| e.to_string())?;
            store.save_item(&new).map_err(|e| e.to_string())?;
            let committed = items::commit(
                &proj.worktree,
                &format!("chore: supersede {id} by {by}"),
                no_commit,
            )?;
            items::report(
                json!({"superseded": old.meta, "by": new.meta}),
                committed,
                json_out,
                &format!("{id} superseded by {by}"),
            )
        }
    }
}

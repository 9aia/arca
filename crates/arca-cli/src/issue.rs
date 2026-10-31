//! `arca issue new|list|resolve|archive|comment` — one md file
//! per issue under `issues/` (archived files in `issues/archive/`).

use std::path::PathBuf;

use arca_core::{ItemId, ItemKind, Status};
use arca_store::Store;
use clap::Subcommand;
use serde_json::json;

use crate::items;
use crate::templates;

#[derive(Subcommand)]
pub enum Cmd {
    /// File an issue — allocs an I-id, writes
    /// `issues/issue-NNNN-<slug>.md` with frontmatter + template.
    New {
        /// One-line title (drives the filename slug).
        title: String,
        #[command(flatten)]
        body: items::BodyFlags,
        #[command(flatten)]
        meta: items::MetaFlags,
    },
    /// List issues (live files; `--archived` includes
    /// `issues/archive/`).
    List {
        /// Include archived issues.
        #[arg(long)]
        archived: bool,
        #[command(flatten)]
        list: items::ListFlags,
    },
    /// Close an issue — walks the state machine to `done`;
    /// `--resolution` appends text under `## Resolution`.
    Resolve {
        /// Issue id (`I-NNN`).
        id: ItemId,
        /// Resolution text appended to the body.
        #[arg(long)]
        resolution: Option<String>,
    },
    /// Shelve a done issue — moves the file to `issues/archive/`.
    Archive {
        /// Issue id (`I-NNN`).
        id: ItemId,
    },
    /// Append a dated note under `## Comments` (created when
    /// missing).
    Comment {
        /// Issue id (`I-NNN`).
        id: ItemId,
        /// Comment text.
        text: String,
    },
}

/// The `issue` verb group.
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
                kind: ItemKind::Issue,
                title,
                body: body.read()?.as_deref(),
                meta: &meta.to_meta(),
                template: templates::issue,
                word: "issue",
                json_out,
                no_commit,
            },
        ),
        Cmd::List { archived, list } => {
            items::list_files(&store, ItemKind::Issue, archived, &list.filter(), json_out)
        }
        Cmd::Resolve { id, resolution } => {
            items::require_kind(id, ItemKind::Issue, "issue resolve")?;
            let mut file = items::item_for_edit(&store, id)?;
            items::walk_to(&mut file.meta, Status::Done)?;
            if let Some(res) = &resolution {
                templates::append_to_section(&mut file.body, "## Resolution", res);
            }
            let item = file.meta.clone();
            store.save_item(&file).map_err(|e| e.to_string())?;
            let committed =
                items::commit(&proj.worktree, &format!("chore: resolve {id}"), no_commit)?;
            items::report(
                json!({"item": item}),
                committed,
                json_out,
                &format!("resolved {id}"),
            )
        }
        Cmd::Archive { id } => {
            items::require_kind(id, ItemKind::Issue, "issue archive")?;
            let file = items::item_for_edit(&store, id)?;
            if file.meta.status != Status::Done {
                return Err(format!(
                    "{id} must be done before archiving (status: {}) — resolve it first",
                    file.meta.status
                ));
            }
            let rel = items::item_rel_path(&file.meta).replacen('/', "/archive/", 1);
            store.archive_item(id).map_err(|e| e.to_string())?;
            let committed =
                items::commit(&proj.worktree, &format!("chore: archive {id}"), no_commit)?;
            items::report(
                json!({"item": file.meta, "archived_to": rel}),
                committed,
                json_out,
                &format!("archived {id} → {rel}"),
            )
        }
        Cmd::Comment { id, text } => {
            items::require_kind(id, ItemKind::Issue, "issue comment")?;
            let mut file = items::item_for_edit(&store, id)?;
            let line = format!("- {} {}", arca_core::date::render(items::today()), text);
            templates::append_to_section(&mut file.body, "## Comments", &line);
            store.save_item(&file).map_err(|e| e.to_string())?;
            let committed =
                items::commit(&proj.worktree, &format!("chore: comment {id}"), no_commit)?;
            items::report(
                json!({"item": file.meta}),
                committed,
                json_out,
                &format!("commented on {id}"),
            )
        }
    }
}

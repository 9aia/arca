//! `arca proposal new|list|status` — design proposals in
//! `proposals/proposal-NNNN-<slug>.md`.

use std::path::PathBuf;

use arca_core::{ItemId, ItemKind, Status};
use arca_store::Store;
use clap::Subcommand;
use serde_json::json;

use crate::items;
use crate::templates;

#[derive(Subcommand)]
pub enum Cmd {
    /// File a proposal — allocs a P-id, writes the proposal file
    /// with frontmatter + template.
    New {
        /// One-line title (drives the filename slug).
        title: String,
        #[command(flatten)]
        body: items::BodyFlags,
        #[command(flatten)]
        meta: items::MetaFlags,
    },
    /// List proposals.
    List {
        #[command(flatten)]
        list: items::ListFlags,
    },
    /// Set a proposal's status. Proposal words alias the state
    /// machine: `draft` = open, `accepted` = done,
    /// `rejected` = deferred — the transition is walked through the
    /// machine, so `accepted` passes `open → in-progress → done`.
    Status {
        /// Proposal id (`P-NNN`).
        id: ItemId,
        /// `draft` | `accepted` | `rejected` (raw machine statuses
        /// like `in-progress` also accepted).
        status: String,
    },
}

/// Proposal-domain words → machine [`Status`].
fn proposal_status(word: &str) -> Result<Status, String> {
    match word {
        "draft" => Ok(Status::Open),
        "accepted" => Ok(Status::Done),
        "rejected" => Ok(Status::Deferred),
        raw => raw.parse::<Status>().map_err(|_| {
            format!(
                "invalid proposal status {word:?} — expected draft|accepted|rejected \
                 (or a raw status: open|claimed|in-progress|blocked|done|deferred)"
            )
        }),
    }
}

/// The `proposal` verb group.
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
                kind: ItemKind::Proposal,
                title,
                body: body.read()?.as_deref(),
                meta: &meta.to_meta(),
                template: templates::proposal,
                word: "proposal",
                json_out,
                no_commit,
            },
        ),
        Cmd::List { list } => {
            items::list_files(&store, ItemKind::Proposal, false, &list.filter(), json_out)
        }
        Cmd::Status { id, status } => {
            items::require_kind(id, ItemKind::Proposal, "proposal status")?;
            let to = proposal_status(&status)?;
            let mut file = items::item_for_edit(&store, id)?;
            let from = file.meta.status;
            items::walk_to(&mut file.meta, to)?;
            let item = file.meta.clone();
            store.save_item(&file).map_err(|e| e.to_string())?;
            let committed = items::commit(
                &proj.worktree,
                &format!("chore: proposal {id} → {to}"),
                no_commit,
            )?;
            items::report(
                json!({"item": item, "from": from.token(), "to": to.token()}),
                committed,
                json_out,
                &format!("{id}: {from} → {to}"),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposal_status_mapping() {
        assert_eq!(proposal_status("draft"), Ok(Status::Open));
        assert_eq!(proposal_status("accepted"), Ok(Status::Done));
        assert_eq!(proposal_status("rejected"), Ok(Status::Deferred));
        assert_eq!(proposal_status("blocked"), Ok(Status::Blocked));
        assert!(proposal_status("wip").is_err());
    }
}

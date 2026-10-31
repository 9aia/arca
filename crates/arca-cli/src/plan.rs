//! `arca plan new|list` — execution plans in
//! `plans/pl-NNNN-<slug>.md` with a phase checklist.

use std::path::PathBuf;

use arca_core::ItemKind;
use clap::Subcommand;

use crate::items;
use crate::templates;

#[derive(Subcommand)]
pub enum Cmd {
    /// File a plan — allocs a PL-id, writes the plan file with
    /// frontmatter + a phase-checklist template.
    New {
        /// One-line title (drives the filename slug).
        title: String,
        #[command(flatten)]
        body: items::BodyFlags,
        #[command(flatten)]
        meta: items::MetaFlags,
    },
    /// List plans.
    List {
        #[command(flatten)]
        list: items::ListFlags,
    },
}

/// The `plan` verb group.
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
                kind: ItemKind::Plan,
                title,
                body: body.read()?.as_deref(),
                meta: &meta.to_meta(),
                template: templates::plan,
                word: "plan",
                json_out,
                no_commit,
            },
        ),
        Cmd::List { list } => {
            items::list_files(&store, ItemKind::Plan, false, &list.filter(), json_out)
        }
    }
}

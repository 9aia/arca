//! Shared plumbing for the item verbs — the clap flag structs
//! (`--pri/--owner/…`, `--status/--label/--limit`, `--body/--text`)
//! that parse into `arca_ops::items` / `arca-core` types. The ops
//! layer itself is re-exported so call sites read `items::X` for
//! either.

use arca_core::{Item, NewItemMeta};

pub use arca_ops::items::*;

/// `--pri/--owner/--label/--blocks/--epic/--milestone` — the
/// metadata flags shared by every `add`/`new` verb.
#[derive(Debug, Default, clap::Args)]
pub struct MetaFlags {
    /// Priority: low|med|high|crit.
    #[arg(long, visible_alias = "priority")]
    pub pri: Option<arca_core::Priority>,
    /// Owner actor id (`devin`, `human:luis`, …).
    #[arg(long)]
    pub owner: Option<arca_core::Actor>,
    /// `#label` — repeatable.
    #[arg(long)]
    pub label: Vec<String>,
    /// Ids this item blocks — comma-separated or repeatable.
    #[arg(long, value_delimiter = ',')]
    pub blocks: Vec<arca_core::ItemId>,
    /// Epic name — the delivery grouping this item belongs to
    /// (`[A-Za-z0-9_.-]+`).
    #[arg(long)]
    pub epic: Option<String>,
    /// Milestone name — the target this item ships under
    /// (`[A-Za-z0-9_.-]+`).
    #[arg(long)]
    pub milestone: Option<String>,
}

impl MetaFlags {
    /// The parsed form — `NewItemMeta` for `new_file`/`add` paths.
    pub fn to_meta(&self) -> NewItemMeta {
        NewItemMeta {
            priority: self.pri,
            owner: self.owner.clone(),
            labels: self.label.clone(),
            blocks: self.blocks.clone(),
            epic: self.epic.clone(),
            milestone: self.milestone.clone(),
        }
    }
}

/// `--status/--owner/--label` list filters, shared by every `list`.
#[derive(Debug, Default, clap::Args)]
pub struct ListFlags {
    /// Keep only items with this status.
    #[arg(long)]
    pub status: Option<arca_core::Status>,
    /// Keep only items owned by this actor.
    #[arg(long)]
    pub owner: Option<String>,
    /// Keep only items carrying all of these labels (repeatable).
    #[arg(long)]
    pub label: Vec<String>,
    /// Keep only the first N items after filtering.
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
}

impl ListFlags {
    /// The parsed form — `Filter` for `list_files`.
    pub fn filter(&self) -> Filter {
        Filter {
            status: self.status,
            owner: self.owner.clone(),
            labels: self.label.clone(),
            limit: self.limit,
        }
    }

    /// Does `item` pass the filters?
    pub fn matches(&self, item: &Item) -> bool {
        self.filter().matches(item)
    }

    /// `--limit N` as a `.take()` bound.
    pub fn take(&self) -> usize {
        self.limit.unwrap_or(usize::MAX)
    }
}

/// A body-source flag pair — `--body FILE|-` reads a file or stdin,
/// `--text` is inline. Mutually exclusive.
#[derive(Debug, Default, clap::Args)]
pub struct BodyFlags {
    /// Body markdown from a file, or `-` for stdin.
    #[arg(long, conflicts_with = "text", value_name = "FILE|-")]
    pub body: Option<String>,
    /// Body markdown inline.
    #[arg(long)]
    pub text: Option<String>,
}

impl BodyFlags {
    /// The supplied body, or `None` → the caller uses the template.
    pub fn read(&self) -> Result<Option<String>, String> {
        if let Some(t) = &self.text {
            return Ok(Some(t.clone()));
        }
        match self.body.as_deref() {
            None => Ok(None),
            Some("-") => {
                use std::io::Read;
                let mut s = String::new();
                std::io::stdin()
                    .read_to_string(&mut s)
                    .map_err(|e| format!("read --body from stdin: {e}"))?;
                Ok(Some(s))
            }
            Some(path) => std::fs::read_to_string(path)
                .map(Some)
                .map_err(|e| format!("read --body {path}: {e}")),
        }
    }
}

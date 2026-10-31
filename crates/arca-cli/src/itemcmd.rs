//! `arca item show|set` — the kind-agnostic item verbs: locate an
//! item anywhere in the ledger; mutate metadata, with `--status`
//! going through the transition machine only.

use std::path::PathBuf;

use arca_core::{is_valid_label, Actor, Item, ItemId, Priority, Status};
use arca_store::Store;
use clap::Subcommand;
use serde_json::json;

use crate::items;

#[derive(Subcommand)]
pub enum Cmd {
    /// Dump one item — frontmatter fields plus the body for
    /// file-backed kinds (`--json` = the full Item + location +
    /// body).
    Show {
        /// Any item id (`T-NNN`, `I-NNN`, `P-NNN`, `ADR-NNNN`,
        /// `PL-NNN`) — live or archived.
        id: ItemId,
    },
    /// Mutate an item in place. `--status` is the only field routed
    /// through the state machine — single steps only, invalid ones
    /// error with the valid targets. Status changes on tasks move
    /// the line between TODO.md and BACKLOG.md (`done` deletes it).
    Set {
        /// Any item id.
        id: ItemId,
        /// New status — one legal step from the current one.
        #[arg(long)]
        status: Option<Status>,
        /// Priority: low|med|high|crit.
        #[arg(long, visible_alias = "priority")]
        pri: Option<Priority>,
        /// Owner actor id.
        #[arg(long)]
        owner: Option<Actor>,
        /// Add (`+x` or `x`) or remove (`-x`) a label — repeatable.
        #[arg(long, allow_hyphen_values = true)]
        label: Vec<String>,
        /// Remove a label — repeatable.
        #[arg(long)]
        unlabel: Vec<String>,
        /// Add (`+T-1` or `T-1`) or remove (`-T-1`) a blocks dep —
        /// repeatable.
        #[arg(long, allow_hyphen_values = true)]
        blocks: Vec<String>,
        /// Remove a blocks dep — repeatable.
        #[arg(long)]
        unblock: Vec<ItemId>,
        /// Epic name — `""` clears the epic.
        #[arg(long)]
        epic: Option<String>,
        /// Milestone name — `""` clears the milestone.
        #[arg(long)]
        milestone: Option<String>,
    },
}

/// The set-verb flag bundle — applied to an `Item` after the
/// status transition check.
#[derive(Default)]
pub(crate) struct SetFlags {
    pub(crate) status: Option<Status>,
    pub(crate) pri: Option<Priority>,
    pub(crate) owner: Option<Actor>,
    pub(crate) label: Vec<String>,
    pub(crate) unlabel: Vec<String>,
    pub(crate) blocks: Vec<String>,
    pub(crate) unblock: Vec<ItemId>,
    pub(crate) epic: Option<String>,
    pub(crate) milestone: Option<String>,
}

impl SetFlags {
    /// A status-only mutation — `arca interrupt` drives a legal
    /// step through the same relocation path as `item set`.
    pub(crate) fn status(to: Status) -> Self {
        SetFlags {
            status: Some(to),
            ..Default::default()
        }
    }

    pub(crate) fn is_noop(&self) -> bool {
        self.status.is_none()
            && self.pri.is_none()
            && self.owner.is_none()
            && self.label.is_empty()
            && self.unlabel.is_empty()
            && self.blocks.is_empty()
            && self.unblock.is_empty()
            && self.epic.is_none()
            && self.milestone.is_none()
    }
}

/// `+x` → add `x`, `-x` → remove `x`, `x` → add — the `item set`
/// add/remove convention.
fn signed(s: &str) -> (bool, &str) {
    match s.strip_prefix('-') {
        Some(rest) => (false, rest),
        None => (true, s.strip_prefix('+').unwrap_or(s)),
    }
}

/// Apply every set flag to `item` — status first (machine-checked),
/// then the plain metadata.
fn apply_set(item: &mut Item, flags: &SetFlags) -> Result<(), String> {
    if let Some(to) = flags.status {
        let from = item.status;
        item.status = from.transition_to(to).map_err(|_| {
            format!(
                "{id}: invalid status transition {from} → {to} (valid from {from}: {t})",
                id = item.id,
                t = items::targets(from)
            )
        })?;
    }
    if let Some(p) = flags.pri {
        item.priority = p;
    }
    if let Some(o) = &flags.owner {
        item.owner = Some(o.clone());
    }
    for raw in &flags.label {
        let (add, name) = signed(raw);
        if !is_valid_label(name) {
            return Err(format!("invalid label {name:?}"));
        }
        if add {
            item.labels.insert(name.to_string());
        } else {
            item.labels.remove(name);
        }
    }
    for name in &flags.unlabel {
        item.labels.remove(name.as_str());
    }
    for raw in &flags.blocks {
        let (add, s) = signed(raw);
        let id: ItemId = s
            .parse()
            .map_err(|e: arca_core::ParseItemIdError| e.to_string())?;
        if add {
            item.blocks.insert(id);
        } else {
            item.blocks.remove(&id);
        }
    }
    for id in &flags.unblock {
        item.blocks.remove(id);
    }
    for (what, flag, slot) in [
        ("epic", &flags.epic, &mut item.epic),
        ("milestone", &flags.milestone, &mut item.milestone),
    ] {
        if let Some(name) = flag {
            if name.is_empty() {
                *slot = None; // `--epic ""` clears
            } else if !arca_core::is_valid_group_name(name) {
                return Err(format!("invalid {what} name {name:?}"));
            } else {
                *slot = Some(name.clone());
            }
        }
    }
    Ok(())
}

/// The `item` verb group.
pub fn run(
    cmd: Cmd,
    dir: Option<PathBuf>,
    root: Option<PathBuf>,
    json_out: bool,
    no_commit: bool,
) -> Result<i32, String> {
    let (proj, store) = items::open(dir.as_deref(), root.as_deref())?;
    match cmd {
        Cmd::Show { id } => match items::find_item(&store, id)? {
            Some((item, location, body)) => {
                let handoffs = store.read_handoffs(id).map_err(|e| e.to_string())?.len();
                let interrupted = crate::continuity::is_interrupted(&store, id);
                if json_out {
                    crate::ops::print_json(
                        &json!({"item": item, "location": location, "handoffs": handoffs, "interrupted": interrupted, "body": body}),
                    );
                } else {
                    println!("{}", items::compact(&item));
                    println!("  {location}");
                    println!("  handoffs: {handoffs}");
                    if interrupted {
                        println!("  ⚡ interrupted — latest handoff marks it; `arca resume {id}`");
                    }
                    if let Some(body) = &body {
                        if !body.is_empty() {
                            println!();
                            println!("{body}");
                        }
                    }
                }
                Ok(0)
            }
            None => Err(format!("{id}: no such item")),
        },
        Cmd::Set {
            id,
            status,
            pri,
            owner,
            label,
            unlabel,
            blocks,
            unblock,
            epic,
            milestone,
        } => {
            let flags = SetFlags {
                status,
                pri,
                owner,
                label,
                unlabel,
                blocks,
                unblock,
                epic,
                milestone,
            };
            if flags.is_noop() {
                return Err("item set: nothing to change — pass at least one flag".into());
            }
            let (item, location) = mutate_item(&store, id, &flags)?;
            let committed = items::commit(&proj.worktree, &format!("chore: set {id}"), no_commit)?;
            let human = item
                .as_ref()
                .map(|i| format!("set {}", items::compact(i)))
                .unwrap_or_else(|| format!("set {id} done — line removed"));
            items::report(
                json!({"item": item, "location": location}),
                committed,
                json_out,
                &human,
            )
        }
    }
}

/// The `item show` payload — `{item, location, handoffs,
/// interrupted, body}` — the `arca serve` method surface.
pub(crate) fn show_json(
    store: &arca_store_md::MdStore,
    id: ItemId,
) -> Result<serde_json::Value, String> {
    match items::find_item(store, id)? {
        Some((item, location, body)) => {
            let handoffs = store.read_handoffs(id).map_err(|e| e.to_string())?.len();
            let interrupted = crate::continuity::is_interrupted(store, id);
            Ok(
                json!({"item": item, "location": location, "handoffs": handoffs, "interrupted": interrupted, "body": body}),
            )
        }
        None => Err(format!("{id}: no such item")),
    }
}

/// Apply `flags` to one item anywhere in the ledger and save —
/// the shared mutation behind `item set` and `interrupt`. Writes
/// but does NOT commit; the caller owns the commit message.
/// Returns the post-mutation item (`None` when done removed a task
/// line) and a location label for reporting.
pub(crate) fn mutate_item(
    store: &arca_store_md::MdStore,
    id: ItemId,
    flags: &SetFlags,
) -> Result<(Option<Item>, String), String> {
    let mut ledger = items::load_ledger(store)?;
    if let Some(loc) = ledger.locate(id) {
        // task item: mutate in place; a status change may
        // relocate the line — done deletes the subtree,
        // deferred detaches the node into BACKLOG.md
        // (children park with it), an active status moves
        // backlog nodes into TODO.md (children unpark).
        // Nested children that keep their doc stay nested.
        let status_change = flags.status.is_some();
        apply_set(ledger.item_mut(&loc), flags)?;
        if status_change {
            match ledger.item(&loc).status {
                Status::Done => {
                    ledger.take(&loc);
                }
                Status::Deferred => {
                    let mut item = ledger.take(&loc);
                    items::walk_descendants_to(&mut item, Status::Deferred)?;
                    ledger.put_backlog(item, None);
                }
                _ if loc.backlog => {
                    let mut item = ledger.take(&loc);
                    items::walk_descendants_to(&mut item, Status::Open)?;
                    ledger.put_todo(item, None);
                }
                _ if !loc.is_child() => {
                    // top-level items re-land at the
                    // section tail — same as before
                    let item = ledger.take(&loc);
                    ledger.put_todo(item, None);
                }
                _ => {} // a TODO child stays nested
            }
        }
        let item = ledger.locate(id).map(|l| ledger.item(&l).clone());
        items::save_ledger(store, &ledger)?;
        Ok((item, "tasks".to_string()))
    } else {
        let mut file = items::item_for_edit(store, id)?;
        apply_set(&mut file.meta, flags)?;
        let item = file.meta.clone();
        store.save_item(&file).map_err(|e| e.to_string())?;
        let location = items::item_rel_path(&item);
        Ok((Some(item), location))
    }
}

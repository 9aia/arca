//! `arca handoff` / `arca resume` / `arca interrupt` — the
//! continuity trio. At session end `handoff` appends a
//! `## <today> · <actor>` entry plus note to `handoffs/<ID>.md`
//! (append-only, one file per item, uniform across kinds). At
//! session start `resume` emits the minimal restart context — the
//! item, its `blocks:` dep titles, the latest handoff entries,
//! the open checklist — sized for a prompt, the single-item
//! counterpart of `status --prompt`. `interrupt` is the abnormal
//! end: a work-state item (claimed/in-progress/blocked) reverts
//! to `open` and gets an `interrupted` handoff in one commit.
//!
//! sepia owns session lifecycle; it calls these verbs from hooks.
//! `--session` is provenance only: a sepia session id,
//! `cli:<pid>`, whatever — arca records it, never interprets it.

use std::collections::BTreeSet;
use std::path::PathBuf;

use arca_core::{is_valid_session, Actor, HandoffEntry, ItemId, Status};
use arca_store::Store;
use serde_json::json;

use crate::{items, ops};

/// `--actor` fallback — the local CLI wrote it.
const DEFAULT_ACTOR: &str = "cli";

/// Handoff entries shown by default (`--last` overrides).
const DEFAULT_LAST: usize = 10;

/// The note prefix `interrupt` writes — also the derivation rule:
/// an item "was interrupted" when its latest handoff note starts
/// with this word. No dedicated status — the event lives in the
/// append-only log; status/board/report surfaces derive the flag.
pub const INTERRUPT_NOTE: &str = "interrupted";

/// Did the item's latest handoff mark it interrupted?
pub fn is_interrupted(store: &impl Store, id: ItemId) -> bool {
    store
        .read_handoffs(id)
        .ok()
        .and_then(|entries| entries.last().map(|e| e.note.clone()))
        .is_some_and(|n| n.trim_start().starts_with(INTERRUPT_NOTE))
}

/// The interrupted subset of `items` — for status/board markers.
pub fn interrupted_set<'a>(
    store: &impl Store,
    items: impl Iterator<Item = &'a arca_core::Item>,
) -> BTreeSet<ItemId> {
    items
        .filter(|i| is_interrupted(store, i.id))
        .map(|i| i.id)
        .collect()
}

/// `--note` inline, or `-` for stdin.
fn read_note(note: &str) -> Result<String, String> {
    let note = if note == "-" {
        use std::io::Read;
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| format!("read --note from stdin: {e}"))?;
        s
    } else {
        note.to_string()
    };
    let note = note.trim().to_string();
    if note.is_empty() {
        return Err("handoff: --note is empty".into());
    }
    Ok(note)
}

/// The handoff write — the item must exist somewhere in the
/// ledger (any kind, live or archived). Validates the note and
/// the session id, appends the entry, and returns it plus the
/// worktree-relative path (the file lands wherever it already
/// lived). Caller commits.
pub fn record_handoff(
    store: &arca_store_md::MdStore,
    worktree: &std::path::Path,
    id: ItemId,
    note: String,
    actor: Option<Actor>,
    session: Option<String>,
) -> Result<(HandoffEntry, String), String> {
    if items::find_item(store, id)?.is_none() {
        return Err(format!("{id}: no such item — handoffs attach to items"));
    }
    let note = note.trim().to_string();
    if note.is_empty() {
        return Err("handoff: note is empty".into());
    }
    if let Some(s) = &session {
        if !is_valid_session(s) {
            return Err(format!(
                "invalid --session {s:?} — no empty, no `·`, no newlines"
            ));
        }
    }
    let entry = HandoffEntry {
        date: items::today(),
        actor: match actor {
            Some(a) => a,
            None => Actor::new(DEFAULT_ACTOR).map_err(|e| e.to_string())?,
        },
        session,
        note,
    };
    store
        .append_handoff(id, &entry)
        .map_err(|e| e.to_string())?;
    // the file lands wherever it already lived
    let live = format!("handoffs/{id}.md");
    let rel = if worktree.join(&live).exists() {
        live
    } else {
        format!("handoffs/archive/{id}.md")
    };
    Ok((entry, rel))
}

/// The `handoff` verb — `-` reads the note from stdin.
#[allow(clippy::too_many_arguments)]
pub fn handoff(
    id: ItemId,
    note: &str,
    actor: Option<Actor>,
    session: Option<String>,
    dir: Option<PathBuf>,
    root: Option<PathBuf>,
    json_out: bool,
    no_commit: bool,
) -> Result<i32, String> {
    let (proj, store) = items::open(dir.as_deref(), root.as_deref())?;
    let (entry, rel) =
        record_handoff(&store, &proj.worktree, id, read_note(note)?, actor, session)?;
    let committed = items::commit(&proj.worktree, &format!("chore: handoff {id}"), no_commit)?;
    items::report(
        json!({"id": id.to_string(), "path": rel, "entry": entry}),
        committed,
        json_out,
        &format!("handoff {id} → {rel}"),
    )
}

/// The interrupt write — a session ended with work still
/// claimed/in-progress/blocked. Reverts the item to `open`
/// through the shared `item set` mutation path (relocation rules
/// included) and appends the `interrupted` marker entry. Caller
/// commits — one commit covers both writes.
pub fn interrupt_item(
    store: &arca_store_md::MdStore,
    id: ItemId,
    note: Option<&str>,
    actor: Option<Actor>,
    session: Option<String>,
) -> Result<HandoffEntry, String> {
    let Some((item, _, _)) = items::find_item(store, id)? else {
        return Err(format!("{id}: no such item"));
    };
    match item.status {
        Status::Claimed | Status::InProgress | Status::Blocked => {}
        s => {
            return Err(format!(
                "{id}: status is {s} — nothing to interrupt \
                 (only claimed/in-progress/blocked revert to open)"
            ));
        }
    }
    if let Some(s) = &session {
        if !is_valid_session(s) {
            return Err(format!(
                "invalid --session {s:?} — no empty, no `·`, no newlines"
            ));
        }
    }
    // revert through the shared mutation path (relocation rules
    // included), then the marker handoff — one commit covers both
    crate::itemcmd::mutate_item(store, id, &crate::itemcmd::SetFlags::status(Status::Open))?;
    let note = match note.map(str::trim).filter(|n| !n.is_empty()) {
        Some(extra) => format!("{INTERRUPT_NOTE} — {extra}"),
        None => INTERRUPT_NOTE.to_string(),
    };
    let entry = HandoffEntry {
        date: items::today(),
        actor: match actor {
            Some(a) => a,
            None => Actor::new(DEFAULT_ACTOR).map_err(|e| e.to_string())?,
        },
        session,
        note,
    };
    store
        .append_handoff(id, &entry)
        .map_err(|e| e.to_string())?;
    Ok(entry)
}

/// The `interrupt` verb — called by whoever owns session
/// lifecycle (sepia hooks, CLI session-end hooks); arca never
/// detects sessions itself.
#[allow(clippy::too_many_arguments)]
pub fn interrupt(
    id: ItemId,
    note: Option<&str>,
    actor: Option<Actor>,
    session: Option<String>,
    dir: Option<PathBuf>,
    root: Option<PathBuf>,
    json_out: bool,
    no_commit: bool,
) -> Result<i32, String> {
    let (proj, store) = items::open(dir.as_deref(), root.as_deref())?;
    let entry = interrupt_item(&store, id, note, actor, session)?;
    let committed = items::commit(&proj.worktree, &format!("chore: interrupt {id}"), no_commit)?;
    items::report(
        json!({"id": id.to_string(), "status": "open", "entry": entry}),
        committed,
        json_out,
        &format!("interrupted {id} → open"),
    )
}

/// Unchecked `- [ ]` checkbox items in a markdown body — the open
/// checklist `resume` surfaces (issue bodies, plan phases).
fn open_checklist(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|l| {
            let t = l.trim_start();
            for m in ["- [ ]", "* [ ]", "+ [ ]"] {
                if let Some(rest) = t.strip_prefix(m) {
                    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
                        return Some(rest.trim().to_string());
                    }
                }
            }
            None
        })
        .collect()
}

/// The gathered `resume` context — the typed form behind
/// [`resume_json`] and the human block.
struct ResumeData {
    item: arca_core::Item,
    location: String,
    blocks_titles: Vec<serde_json::Value>,
    shown: Vec<HandoffEntry>,
    total: usize,
    checklist: Vec<String>,
}

/// Gather the resume context — the item, `blocks:` dep titles,
/// the latest handoff entries, the open checklist.
fn resume_data(
    store: &arca_store_md::MdStore,
    id: ItemId,
    last: Option<usize>,
) -> Result<ResumeData, String> {
    let Some((item, location, body)) = items::find_item(store, id)? else {
        return Err(format!("{id}: no such item"));
    };
    let mut blocks_titles = Vec::new();
    for b in &item.blocks {
        let title = items::find_item(store, *b)?.map(|(i, _, _)| i.title);
        blocks_titles.push(json!({"id": b.to_string(), "title": title}));
    }
    let entries = store.read_handoffs(id).map_err(|e| e.to_string())?;
    let total = entries.len();
    let n = last.unwrap_or(DEFAULT_LAST);
    let shown: Vec<HandoffEntry> = entries
        .iter()
        .rev()
        .take(n)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let checklist = body.as_deref().map(open_checklist).unwrap_or_default();
    Ok(ResumeData {
        item,
        location,
        blocks_titles,
        shown,
        total,
        checklist,
    })
}

/// The resume payload — `{item, location, blocks_titles,
/// handoffs[], open_checklist}` — shared by `--json` and
/// `arca serve`.
pub fn resume_json(
    store: &arca_store_md::MdStore,
    id: ItemId,
    last: Option<usize>,
) -> Result<serde_json::Value, String> {
    let d = resume_data(store, id, last)?;
    Ok(json!({
        "item": d.item,
        "location": d.location,
        "blocks_titles": d.blocks_titles,
        "handoffs": d.shown,
        "handoffs_total": d.total,
        "open_checklist": d.checklist,
    }))
}

/// The `resume` verb — `--json` is
/// `{item, location, blocks_titles, handoffs[], open_checklist}`;
/// human output is a compact prompt-ready block.
pub fn resume(
    id: ItemId,
    last: Option<usize>,
    dir: Option<PathBuf>,
    root: Option<PathBuf>,
    json_out: bool,
) -> Result<i32, String> {
    let (_proj, store) = items::open(dir.as_deref(), root.as_deref())?;
    let d = resume_data(&store, id, last)?;
    if json_out {
        ops::print_json(&json!({
            "item": d.item,
            "location": d.location,
            "blocks_titles": d.blocks_titles,
            "handoffs": d.shown,
            "handoffs_total": d.total,
            "open_checklist": d.checklist,
        }));
        return Ok(0);
    }
    let ResumeData {
        item,
        location,
        blocks_titles,
        shown,
        total,
        checklist,
    } = d;
    // prompt-ready block — compact item line first; sections only
    // when they carry context
    println!("{}", items::compact(&item));
    println!("in: {location}");
    if !blocks_titles.is_empty() {
        println!("blocks:");
        for b in &blocks_titles {
            println!(
                "  {} {}",
                b["id"].as_str().unwrap_or_default(),
                b["title"].as_str().unwrap_or("(missing)")
            );
        }
    }
    if !shown.is_empty() {
        println!("handoffs:");
        for e in &shown {
            let mut head = format!("{} · {}", e.date, e.actor);
            if let Some(s) = &e.session {
                head.push_str(&format!(" · session:{s}"));
            }
            println!("{head}");
            for line in e.note.lines() {
                println!("  {line}");
            }
            println!();
        }
        if total > shown.len() {
            println!("… +{} earlier entries", total - shown.len());
        }
    }
    if !checklist.is_empty() {
        println!("open checklist:");
        for c in &checklist {
            println!("  - [ ] {c}");
        }
    }
    Ok(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn checklist_extraction() {
        let body = "## Checklist\n\n- [ ] do a\n  - [x] done\n* [ ] star\n+ [ ] plus\n- [ ]not-a-box\n- plain\n\n- [ ]\n";
        assert_eq!(open_checklist(body), vec!["do a", "star", "plus", ""]);
        assert!(open_checklist("no items\n").is_empty());
    }
}

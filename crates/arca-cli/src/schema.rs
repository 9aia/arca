//! `arca schema` — the file/state spec as machine-readable JSON.
//!
//! The document is versioned (`version: 1`) and has three parts:
//! schemars-generated JSON Schemas for the domain types, a
//! data-shaped description of the `project/` file spec (paths,
//! filename patterns, frontmatter keys, the item-line grammar),
//! and the status transition table — everything an agent needs to
//! read the ledger without the skill.

use arca_core::{
    BacklogDoc, HandoffEntry, Item, ItemFile, ItemKind, NextCounters, Priority, ProjectMeta,
    Section, Status, TodoDoc,
};
use serde_json::json;

use crate::ops;

/// `schemars::schema_for!` → owned JSON value.
fn schema<T: schemars::JsonSchema>() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(T))
        .unwrap_or_else(|e| json!({"error": e.to_string()}))
}

/// The whole schema document (also unit-tested for shape).
pub fn document() -> serde_json::Value {
    let transitions: serde_json::Map<String, serde_json::Value> = Status::ALL
        .into_iter()
        .map(|s| {
            (
                s.token().to_string(),
                json!(s
                    .transitions()
                    .iter()
                    .map(|t| t.token())
                    .collect::<Vec<_>>()),
            )
        })
        .collect();
    let kind_ids: serde_json::Map<String, serde_json::Value> = ItemKind::ALL
        .into_iter()
        .map(|k| {
            (
                k.prefix().to_string(),
                json!({"kind": format!("{k:?}"), "width": k.width(), "counter": k.counter_key()}),
            )
        })
        .collect();
    json!({
        "version": 1,
        "types": {
            "Item": schema::<Item>(),
            "ItemFile": schema::<ItemFile>(),
            "TodoDoc": schema::<TodoDoc>(),
            "BacklogDoc": schema::<BacklogDoc>(),
            "Section": schema::<Section>(),
            "ProjectMeta": schema::<ProjectMeta>(),
            "NextCounters": schema::<NextCounters>(),
            "Status": schema::<Status>(),
            "Priority": schema::<Priority>(),
            "ItemKind": schema::<ItemKind>(),
            "ItemId": schema::<arca_core::ItemId>(),
            "HandoffEntry": schema::<HandoffEntry>(),
        },
        "statuses": Status::ALL.iter().map(|s| s.token()).collect::<Vec<_>>(),
        "priorities": Priority::ALL.iter().map(|p| p.token()).collect::<Vec<_>>(),
        "transitions": transitions,
        "ids": {
            "shape": "<prefix>-<number>, zero-padded per kind",
            "kinds": kind_ids,
            "file_backed": ItemKind::ALL.iter()
                .filter(|k| arca_store_md::item_dir(**k).is_some())
                .map(|k| k.prefix())
                .collect::<Vec<_>>(),
        },
        "files": {
            "meta.toml": {
                "format": "toml",
                "schema": "ProjectMeta",
                "keys": {
                    "name": "project/repo name",
                    "next": "per-kind next-id counters (t, i, p, adr, pl)",
                },
            },
            "TODO.md": {
                "format": "markdown",
                "schema": "TodoDoc",
                "title": "# TODO — <repo>",
                "section": "## <name>",
                "line": "- [ ] T-NNN <title>[ · <token>]…",
                "checkbox": "[ ] open — done items are removed, git history keeps them",
                "children": "a `- [ ]` list indented under an item nests as its sub-items (canonical: 2 spaces/level)",
                "default_status": "open",
            },
            "BACKLOG.md": {
                "format": "markdown",
                "schema": "BacklogDoc",
                "line": "- T-NNN <title>[ · <token>]…",
                "children": "a `-` list indented under an item nests as its sub-items (canonical: 2 spaces/level)",
                "default_status": "deferred",
            },
            "index.md": {"format": "markdown", "verbatim": true},
            "kinds": {
                "issues":     {"dir": "issues/",     "pattern": "issue-NNNN-<slug>.md",    "archive": "issues/archive/"},
                "proposals":  {"dir": "proposals/",  "pattern": "proposal-NNNN-<slug>.md", "archive": "proposals/archive/"},
                "adrs":       {"dir": "adrs/",       "pattern": "NNNN-<slug>.md",          "archive": "adrs/archive/"},
                "plans":      {"dir": "plans/",      "pattern": "pl-NNNN-<slug>.md",       "archive": "plans/archive/"},
            },
            "item_file": {
                "format": "`+++` TOML frontmatter + free-form markdown body",
                "frontmatter_keys": [
                    "id", "title", "status", "created", "owner", "priority",
                    "labels", "blocks", "note", "superseded_by", "supersedes",
                    "epic", "milestone",
                ],
            },
            "handoffs": {
                "dir": "handoffs/",
                "file": "<ID>.md — one append-only file per item, uniform across kinds",
                "pattern": "<PREFIX>-NNN.md (e.g. T-001.md, ADR-0004.md)",
                "archive": "handoffs/archive/ — files whose item was removed (`check --fix` moves them here)",
                "frontmatter_keys": ["item"],
                "title": "# Handoffs — <ID>",
                "entry": "## <YYYY-MM-DD> · <actor>[ · session:<id>] + freeform note body",
                "append_only": true,
            },
        },
        "line_grammar": {
            "shape": "<id> <title> [· status:<s>] [· pri:<p>] [· owner:<a>] \
                      [· created:<YYYY-MM-DD>] [· #<label>]… [· blocks:<id>[,<id>]…] \
                      [· epic:<name>] [· ms:<name>] [· note:<text>]",
            "token_order": ["status", "pri", "owner", "created", "#label", "blocks", "epic", "ms", "note"],
            "separator": "·",
            "defaults": {"status": "doc default (open|deferred)", "pri": "med"},
            "label_pattern": "[A-Za-z0-9_-]+",
            "group_pattern": "[A-Za-z0-9_.-]+ (epic/ms names — `ms` is the milestone token)",
            "actor_pattern": "[A-Za-z0-9._:-]+ (no leading/trailing ':', no '::')",
            "nesting": "an item's children are a `-`/`[ ]` list indented under it \
                       (canonical 2 spaces per level; one level recommended). Children \
                       carry the same token grammar — `done`/`defer` on a parent act \
                       on the whole subtree, on a child just that node",
        },
    })
}

/// The `schema` verb — JSON on stdout (`--json` or not; there is
/// no useful human rendering of a spec document).
pub fn run() -> Result<i32, String> {
    ops::print_json(&document());
    Ok(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn document_shape_is_stable() {
        let d = document();
        assert_eq!(d["version"], 1);
        for t in ["Item", "ItemFile", "Status", "Priority", "ItemId"] {
            assert!(d["types"][t].is_object(), "types.{t}");
        }
        // transitions mirror the state machine exactly
        assert_eq!(
            d["transitions"]["in-progress"],
            json!(["blocked", "done", "deferred", "open"])
        );
        assert_eq!(d["transitions"]["done"], json!([]));
        assert_eq!(
            d["statuses"],
            json!([
                "open",
                "claimed",
                "in-progress",
                "blocked",
                "done",
                "deferred"
            ])
        );
        // file spec data
        assert_eq!(
            d["files"]["kinds"]["issues"]["pattern"],
            "issue-NNNN-<slug>.md"
        );
        assert_eq!(d["files"]["kinds"]["adrs"]["pattern"], "NNNN-<slug>.md");
        assert!(d["files"]["item_file"]["frontmatter_keys"]
            .as_array()
            .unwrap()
            .contains(&json!("superseded_by")));
        assert!(d["files"]["item_file"]["frontmatter_keys"]
            .as_array()
            .unwrap()
            .contains(&json!("epic")));
        assert!(d["line_grammar"]["token_order"]
            .as_array()
            .unwrap()
            .ends_with(&[json!("epic"), json!("ms"), json!("note")]));
        assert!(d["line_grammar"]["shape"].as_str().unwrap().contains('·'));
        assert_eq!(d["ids"]["kinds"]["ADR"]["width"], 4);
    }
}

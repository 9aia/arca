//! `arca status [--prompt]` — the ledger digest; THE agent-context
//! verb. Human output is compact counts + the in-progress and
//! blocked lists; `--prompt` is a token-sized block (repo, counts
//! one-liner, then items grouped in-progress → blocked → claimed →
//! open-by-priority, one per line, no prose); `--json` is the
//! structured digest.

use std::collections::BTreeSet;
use std::path::PathBuf;

use arca_core::{Item, ItemId, Status};
use serde_json::json;

use crate::{digest, items, ops};

/// Hard cap on `--prompt` lines — the block must stay token-cheap.
pub const PROMPT_MAX_LINES: usize = 60;

/// `<id> <pri> <title> [⚡]` — one prompt line, no prose; `⚡`
/// marks an interrupted item (latest handoff is an interrupt).
fn prompt_item(i: &Item, interrupted: bool) -> String {
    format!(
        "{} {} {}{}",
        i.id,
        i.priority.token(),
        i.title,
        if interrupted { " ⚡" } else { "" }
    )
}

/// The `--prompt` block, capped at [`PROMPT_MAX_LINES`] — the open
/// group (always last, always largest) is truncated with a
/// `… +N more` marker when it would overflow.
pub fn prompt_block(scan: &digest::Scan, interrupted: &BTreeSet<ItemId>) -> String {
    let c = digest::counts(scan);
    let mut out = vec![format!("repo:{} {}", scan.name, c.oneline())];
    let section = |name: &str, items: &[&Item], room: usize| -> Vec<String> {
        if items.is_empty() {
            return Vec::new();
        }
        let mut v = vec![format!("{name}:")];
        v.extend(
            items
                .iter()
                .map(|i| prompt_item(i, interrupted.contains(&i.id)))
                .take(room),
        );
        v
    };
    // sections in spec order; the open tail absorbs the budget
    let fixed: Vec<(&str, Vec<&Item>)> = vec![
        ("in-progress", scan.in_status(Status::InProgress)),
        ("blocked", scan.in_status(Status::Blocked)),
        ("claimed", scan.in_status(Status::Claimed)),
    ];
    let mut open: Vec<&Item> = scan.in_status(Status::Open);
    open.sort_by(|a, b| digest::pri_then_id(a, b));

    let mut fixed_lines = Vec::new();
    for (name, list) in &fixed {
        fixed_lines.extend(section(name, list, usize::MAX));
    }
    // +2 = "open:" header + possible "… +N more" marker
    let room = PROMPT_MAX_LINES.saturating_sub(out.len() + fixed_lines.len() + 2);
    out.extend(fixed_lines);
    if !open.is_empty() {
        out.extend(section("open", &open, room));
        if open.len() > room {
            out.push(format!("… +{} more open", open.len() - room));
        }
    }
    debug_assert!(out.len() <= PROMPT_MAX_LINES);
    out.join("\n")
}

/// The `--json` digest — counts + the active groups as full items.
pub fn digest_json(scan: &digest::Scan) -> serde_json::Value {
    let c = digest::counts(scan);
    let group = |s: Status| {
        scan.in_status(s)
            .iter()
            .map(|i| serde_json::to_value(i).unwrap_or(json!(null)))
            .collect::<Vec<_>>()
    };
    json!({
        "repo": scan.name,
        "counts": c.to_json(),
        "in-progress": group(Status::InProgress),
        "blocked": group(Status::Blocked),
        "claimed": group(Status::Claimed),
        "open": group(Status::Open),
    })
}

/// The status digest + `interrupted` id list — the `--json`
/// payload, shared with `arca serve`.
pub fn status_json(store: &arca_store_md::MdStore) -> Result<serde_json::Value, String> {
    let scan = digest::scan(store)?;
    let interrupted = crate::continuity::interrupted_set(store, scan.live());
    let mut v = digest_json(&scan);
    v["interrupted"] = json!(interrupted
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>());
    Ok(v)
}

/// The `--prompt` block payload — `{"prompt": "<block>"}` for
/// `arca serve`, where the digest isn't a useful shape.
pub fn prompt_json(store: &arca_store_md::MdStore) -> Result<serde_json::Value, String> {
    let scan = digest::scan(store)?;
    let interrupted = crate::continuity::interrupted_set(store, scan.live());
    Ok(json!({"prompt": prompt_block(&scan, &interrupted)}))
}

/// The `status` verb.
pub fn run(
    dir: Option<PathBuf>,
    root: Option<PathBuf>,
    prompt: bool,
    json_out: bool,
) -> Result<i32, String> {
    let (_proj, store) = items::open(dir.as_deref(), root.as_deref())?;
    let scan = digest::scan(&store)?;
    let interrupted = crate::continuity::interrupted_set(&store, scan.live());
    if prompt {
        println!("{}", prompt_block(&scan, &interrupted));
        return Ok(0);
    }
    if json_out {
        let mut v = digest_json(&scan);
        v["interrupted"] = json!(interrupted
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>());
        ops::print_json(&v);
        return Ok(0);
    }
    // human: counts + the two groups an agent asks about first
    let c = digest::counts(&scan);
    println!("{} — project ledger", scan.name);
    println!("{}", c.oneline());
    for (name, status) in [
        ("in progress", Status::InProgress),
        ("blocked", Status::Blocked),
    ] {
        let list = scan.in_status(status);
        if !list.is_empty() {
            println!("{name}:");
            for i in list {
                println!(
                    "  {}{}",
                    items::compact(i),
                    if interrupted.contains(&i.id) {
                        " ⚡"
                    } else {
                        ""
                    }
                );
            }
        }
    }
    Ok(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use arca_core::{ItemId, ItemKind, Priority};
    use std::collections::BTreeSet;

    fn scan_with(items: Vec<Item>) -> digest::Scan {
        digest::Scan {
            name: "arca".into(),
            items,
            archived: BTreeSet::new(),
        }
    }

    #[test]
    fn prompt_block_groups_and_caps() {
        let mut v = vec![
            Item::new(ItemId::new(ItemKind::Task, 2), "work").status(Status::InProgress),
            Item::new(ItemId::new(ItemKind::Issue, 1), "stuck").status(Status::Blocked),
            Item::new(ItemId::new(ItemKind::Task, 5), "owned").status(Status::Claimed),
            Item::new(ItemId::new(ItemKind::Task, 1), "hi").priority(Priority::Critical),
            Item::new(ItemId::new(ItemKind::Task, 3), "mid"),
            Item::new(ItemId::new(ItemKind::Task, 4), "done").status(Status::Done),
            Item::new(ItemId::new(ItemKind::Task, 6), "parked").status(Status::Deferred),
        ];
        let block = prompt_block(&scan_with(v.clone()), &BTreeSet::new());
        let lines: Vec<&str> = block.lines().collect();
        assert_eq!(
            lines[0],
            "repo:arca tasks 2o/1c/1ip/0b backlog 1 issues 1o p 0o pl 0o adr 0o"
        );
        let whole = lines.join("\n");
        let ip = lines.iter().position(|l| *l == "in-progress:").unwrap();
        let bl = lines.iter().position(|l| *l == "blocked:").unwrap();
        let cl = lines.iter().position(|l| *l == "claimed:").unwrap();
        let op = lines.iter().position(|l| *l == "open:").unwrap();
        assert!(ip < bl && bl < cl && cl < op);
        // open group is crit-first
        assert!(whole.contains("T-001 crit hi"));
        assert!(lines.len() <= PROMPT_MAX_LINES);
        // done/deferred never appear
        assert!(!whole.contains("done") && !whole.contains("parked"));

        // overflow: 70 open items → capped at 60 lines with a marker
        v.clear();
        for n in 1..=70 {
            v.push(Item::new(
                ItemId::new(ItemKind::Task, n),
                format!("task {n}"),
            ));
        }
        let block = prompt_block(&scan_with(v), &BTreeSet::new());
        let lines: Vec<&str> = block.lines().collect();
        assert!(lines.len() <= PROMPT_MAX_LINES, "{}", lines.len());
        assert!(block.contains("… +"), "{}", block);
    }

    #[test]
    fn prompt_block_no_open_items() {
        // in-progress/blocked but nothing open → no "open:" section
        let v = vec![
            Item::new(ItemId::new(ItemKind::Task, 1), "wip").status(Status::InProgress),
            Item::new(ItemId::new(ItemKind::Task, 2), "stuck").status(Status::Blocked),
        ];
        let block = prompt_block(&scan_with(v), &BTreeSet::new());
        assert!(block.contains("in-progress:"), "{block}");
        assert!(!block.contains("open:"), "{block}");
    }

    #[test]
    fn digest_json_shape() {
        let s = scan_with(vec![
            Item::new(ItemId::new(ItemKind::Task, 1), "x").status(Status::Blocked)
        ]);
        let j = digest_json(&s);
        assert_eq!(j["repo"], "arca");
        assert_eq!(j["counts"]["tasks"]["blocked"], 1);
        assert_eq!(j["blocked"].as_array().unwrap()[0]["id"], "T-001");
        assert!(j["open"].as_array().unwrap().is_empty());
    }
}

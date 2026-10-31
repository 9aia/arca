//! `arca epic [name]` / `arca milestone [name]` — grouping views
//! over item metadata. Bare lists every group with its live-item
//! count; a name renders that group's items as the board's five
//! status columns (`epic v2` ≡ `board --epic v2`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use arca_core::Item;
use serde_json::json;

use crate::{board, digest, items, ops};

/// Which metadata field the group verb reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// `item.epic` — `arca epic`.
    Epic,
    /// `item.milestone` — `arca milestone`.
    Milestone,
}

impl Group {
    /// The verb/field name in output (`epic`, `milestone`).
    pub fn name(self) -> &'static str {
        match self {
            Group::Epic => "epic",
            Group::Milestone => "milestone",
        }
    }

    /// The item's group membership, if any.
    pub fn of(self, item: &Item) -> Option<&str> {
        match self {
            Group::Epic => item.epic.as_deref(),
            Group::Milestone => item.milestone.as_deref(),
        }
    }
}

/// Live-item counts per group name — sorted (`BTreeMap`).
fn counts(scan: &digest::Scan, group: Group) -> BTreeMap<&str, usize> {
    let mut m: BTreeMap<&str, usize> = BTreeMap::new();
    for i in scan.live() {
        if let Some(name) = group.of(i) {
            *m.entry(name).or_default() += 1;
        }
    }
    m
}

/// The shared `epic`/`milestone` verb body. `all` lifts the board
/// row cap when a name renders a group board.
pub fn run(
    group: Group,
    name: Option<String>,
    md: bool,
    all: bool,
    dir: Option<PathBuf>,
    root: Option<PathBuf>,
    json_out: bool,
) -> Result<i32, String> {
    let (_proj, store) = items::open(dir.as_deref(), root.as_deref())?;
    let scan = digest::scan(&store)?;
    match name {
        None => {
            let groups = counts(&scan, group);
            if json_out {
                ops::print_json(&json!({
                    "group": group.name(),
                    "groups": groups
                        .iter()
                        .map(|(name, count)| json!({"name": name, "count": count}))
                        .collect::<Vec<_>>(),
                }));
            } else if groups.is_empty() {
                println!(
                    "{}: no {}s — set one via `arca item set <id> --{} <name>`",
                    scan.name,
                    group.name(),
                    group.name()
                );
            } else {
                for (name, count) in groups {
                    println!("{name}  {count}");
                }
            }
            Ok(0)
        }
        Some(name) => {
            if !arca_core::is_valid_group_name(&name) {
                return Err(format!("invalid {} name {name:?}", group.name()));
            }
            let members: Vec<&Item> = scan
                .live()
                .filter(|i| group.of(i) == Some(name.as_str()))
                .collect();
            if json_out {
                ops::print_json(&board::columns_json(members.into_iter(), None));
                return Ok(0);
            }
            if members.is_empty() {
                println!("{}: no items in {} {name:?}", scan.name, group.name());
            } else {
                let interrupted =
                    crate::continuity::interrupted_set(&store, members.iter().copied());
                ops::print_paged(&board::render(
                    members.into_iter(),
                    None,
                    md,
                    board::Fit::terminal(all),
                    &interrupted,
                ));
            }
            Ok(0)
        }
    }
}

//! `arca report` — aging + velocity + blocked, the "how is the
//! ledger trending" view.
//!
//! Aging buckets active items by `created` age (`<7d` / `7-30d` /
//! `>30d`; undated items are counted apart). Velocity replays the
//! project-branch history: `chore: done|resolve <id>` subjects are
//! completions, `chore: add|new …` are creations — grouped per ISO
//! week. Done items leave the files, so git is the only place that
//! history still exists.

use std::collections::BTreeMap;
use std::path::PathBuf;

use arca_core::{date, Item, Status};
use serde_json::json;

use crate::{digest, items, ops};

/// Open-item aging by `created` age.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Aging {
    /// Created < 7 days ago.
    pub lt7: usize,
    /// Created 7–30 days ago.
    pub d7_30: usize,
    /// Created > 30 days ago.
    pub gt30: usize,
    /// No `created` date.
    pub undated: usize,
}

/// Bucket active items by age relative to `today`.
pub fn aging<'a>(items: impl Iterator<Item = &'a Item>, today: time::Date) -> Aging {
    let mut a = Aging::default();
    for i in items {
        match i.created {
            None => a.undated += 1,
            Some(d) => {
                let age = (today - d).whole_days();
                if age < 7 {
                    a.lt7 += 1;
                } else if age <= 30 {
                    a.d7_30 += 1;
                } else {
                    a.gt30 += 1;
                }
            }
        }
    }
    a
}

/// What a `chore: <verb> …` subject did to the ledger.
fn classify(subject: &str) -> Option<Verb> {
    let rest = subject.strip_prefix("chore:")?.trim_start();
    let verb = rest.split_whitespace().next()?;
    match verb {
        "done" | "resolve" => Some(Verb::Done),
        "add" | "new" => Some(Verb::New),
        _ => None,
    }
}

enum Verb {
    Done,
    New,
}

/// One ISO week's tally.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WeekTally {
    /// Items created that week.
    pub new: usize,
    /// Items completed that week.
    pub done: usize,
}

/// Per-week velocity from worktree git history — `ISO-week →
/// {new, done}`, weeks sorted ascending. Entries with unparseable
/// dates or non-`chore:` subjects are skipped.
pub fn velocity(log: &[arca_git::LogEntry]) -> BTreeMap<(i32, u8), WeekTally> {
    let mut weeks: BTreeMap<(i32, u8), WeekTally> = BTreeMap::new();
    for e in log {
        let Some(v) = classify(&e.subject) else {
            continue;
        };
        let Ok(d) = date::parse(&e.date) else {
            continue;
        };
        let (iso_year, iso_week, _) = d.to_iso_week_date();
        let w = weeks.entry((iso_year, iso_week)).or_default();
        match v {
            Verb::Done => w.done += 1,
            Verb::New => w.new += 1,
        }
    }
    weeks
}

/// Days idle before an `in-progress` item counts as a stale claim
/// — the no-hook safety net: nobody called `arca interrupt`, but
/// work clearly isn't moving. Claimed items are excluded —
/// parking a claim is legitimate; in-progress idle is a leak.
pub const STALE_CLAIM_DAYS: i64 = 3;

/// `in-progress` items whose last activity (latest handoff, else
/// `created`) is older than [`STALE_CLAIM_DAYS`] — interrupted in
/// practice even if no hook fired.
fn stale_claims<'a>(
    store: &impl arca_store::Store,
    scan: &'a digest::Scan,
    today: time::Date,
) -> Vec<(&'a arca_core::Item, i64)> {
    scan.in_status(Status::InProgress)
        .into_iter()
        .filter_map(|i| {
            let last = store
                .read_handoffs(i.id)
                .ok()
                .and_then(|h| h.last().map(|e| e.date))
                .or(i.created)?;
            let idle = (today - last).whole_days();
            (idle > STALE_CLAIM_DAYS).then_some((i, idle))
        })
        .collect()
}

/// The report payload — aging + velocity + blocked + stale
/// claims — shared by `--json` and `arca serve`. `worktree` is
/// where the project-branch history lives (velocity reads the
/// git log).
pub fn report_json(
    store: &arca_store_md::MdStore,
    worktree: &std::path::Path,
) -> Result<serde_json::Value, String> {
    let scan = digest::scan(store)?;
    let today = items::today();
    let aging = aging(scan.active(), today);
    let log = arca_git::log(worktree, None).map_err(|e| e.to_string())?;
    let velocity = velocity(&log);
    let blocked = scan.in_status(Status::Blocked);
    let stale = stale_claims(store, &scan, today);
    Ok(json!({
        "repo": scan.name,
        "as_of": date::render(today),
        "aging": {
            "<7d": aging.lt7,
            "7-30d": aging.d7_30,
            ">30d": aging.gt30,
            "undated": aging.undated,
        },
        "velocity": velocity
            .iter()
            .map(|((y, w), t)| json!({
                "week": format!("{y}-W{w:02}"),
                "new": t.new,
                "done": t.done,
            }))
            .collect::<Vec<_>>(),
        "blocked": blocked
            .iter()
            .map(|i| serde_json::to_value(i).unwrap_or(json!(null)))
            .collect::<Vec<_>>(),
        "stale_claims": stale
            .iter()
            .map(|(i, idle)| json!({
                "id": i.id.to_string(),
                "title": i.title,
                "idle_days": idle,
            }))
            .collect::<Vec<_>>(),
    }))
}

/// The `report` verb.
pub fn run(dir: Option<PathBuf>, root: Option<PathBuf>, json_out: bool) -> Result<i32, String> {
    let (proj, store) = items::open(dir.as_deref(), root.as_deref())?;
    if json_out {
        ops::print_json(&report_json(&store, &proj.worktree)?);
        return Ok(0);
    }
    let scan = digest::scan(&store)?;
    let today = items::today();
    let aging = aging(scan.active(), today);
    let log = arca_git::log(&proj.worktree, None).map_err(|e| e.to_string())?;
    let velocity = velocity(&log);
    let blocked = scan.in_status(Status::Blocked);
    let stale = stale_claims(&store, &scan, today);
    println!("{} — report as of {}", scan.name, date::render(today));
    println!(
        "aging:    <7d {} · 7-30d {} · >30d {} · undated {}",
        aging.lt7, aging.d7_30, aging.gt30, aging.undated
    );
    if velocity.is_empty() {
        println!("velocity: no chore: history yet");
    } else {
        let parts: Vec<String> = velocity
            .iter()
            .map(|((y, w), t)| format!("{y}-W{w:02} +{}/-{}", t.new, t.done))
            .collect();
        println!("velocity: {}", parts.join(" · "));
    }
    if !blocked.is_empty() {
        println!("blocked:");
        for i in blocked {
            println!("  {}", items::compact(i));
        }
    }
    if !stale.is_empty() {
        println!("stale claims (in-progress >{STALE_CLAIM_DAYS}d idle — interrupted?):");
        for (i, idle) in stale {
            println!("  {} · {idle}d", items::compact(i));
        }
    }
    Ok(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use arca_core::{ItemId, ItemKind};

    fn entry(date: &str, subject: &str) -> arca_git::LogEntry {
        arca_git::LogEntry {
            hash: "h".into(),
            date: date.into(),
            subject: subject.into(),
        }
    }

    #[test]
    fn aging_buckets() {
        let today = date::parse("2026-02-09").unwrap();
        let mk = |d: Option<&str>| {
            let mut i = Item::new(ItemId::new(ItemKind::Task, 1), "x");
            i.created = d.map(|s| date::parse(s).unwrap());
            i
        };
        let items = [
            mk(Some("2026-02-09")), // today
            mk(Some("2026-02-03")), // 6d
            mk(Some("2026-02-02")), // 7d
            mk(Some("2026-01-11")), // 29d
            mk(Some("2026-01-10")), // 30d
            mk(Some("2026-01-09")), // 31d
            mk(None),
        ];
        let a = aging(items.iter(), today);
        assert_eq!(a.lt7, 2);
        assert_eq!(a.d7_30, 3);
        assert_eq!(a.gt30, 1);
        assert_eq!(a.undated, 1);
    }

    #[test]
    fn velocity_parses_chore_subjects() {
        let log = vec![
            entry("2026-02-09", "chore: done T-001"),
            entry("2026-02-09", "chore: resolve I-002"),
            entry("2026-02-08", "chore: add T-003"),
            entry("2026-02-08", "chore: new issue I-002"),
            entry("2026-02-08", "chore: defer T-004"),
            entry("2026-02-08", "feat: unrelated"),
            entry("2026-02-02", "chore: done T-005"),
            entry("bogus", "chore: done T-006"),
            entry("2026-02-07", "chore:"),
        ];
        let v = velocity(&log);
        assert_eq!(v.len(), 2);
        // 2026-02-09 is a Monday → W07; 02-02…08 → W06
        let w7 = v[&(2026, 7)];
        assert_eq!(w7.done, 2);
        assert_eq!(w7.new, 0);
        let w6 = v[&(2026, 6)];
        assert_eq!(w6.done, 1);
        assert_eq!(w6.new, 2);
    }
}

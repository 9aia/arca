//! `arca board [--md]` — kanban of the current repo's ledger:
//! Backlog | Open | Claimed | In Progress | Blocked columns (done
//! items leave the ledger, so there's no Done column). Human
//! output is a `tabled` grid; `--md` is the same board as a
//! markdown table for embedding in docs; `--json` is the columns
//! map.

use std::collections::BTreeSet;
use std::path::PathBuf;

use arca_core::{Item, Status};
use serde_json::json;
use tabled::settings::Style;
use tabled::{Table, Tabled};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{digest, items, ops};

/// Default cap on board rows for human output (`--all` lifts it) —
/// a screenful, so a huge ledger can't bury the terminal.
pub const DEFAULT_MAX_ROWS: usize = 25;

/// The ascii style's per-table overhead: `| ` + ` ` per column
/// (3×5) plus the outer borders (1).
const TABLE_OVERHEAD: usize = 16;

/// Human-render constraints for the board — width budget + row
/// cap. `--md`/`--json` use [`Fit::UNBOUNDED`]; caps are a terminal
/// concern only.
#[derive(Debug, Clone, Copy)]
pub struct Fit {
    /// Total table width budget in terminal columns.
    pub width: usize,
    /// Max data rows before a `… +N more` footer.
    pub max_rows: usize,
}

impl Fit {
    /// No constraints — markdown + JSON output.
    pub const UNBOUNDED: Fit = Fit {
        width: usize::MAX,
        max_rows: usize::MAX,
    };

    /// Fit for human output — `all` lifts the row cap.
    pub fn terminal(all: bool) -> Fit {
        Fit {
            width: term_width(),
            max_rows: if all { usize::MAX } else { DEFAULT_MAX_ROWS },
        }
    }
}

/// Display width budget: `$ARCA_WIDTH` → tty width → 100 piped.
fn term_width() -> usize {
    if let Some(w) = std::env::var("ARCA_WIDTH")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        return w;
    }
    terminal_size::terminal_size()
        .map(|(w, _)| usize::from(w.0))
        .unwrap_or(100)
}

/// ANSI on? `NO_COLOR` kills it, `CLICOLOR_FORCE`/`FORCE_COLOR`
/// force it, else only on a tty.
fn color_enabled() -> bool {
    use std::io::IsTerminal;
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if std::env::var("CLICOLOR_FORCE")
        .or_else(|_| std::env::var("FORCE_COLOR"))
        .is_ok_and(|v| v != "0")
    {
        return true;
    }
    std::io::stdout().is_terminal()
}

/// `\x1b[<code>m…\x1b[0m` wrapper — raw escapes, no dep.
fn sgr(code: &str, s: &str) -> String {
    format!("\x1b[{code}m{s}\x1b[0m")
}

fn dim(s: &str) -> String {
    sgr("2", s)
}

/// Per-lane header tints — parked lanes dim, active lanes bright.
const HEADER_STYLE: [(&str, &str); 5] = [
    ("Backlog", "2"),
    ("Open", "1;36"),
    ("Claimed", "1;33"),
    ("In Progress", "1;32"),
    ("Blocked", "1;31"),
];

/// `T-008` / `thal:T-118` / `ADR-0001` — the leading card token.
fn looks_like_id(tok: &str) -> bool {
    let id = tok.rsplit(':').next().unwrap_or(tok);
    let mut parts = id.split('-');
    matches!(parts.next(), Some(k) if !k.is_empty() && k.chars().all(|c| c.is_ascii_uppercase()))
        && matches!(parts.next(), Some(n) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// A header cell (` Name `) gets its lane tint, padding kept.
fn style_header_cell(cell: &str) -> String {
    let name = cell.trim();
    if let Some((_, code)) = HEADER_STYLE.iter().find(|(n, _)| *n == name) {
        let lead = &cell[..cell.len() - cell.trim_start().len()];
        let trail = &cell[cell.trim_end().len()..];
        return format!("{lead}{}{trail}", sgr(code, name));
    }
    cell.to_string()
}

/// A card cell gets its leading item id bolded.
fn style_card_cell(cell: &str) -> String {
    let lead_len = cell.len() - cell.trim_start().len();
    let trail_start = cell.trim_end().len();
    if lead_len >= trail_start {
        return cell.to_string();
    }
    let body = &cell[lead_len..trail_start];
    let id_len = body.find(' ').unwrap_or(body.len());
    let (id, rest) = body.split_at(id_len);
    if !looks_like_id(id) {
        return cell.to_string();
    }
    format!(
        "{}{}{rest}{}",
        &cell[..lead_len],
        sgr("1", id),
        &cell[trail_start..]
    )
}

/// Paint a rendered board: dim the `+-|` chrome so content pops,
/// tint the header lane names, bold card ids, dim the footer.
fn colorize(out: &str) -> String {
    let pipe = dim("|");
    let mut header_done = false;
    let mut lines = Vec::new();
    for line in out.split('\n') {
        if line.is_empty() {
            lines.push(String::new());
        } else if line.starts_with('+') {
            lines.push(dim(line));
        } else if line.starts_with('|') {
            let styled: Vec<String> = line
                .split('|')
                .map(|cell| {
                    if header_done {
                        style_card_cell(cell)
                    } else {
                        style_header_cell(cell)
                    }
                })
                .collect();
            header_done = true;
            lines.push(styled.join(&pipe));
        } else {
            lines.push(dim(line));
        }
    }
    lines.join("\n")
}

/// `…`-truncate `s` to `w` display columns.
fn truncate(s: &str, w: usize) -> String {
    if UnicodeWidthStr::width(s) <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + cw > w.saturating_sub(1) {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    out
}

/// Water-fill `budget` across the five columns: a column under the
/// fair share keeps its natural width and the rest split what's
/// left. Never grows a column past its natural width.
fn fit_widths(natural: [usize; 5], budget: usize) -> [usize; 5] {
    let mut out = natural;
    let mut locked = [false; 5];
    let mut open = 5usize;
    let mut rem = budget;
    loop {
        if open == 0 {
            break;
        }
        let share = rem / open;
        let mut extra = rem - share * open;
        let mut progressed = false;
        for c in 0..5 {
            if !locked[c] && out[c] <= share {
                locked[c] = true;
                open -= 1;
                rem -= out[c];
                progressed = true;
            }
        }
        if !progressed {
            for c in 0..5 {
                if !locked[c] {
                    // every open column wants more than the share
                    out[c] = share + usize::from(extra > 0);
                    extra = extra.saturating_sub(1);
                    locked[c] = true;
                    open -= 1;
                }
            }
        }
    }
    out
}

/// Column order — a `deferred` item is a backlog item wherever it
/// lives, and `done` has no column (done leaves the ledger).
pub const COLUMNS: [(&str, Option<Status>); 5] = [
    ("backlog", Some(Status::Deferred)),
    ("open", Some(Status::Open)),
    ("claimed", Some(Status::Claimed)),
    ("in-progress", Some(Status::InProgress)),
    ("blocked", Some(Status::Blocked)),
];

/// One board row — five card cells, any may be empty.
#[derive(Tabled)]
struct Row {
    #[tabled(rename = "Backlog")]
    backlog: String,
    #[tabled(rename = "Open")]
    open: String,
    #[tabled(rename = "Claimed")]
    claimed: String,
    #[tabled(rename = "In Progress")]
    in_progress: String,
    #[tabled(rename = "Blocked")]
    blocked: String,
}

/// Card text — `T-012 the title`, with an optional prefix
/// (`arca:`) for org boards.
fn card(item: &Item, prefix: Option<&str>, interrupted: bool) -> String {
    let id = match prefix {
        Some(p) => format!("{p}:{}", item.id),
        None => item.id.to_string(),
    };
    let id = if interrupted { format!("{id} ⚡") } else { id };
    if item.title.is_empty() {
        id
    } else {
        format!("{id} {}", item.title)
    }
}

/// Bucket live items into the five columns (in-column order is by
/// priority then id — the most urgent cards float to the top).
fn columns<'a>(items: impl Iterator<Item = &'a Item>) -> [Vec<&'a Item>; 5] {
    let mut cols: [Vec<&'a Item>; 5] = Default::default();
    for i in items {
        let idx = COLUMNS.iter().position(|(_, s)| *s == Some(i.status));
        if let Some(idx) = idx {
            cols[idx].push(i);
        }
    }
    for col in &mut cols {
        col.sort_by(|a, b| digest::pri_then_id(a, b));
    }
    cols
}

/// Render the board. `prefix` puts `repo:` on every card (org).
/// `fit` constrains human output (`--md` passes [`Fit::UNBOUNDED`]).
/// `interrupted` ids get a `⚡` marker on their card.
pub fn render<'a>(
    items: impl Iterator<Item = &'a Item>,
    prefix: Option<&str>,
    md: bool,
    fit: Fit,
    interrupted: &BTreeSet<arca_core::ItemId>,
) -> String {
    let cols = columns(items);
    let cards: [Vec<String>; 5] = std::array::from_fn(|c| {
        cols[c]
            .iter()
            .map(|i| card(i, prefix, interrupted.contains(&i.id)))
            .collect()
    });
    render_cards(&cards, md, fit)
}

/// Render five columns of card strings — shared with `org board`,
/// which builds mixed-repo cards itself.
pub fn render_cards(cards: &[Vec<String>; 5], md: bool, fit: Fit) -> String {
    let fit = if md { Fit::UNBOUNDED } else { fit };
    // shrink cards to the terminal budget — wide columns split
    // what's left once narrow ones keep their natural width
    let headers = ["Backlog", "Open", "Claimed", "In Progress", "Blocked"];
    let natural: [usize; 5] = std::array::from_fn(|c| {
        cards[c]
            .iter()
            .map(|s| UnicodeWidthStr::width(s.as_str()))
            .max()
            .unwrap_or(0)
            .max(UnicodeWidthStr::width(headers[c]))
    });
    let budget = fit.width.saturating_sub(TABLE_OVERHEAD);
    let widths = fit_widths(natural, budget);
    let height = cards.iter().map(Vec::len).max().unwrap_or(0);
    let shown = height.min(fit.max_rows);
    let mut rows = Vec::with_capacity(shown);
    for r in 0..shown {
        let cell = |c: usize| {
            cards[c]
                .get(r)
                .map(|s| truncate(s, widths[c]))
                .unwrap_or_default()
        };
        rows.push(Row {
            backlog: cell(0),
            open: cell(1),
            claimed: cell(2),
            in_progress: cell(3),
            blocked: cell(4),
        });
    }
    let mut table = Table::new(rows);
    if md {
        table.with(Style::markdown());
    }
    let mut out = table.to_string();
    let hidden: usize = cards.iter().map(|c| c.len().saturating_sub(shown)).sum();
    if hidden > 0 {
        out.push_str(&format!("\n… +{hidden} more (`--all` to show)"));
    }
    if !md && color_enabled() {
        out = colorize(&out);
    }
    out
}

/// Bucket `(label, item)` pairs into the five columns, sorting
/// each by priority — org cards carry their repo prefix already.
pub fn cards_by_column<'a>(items: impl Iterator<Item = (String, &'a Item)>) -> [Vec<String>; 5] {
    let mut cols: [Vec<(&'a Item, String)>; 5] = Default::default();
    for (label, i) in items {
        if let Some(idx) = COLUMNS.iter().position(|(_, s)| *s == Some(i.status)) {
            cols[idx].push((i, label));
        }
    }
    std::array::from_fn(|c| {
        let mut col = std::mem::take(&mut cols[c]);
        col.sort_by(|(a, _), (b, _)| digest::pri_then_id(a, b));
        col.into_iter().map(|(_, l)| l).collect()
    })
}

/// The `--json` columns map — column name → full items.
pub fn columns_json<'a>(
    items: impl Iterator<Item = &'a Item>,
    prefix: Option<&str>,
) -> serde_json::Value {
    let cols = columns(items);
    let mut map = serde_json::Map::new();
    for (i, (name, _)) in COLUMNS.iter().enumerate() {
        map.insert(
            (*name).to_string(),
            json!(cols[i]
                .iter()
                .map(|it| {
                    let mut v = serde_json::to_value(it).unwrap_or(json!(null));
                    if let (Some(p), Some(obj)) = (prefix, v.as_object_mut()) {
                        obj.insert("repo".into(), json!(p));
                    }
                    v
                })
                .collect::<Vec<_>>()),
        );
    }
    json!(map)
}

/// The `board` verb. `--epic`/`--milestone` scope the columns to
/// items in that group (both set = AND); `--all` lifts the row cap.
pub fn run(
    dir: Option<PathBuf>,
    root: Option<PathBuf>,
    md: bool,
    all: bool,
    epic: Option<String>,
    milestone: Option<String>,
    json_out: bool,
) -> Result<i32, String> {
    let (_proj, store) = items::open(dir.as_deref(), root.as_deref())?;
    let scan = digest::scan(&store)?;
    let live: Vec<&Item> = scan
        .live()
        .filter(|i| {
            epic.as_deref().is_none_or(|e| i.epic.as_deref() == Some(e))
                && milestone
                    .as_deref()
                    .is_none_or(|m| i.milestone.as_deref() == Some(m))
        })
        .collect();
    if json_out {
        ops::print_json(&columns_json(live.into_iter(), None));
        return Ok(0);
    }
    if live.is_empty() {
        println!("{}: empty board — `arca todo add` something", scan.name);
    } else {
        let interrupted = crate::continuity::interrupted_set(&store, live.iter().copied());
        ops::print_paged(&render(
            live.into_iter(),
            None,
            md,
            Fit::terminal(all),
            &interrupted,
        ));
    }
    Ok(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use arca_core::{ItemId, ItemKind, Priority};

    fn items() -> Vec<Item> {
        vec![
            Item::new(ItemId::new(ItemKind::Task, 1), "parked").status(Status::Deferred),
            Item::new(ItemId::new(ItemKind::Task, 2), "plain"),
            Item::new(ItemId::new(ItemKind::Task, 3), "urgent").priority(Priority::Critical),
            Item::new(ItemId::new(ItemKind::Issue, 1), "owned").status(Status::Claimed),
            Item::new(ItemId::new(ItemKind::Task, 4), "wip").status(Status::InProgress),
            Item::new(ItemId::new(ItemKind::Task, 5), "stuck").status(Status::Blocked),
            Item::new(ItemId::new(ItemKind::Task, 6), "gone").status(Status::Done),
        ]
    }

    #[test]
    fn columns_bucket_and_sort() {
        let v = items();
        let cols = columns(v.iter());
        assert_eq!(cols[0][0].id, ItemId::new(ItemKind::Task, 1));
        // open column: crit before med
        assert_eq!(
            cols[1].iter().map(|i| i.id.number()).collect::<Vec<_>>(),
            [3, 2]
        );
        assert_eq!(cols[2][0].id, ItemId::new(ItemKind::Issue, 1));
        assert_eq!(cols[3][0].id, ItemId::new(ItemKind::Task, 4));
        assert_eq!(cols[4][0].id, ItemId::new(ItemKind::Task, 5));
        // done items land nowhere
        assert!(cols.iter().flatten().all(|i| i.status != Status::Done));
    }

    #[test]
    fn render_grid_and_md() {
        let v = items();
        let grid = render(v.iter(), None, false, Fit::UNBOUNDED, &BTreeSet::new());
        assert!(grid.contains("Backlog"));
        assert!(grid.contains("In Progress"));
        assert!(grid.contains("T-004 wip"));
        let md = render(
            v.iter(),
            Some("arca"),
            true,
            Fit::UNBOUNDED,
            &BTreeSet::new(),
        );
        assert!(md.starts_with('|'));
        assert!(md.contains("arca:T-001 parked"));
        assert!(md.contains("arca:I-001 owned"));
        // markdown table: header + separator + rows
        assert!(md.lines().any(|l| l.contains("---")));
    }

    #[test]
    fn fit_widths_water_fills() {
        // narrow columns keep natural width; the wide one gets
        // what's left of the budget
        assert_eq!(fit_widths([500, 4, 4, 4, 4], 84), [68, 4, 4, 4, 4]);
        // everything fits → natural widths unchanged
        assert_eq!(fit_widths([20, 10, 8, 11, 7], 100), [20, 10, 8, 11, 7]);
        // several wide columns split evenly (remainder to the first)
        assert_eq!(fit_widths([30, 30, 30, 4, 4], 50), [14, 14, 14, 4, 4]);
        // never grown past natural
        assert_eq!(fit_widths([80, 4, 4, 4, 4], 100), [80, 4, 4, 4, 4]);
    }

    #[test]
    fn truncate_fits_display_width() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("T-148 Fix https://long-url", 10), "T-148 Fix…");
        assert_eq!(truncate("ab", 1), "…");
    }

    #[test]
    fn looks_like_id_covers_kinds_and_prefixes() {
        for id in ["T-008", "I-1", "P-001", "PL-12", "ADR-0001", "thal:T-118"] {
            assert!(looks_like_id(id), "{id}");
        }
        for not in ["", "task", "a-1", "T-x", "-1", "1-2"] {
            assert!(!looks_like_id(not), "{not}");
        }
    }

    #[test]
    fn colorize_dims_chrome_and_bolds_ids() {
        let v = [Item::new(ItemId::new(ItemKind::Task, 8), "work")];
        let plain = render(v.iter(), None, false, Fit::UNBOUNDED, &BTreeSet::new());
        let out = colorize(&plain);
        // borders dimmed
        assert!(out.contains("\x1b[2m+"), "{out}");
        // header tinted
        assert!(out.contains("\x1b[1;36mOpen\x1b[0m"), "{out}");
        assert!(out.contains("\x1b[1;31mBlocked\x1b[0m"), "{out}");
        // card id bold
        assert!(out.contains("\x1b[1mT-008\x1b[0m"), "{out}");
        // content untouched
        assert!(out.contains("work"), "{out}");
    }

    #[test]
    fn render_caps_rows_and_width() {
        let mut v = Vec::new();
        for n in 1..=30 {
            v.push(
                Item::new(
                    ItemId::new(ItemKind::Task, n),
                    format!("a fairly long task title number {n}"),
                )
                .status(Status::Deferred),
            );
        }
        let fit = Fit {
            width: 60,
            max_rows: 25,
        };
        let out = render(v.iter(), None, false, fit, &BTreeSet::new());
        assert!(out.contains("… +5 more"), "{out}");
        for line in out.lines() {
            assert!(
                UnicodeWidthStr::width(line) <= 60 + 5, // +header slack
                "{line}"
            );
        }
        // --all-equivalent: uncapped renders every row
        let full = render(v.iter(), None, false, Fit::UNBOUNDED, &BTreeSet::new());
        assert!(full.contains("number 30"));
    }

    #[test]
    fn card_bare_id_for_empty_title() {
        let i = Item::new(ItemId::new(ItemKind::Task, 9), "");
        assert_eq!(card(&i, None, false), "T-009");
        assert_eq!(card(&i, Some("arca"), false), "arca:T-009");
    }

    #[test]
    fn columns_json_map() {
        let v = items();
        let j = columns_json(v.iter(), Some("arca"));
        assert_eq!(j["open"].as_array().unwrap().len(), 2);
        assert_eq!(j["open"][0]["id"], "T-003");
        assert_eq!(j["open"][0]["repo"], "arca");
        assert!(j["blocked"].as_array().unwrap()[0]["id"] == "T-005");
        assert!(j["backlog"].as_array().unwrap().len() == 1);
    }
}

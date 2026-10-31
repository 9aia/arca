//! `arca absorb comments|jira|github` — import work into the
//! ledger from three sources:
//!
//! - `comments` — `rg` sweeps TODO/FIXME/HACK (any `--kinds`)
//!   comments at the repo root (respects `.gitignore`; `project/`,
//!   `docs/`, `target/` excluded). Each hit becomes a backlog item
//!   or an issue, `note` = `file:line`.
//! - `jira` — a Jira CSV export (`Issue key`, `Summary`, `Status`,
//!   `Priority`, `Assignee`, `Labels`; header-name/case tolerant,
//!   BOM and quoted multi-line cells fine, optional columns may be
//!   absent, `Epic Link`/`Epic Name`/`Parent key` and
//!   `Fix Version/s` variants understood) → `I-NNN` issues, the
//!   Jira key preserved in `note` (`jira:PROJ-123`).
//! - `github` — a GitHub issues dump (JSON array, an API
//!   `{"issues": […]}`/`{"items": […]}` wrapper, or CSV) → `I-NNN`
//!   issues, `note` = `github:<number>`.
//!
//! Dedup is uniform: a row is skipped when its `note` marker
//! (`file:line`, `jira:KEY`, `github:N`) already appears in any
//! ledger item's note — re-running an import is a no-op. Done-ish
//! rows are skipped unless `--include-done`, which creates them
//! `done` and files them straight into `archive/`. Row-level
//! failures (unreadable records, rows without a title/key,
//! non-object issues) collect into the report's `warnings` — one
//! bad row never aborts the file. Every source ends in one
//! `chore: absorb <source> (N items)` commit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use arca_core::{Actor, Item, ItemFile, ItemId, ItemKind, Priority, Status};
use arca_store::Store;
use clap::Subcommand;
use serde_json::json;

use crate::discover::Project;
use crate::{digest, items, ops, templates};

/// The `absorb` verb group.
#[derive(Subcommand)]
pub enum Cmd {
    /// Sweep code comments into the ledger via ripgrep (`rg` must
    /// be installed — respects `.gitignore`; `project/`, `docs/`,
    /// `target/` are excluded).
    Comments {
        /// Directory to scan (default: the repo root).
        #[arg(long)]
        path: Option<PathBuf>,
        /// Comment kinds to match — comma-separated or repeatable
        /// (default TODO,FIXME,HACK).
        #[arg(long, value_delimiter = ',')]
        kinds: Option<Vec<String>>,
        /// Where findings land: `backlog` items or `issues` files
        /// (default `backlog`).
        #[arg(long, default_value = "backlog")]
        to: String,
        /// Report findings without creating items or committing.
        #[arg(long)]
        dry: bool,
    },
    /// Import a Jira CSV export. `Done`/`Resolved` rows are skipped
    /// unless `--include-done` (created done + archived).
    Jira {
        /// The export file (`.csv`).
        file: PathBuf,
        /// Import done/resolved issues too.
        #[arg(long)]
        include_done: bool,
    },
    /// Import a GitHub issues dump — a JSON array (`number`,
    /// `title`, `state`, `labels`, `assignee`, `body`) or the CSV
    /// equivalent. `closed` rows are skipped unless
    /// `--include-done` (created done + archived).
    Github {
        /// The export file (`.json` or `.csv`).
        file: PathBuf,
        /// Import closed issues too.
        #[arg(long)]
        include_done: bool,
    },
}

/// The `absorb` verb group body.
pub fn run(
    cmd: Cmd,
    dir: Option<PathBuf>,
    root: Option<PathBuf>,
    json_out: bool,
    no_commit: bool,
) -> Result<i32, String> {
    let (proj, store) = items::open(dir.as_deref(), root.as_deref())?;
    match cmd {
        Cmd::Comments {
            path,
            kinds,
            to,
            dry,
        } => comments(
            &proj,
            &store,
            CommentSpec {
                path,
                kinds,
                to,
                dry,
            },
            json_out,
            no_commit,
        ),
        Cmd::Jira { file, include_done } => {
            jira(&proj, &store, &file, include_done, json_out, no_commit)
        }
        Cmd::Github { file, include_done } => {
            github(&proj, &store, &file, include_done, json_out, no_commit)
        }
    }
}

// ---------- shared ----------

/// Statuses an import row can carry — richer than open/done so
/// Jira's `In Progress`/`Blocked` survive the trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImportStatus {
    Open,
    InProgress,
    Blocked,
    Done,
}

/// One normalized import row (Jira + GitHub reduce to this).
struct ImportRow {
    title: String,
    status: ImportStatus,
    priority: Priority,
    owner: Option<Actor>,
    labels: Vec<String>,
    /// Dedup key + provenance — `jira:PROJ-123`, `github:42`.
    note: String,
    epic: Option<String>,
    milestone: Option<String>,
    body: Option<String>,
}

/// Every `note` in the ledger (live + archived) — the dedup set.
fn existing_notes(scan: &digest::Scan) -> Vec<String> {
    scan.items.iter().filter_map(|i| i.note.clone()).collect()
}

/// Does `marker` appear in any existing item note? The match is
/// boundary-checked — `jira:PROJ-1` must not dedupe `jira:PROJ-10`
/// (and `src/a.rs:1` must not shadow `src/a.rs:10`), so the marker
/// counts only when it isn't fused into a longer id/path.
fn deduped(notes: &[String], marker: &str) -> bool {
    fn part_of(c: char) -> bool {
        c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '/' | '.')
    }
    notes.iter().any(|n| {
        n.match_indices(marker).any(|(start, _)| {
            let end = start + marker.len();
            let before_ok = start == 0 || !n[..start].chars().last().is_some_and(part_of);
            let after_ok = end >= n.len() || !n[end..].chars().next().is_some_and(part_of);
            before_ok && after_ok
        })
    })
}

/// Import rows as `I-NNN` issues. Returns `(created, deduped,
/// skipped_done, archived, ids)` — `skipped_done` counts `Done`
/// rows dropped for lacking `--include-done`.
fn import_issues(
    store: &arca_store_md::MdStore,
    notes: &[String],
    rows: &[ImportRow],
    include_done: bool,
) -> Result<(usize, usize, usize, usize, Vec<ItemId>), String> {
    let (mut created, mut dup, mut skipped_done, mut archived) = (0, 0, 0, 0);
    let mut ids = Vec::new();
    for row in rows {
        if deduped(notes, &row.note) {
            dup += 1;
            continue;
        }
        if row.status == ImportStatus::Done && !include_done {
            skipped_done += 1;
            continue;
        }
        let id = store.alloc_id(ItemKind::Issue).map_err(|e| e.to_string())?;
        let mut meta = Item::new(id, &row.title)
            .created(items::today())
            .priority(row.priority)
            .note(&row.note);
        meta.status = match row.status {
            ImportStatus::Open => Status::Open,
            ImportStatus::InProgress => Status::InProgress,
            ImportStatus::Blocked => Status::Blocked,
            ImportStatus::Done => Status::Done,
        };
        meta.owner = row.owner.clone();
        meta.labels = row.labels.iter().cloned().collect();
        meta.epic = row.epic.clone();
        meta.milestone = row.milestone.clone();
        let body = row
            .body
            .clone()
            .filter(|b| !b.trim().is_empty())
            .unwrap_or_else(|| templates::issue(&meta));
        let file = ItemFile::new(meta).body(body);
        store.save_item(&file).map_err(|e| e.to_string())?;
        if row.status == ImportStatus::Done {
            store.archive_item(id).map_err(|e| e.to_string())?;
            archived += 1;
        }
        ids.push(id);
        created += 1;
    }
    Ok((created, dup, skipped_done, archived, ids))
}

/// The shared import tail — report + `chore: absorb <src>`.
fn finish(
    proj: &Project,
    source: &str,
    payload: serde_json::Value,
    created: usize,
    json_out: bool,
    no_commit: bool,
) -> Result<i32, String> {
    let committed = items::commit(
        &proj.worktree,
        &format!("chore: absorb {source} ({created} items)"),
        no_commit,
    )?;
    items::report(
        payload,
        committed,
        json_out,
        &format!("absorbed {created} items from {source}"),
    )
}

// ---------- title/field sanitation ----------

/// A ledger-safe title: `·` → `-` (line grammar), whitespace
/// collapsed, ~70 chars max at a char boundary (with `…` when cut).
fn clean_title(raw: &str) -> String {
    let mut s = String::new();
    let mut last_ws = false;
    for c in raw.chars() {
        // `·` is the line-grammar separator — it can't survive in a
        // title; map it to `-` so words keep their boundary
        let c = if c == '·' { '-' } else { c };
        if c.is_whitespace() {
            if !last_ws && !s.is_empty() {
                s.push(' ');
            }
            last_ws = true;
        } else {
            s.push(c);
            last_ws = false;
        }
    }
    let s = s.trim().to_string();
    if s.chars().count() <= 70 {
        return s;
    }
    let cut: String = s.chars().take(69).collect();
    format!("{}…", cut.trim_end())
}

/// Force `raw` into the group-name charset `[A-Za-z0-9_.-]` —
/// invalid runs collapse to one `-`. `None` when nothing survives.
fn sanitize_group(raw: &str) -> Option<String> {
    let mut out = String::new();
    for c in raw.trim().chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
            out.push(c);
        } else if !matches!(out.chars().last(), None | Some('-' | '.')) {
            out.push('-');
        }
    }
    let out = out.trim_matches(|c| c == '-' || c == '.').to_string();
    if arca_core::is_valid_group_name(&out) {
        Some(out)
    } else {
        None
    }
}

/// Force `raw` into a `#label` (`[A-Za-z0-9_-]`, `.` collapses too).
fn sanitize_label(raw: &str) -> Option<String> {
    sanitize_group(raw).filter(|s| arca_core::is_valid_label(s))
}

/// Split label cells into valid `#label`s — `,`/`;` separated;
/// each piece is one label (GitHub labels carry spaces, so they're
/// sanitized to dashes, not split).
fn labels_from(raw_values: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for v in raw_values {
        for piece in v.split([',', ';']) {
            if let Some(l) = sanitize_label(piece) {
                if !out.contains(&l) {
                    out.push(l);
                }
            }
        }
    }
    out
}

/// An `owner:` actor from import text — tries the raw name, then a
/// lowercased/dashed form (`"Luis Figueiredo"` → `luis-figueiredo`).
fn sanitize_actor(raw: &str) -> Option<Actor> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(a) = Actor::new(raw) {
        return Some(a);
    }
    let mut out = String::new();
    for c in raw.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':') {
            out.push(c);
        } else if !matches!(out.chars().last(), None | Some('-' | '.' | ':')) {
            out.push('-');
        }
    }
    Actor::new(out.trim_matches(|c| matches!(c, '-' | '.' | ':'))).ok()
}

// ---------- comments ----------

/// One rg hit reduced to an absorbable finding.
struct Finding {
    file: String,
    line: u32,
    kind: String,
    title: String,
    /// `file:line` — the note + dedup marker.
    note: String,
}

/// `path:line:text` → `(path, line, text)`; `None` when the shape
/// doesn't fit (rg context lines, binary hits).
fn parse_rg_line(line: &str) -> Option<(&str, u32, &str)> {
    let (file, rest) = line.split_once(':')?;
    let (num, text) = rest.split_once(':')?;
    let num: u32 = num.trim().parse().ok()?;
    Some((file, num, text))
}

/// Extract the comment text after the matched kind keyword —
/// `// TODO(devin): fix the thing` → `fix the thing`.
fn comment_title(text: &str, kinds: &[String]) -> String {
    // the earliest kind occurrence wins
    let mut best: Option<(usize, &str)> = None;
    for k in kinds {
        if let Some(p) = text.find(k.as_str()) {
            if best.is_none_or(|(bp, _)| p < bp) {
                best = Some((p, k));
            }
        }
    }
    let Some((pos, kind)) = best else {
        return clean_title(text);
    };
    let mut rest = &text[pos + kind.len()..];
    rest = rest.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
    // `(who)` attribution between kind and text
    if let Some(after) = rest.strip_prefix('(') {
        if let Some(close) = after.find(')') {
            let tail = &after[close + 1..];
            if tail.starts_with(':') || tail.starts_with(char::is_whitespace) {
                rest = tail.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
            }
        }
    }
    clean_title(rest)
}

/// The `comments` flag bundle — keeps `comments()` under the
/// arg-count lint.
struct CommentSpec {
    path: Option<PathBuf>,
    kinds: Option<Vec<String>>,
    to: String,
    dry: bool,
}

fn comments(
    proj: &Project,
    store: &arca_store_md::MdStore,
    spec: CommentSpec,
    json_out: bool,
    no_commit: bool,
) -> Result<i32, String> {
    let CommentSpec {
        path,
        kinds,
        to,
        dry,
    } = spec;
    let to = to.as_str();
    let kinds = kinds.unwrap_or_else(|| vec!["TODO".into(), "FIXME".into(), "HACK".into()]);
    for k in &kinds {
        if k.is_empty() || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(format!(
                "invalid comment kind {k:?} — expected [A-Za-z0-9_]+ (e.g. TODO, FIXME, HACK)"
            ));
        }
    }
    if to != "backlog" && to != "issues" {
        return Err(format!("--to takes backlog|issues — got {to:?}"));
    }
    let scan_dir = match &path {
        Some(p) => p
            .canonicalize()
            .map_err(|e| format!("{}: {e}", p.display()))?,
        None => proj.root.clone(),
    };
    if !scan_dir.is_dir() {
        return Err(format!("{} is not a directory", scan_dir.display()));
    }
    let pattern = format!("({})", kinds.join("|"));
    let cmd = format!(
        "rg -n --no-heading --glob '!project/' --glob '!docs/' --glob '!target/' '{pattern}'"
    );
    let out = arca_shell::output(&scan_dir, &cmd)
        .map_err(|e| format!("rg failed to run in {}: {e}", scan_dir.display()))?;
    match out.status.code() {
        Some(0) | Some(1) => {} // 0 = hits, 1 = none
        other => {
            return Err(format!(
                "rg exited {other:?} in {}: {}",
                scan_dir.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut findings = Vec::new();
    for line in stdout.lines() {
        let Some((file, num, text)) = parse_rg_line(line) else {
            continue;
        };
        let file = file.strip_prefix("./").unwrap_or(file);
        let title = comment_title(text, &kinds);
        let kind = kinds
            .iter()
            .find(|k| text.contains(k.as_str()))
            .cloned()
            .unwrap_or_default();
        let title = if title.is_empty() {
            format!("{kind} at {file}:{num}")
        } else {
            title
        };
        findings.push(Finding {
            file: file.to_string(),
            line: num,
            kind,
            title,
            note: format!("{file}:{num}"),
        });
    }
    let scan = digest::scan(store)?;
    let notes = existing_notes(&scan);
    let fresh: Vec<&Finding> = findings
        .iter()
        .filter(|f| !deduped(&notes, &f.note))
        .collect();
    let deduped_n = findings.len() - fresh.len();
    if dry {
        let payload = json!({
            "source": "comments",
            "path": scan_dir,
            "kinds": kinds,
            "found": findings.len(),
            "created": 0,
            "deduped": deduped_n,
            "dry": true,
            "findings": findings.iter().map(|f| json!({
                "file": f.file, "line": f.line, "kind": f.kind,
                "title": f.title, "note": f.note,
                "deduped": deduped(&notes, &f.note),
            })).collect::<Vec<_>>(),
        });
        if json_out {
            ops::print_json(&payload);
        } else {
            for f in &findings {
                let mark = if deduped(&notes, &f.note) { "=" } else { "+" };
                println!("{mark} {} — {}", f.note, f.title);
            }
            println!(
                "absorb comments --dry: {} found, {} would be created, {} deduped",
                findings.len(),
                fresh.len(),
                deduped_n
            );
        }
        return Ok(0);
    }
    let mut created_ids = Vec::new();
    if to == "issues" {
        let rows: Vec<ImportRow> = fresh
            .iter()
            .map(|f| ImportRow {
                title: f.title.clone(),
                status: ImportStatus::Open,
                priority: Priority::Medium,
                owner: None,
                labels: vec!["swept".to_string()],
                note: f.note.clone(),
                epic: None,
                milestone: None,
                body: None,
            })
            .collect();
        let (created, _, _, _, ids) = import_issues(store, &[], &rows, false)?;
        created_ids = ids;
        debug_assert_eq!(created, fresh.len());
    } else {
        let mut ledger = items::load_ledger(store)?;
        for f in &fresh {
            let id = store.alloc_id(ItemKind::Task).map_err(|e| e.to_string())?;
            let item = Item::new(id, &f.title)
                .status(Status::Deferred)
                .created(items::today())
                .note(&f.note);
            ledger.put_backlog(item, None);
            created_ids.push(id);
        }
        if !created_ids.is_empty() {
            items::save_ledger(store, &ledger)?;
        }
    }
    finish(
        proj,
        "comments",
        json!({
            "source": "comments",
            "path": scan_dir,
            "to": to,
            "kinds": kinds,
            "found": findings.len(),
            "created": created_ids.len(),
            "deduped": deduped_n,
            "dry": false,
            "items": created_ids,
        }),
        created_ids.len(),
        json_out,
        no_commit,
    )
}

// ---------- csv rows ----------

/// The row shape both import formats reduce to: lowercased header
/// name → every non-empty cell under it.
type Rows = Vec<BTreeMap<String, Vec<String>>>;

/// Header-name-tolerant CSV: rows are `lowercased header → values`
/// (repeated headers like Jira's `Labels` merge). First row is
/// always a header. A UTF-8 BOM is stripped (Jira exports carry
/// one; it would land inside the first header's name) and headers
/// are case/whitespace-folded, so `Issue KEY`, ` issue key ` and
/// `Issue Key` all address the same column. A structurally
/// malformed file errors naming the file (and line when csv
/// reports one); a *row* that can't map (no key/title) is the
/// caller's skipped-warning, not a parse failure.
fn read_csv(path: &Path) -> Result<Rows, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let src = src.trim_start_matches('\u{feff}');
    let mut rdr = csv::ReaderBuilder::new()
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(src.as_bytes());
    let headers: Vec<String> = rdr
        .headers()
        .map_err(|e| format!("{} headers: {e}", path.display()))?
        .iter()
        .map(|h| h.trim().to_lowercase())
        .collect();
    let mut rows = Vec::new();
    for rec in rdr.records() {
        // a record-level failure is file-malformation — error out
        // naming the file (csv's message carries line/record when
        // it tracked a position)
        let rec = rec.map_err(|e| format!("{}: {e}", path.display()))?;
        let mut row: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (i, cell) in rec.iter().enumerate() {
            if let Some(h) = headers.get(i) {
                if !h.is_empty() && !cell.trim().is_empty() {
                    row.entry(h.clone()).or_default().push(cell.to_string());
                }
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

/// The first non-empty value across `aliases` (case-folded at
/// read time).
fn col<'a>(row: &'a BTreeMap<String, Vec<String>>, aliases: &[&str]) -> Option<&'a str> {
    for a in aliases {
        if let Some(vals) = row.get(*a) {
            if let Some(v) = vals.iter().map(|v| v.trim()).find(|v| !v.is_empty()) {
                return Some(v);
            }
        }
    }
    None
}

/// Every non-empty value across `aliases` (for repeated columns
/// like Jira `Labels`).
fn col_all(row: &BTreeMap<String, Vec<String>>, aliases: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for a in aliases {
        if let Some(vals) = row.get(*a) {
            out.extend(vals.iter().filter(|v| !v.trim().is_empty()).cloned());
        }
    }
    out
}

/// Jira-ish status words → the machine (`done-ish` marked for the
/// skip/archive rule). Unknown → `Open`. Real exports seen in the
/// wild and how they map: "Selected for Development" is the
/// standard pre-sprint state → open; "In Review"/"Review" is
/// active work → in-progress; "Cancelled"/"Declined"/"Won't Fix"/
/// "Not Planned" are terminal-but-not-shipped → done-ish (skipped
/// unless `--include-done`, then archived).
fn import_status(raw: &str) -> ImportStatus {
    match raw.trim().to_lowercase().as_str() {
        "to do" | "open" | "new" | "backlog" | "todo" | "reopened" | "selected for development" => {
            ImportStatus::Open
        }
        "in progress" | "in-progress" | "doing" | "started" | "in review" | "review" => {
            ImportStatus::InProgress
        }
        "blocked" | "on hold" | "waiting" => ImportStatus::Blocked,
        "done" | "resolved" | "closed" | "complete" | "completed" | "fixed" | "cancelled"
        | "canceled" | "declined" | "won't fix" | "wontfix" | "not planned" | "not_planned" => {
            ImportStatus::Done
        }
        _ => ImportStatus::Open,
    }
}

/// Jira-ish priority words → [`Priority`]. Unknown → `Medium`.
fn import_priority(raw: &str) -> Priority {
    match raw.trim().to_lowercase().as_str() {
        "highest" | "critical" | "blocker" | "crit" | "p0" => Priority::Critical,
        "high" | "major" | "p1" => Priority::High,
        "low" | "lowest" | "trivial" | "minor" | "p3" => Priority::Low,
        _ => Priority::Medium,
    }
}

// ---------- jira ----------

/// A Jira CSV row → [`ImportRow`]. `None` when the row carries no
/// title and no key — the caller reports it as skipped. Optional
/// columns (`Priority`, `Assignee`, `Labels`, epic, milestone)
/// simply degrade to defaults when the export lacks them.
fn jira_row(row: &BTreeMap<String, Vec<String>>) -> Option<ImportRow> {
    let key = col(row, &["issue key", "key", "issue id", "issuekey", "issue"]);
    let title = col(row, &["summary", "title"]);
    let title = clean_title(title.or(key).unwrap_or_default());
    if title.is_empty() {
        return None;
    }
    let marker = key.unwrap_or(&title).to_string();
    Some(ImportRow {
        title,
        status: col(row, &["status"]).map_or(ImportStatus::Open, import_status),
        priority: col(row, &["priority", "pri"]).map_or(Priority::Medium, import_priority),
        owner: col(row, &["assignee", "owner", "reporter"]).and_then(sanitize_actor),
        labels: labels_from(&col_all(row, &["labels", "label"])),
        note: format!("jira:{marker}"),
        // Jira's epic column has shipped under many names
        epic: col(
            row,
            &["epic link", "epic name", "epic", "parent key", "parent"],
        )
        .and_then(sanitize_group),
        milestone: col(
            row,
            &[
                "fix version/s",
                "fix versions",
                "fix version",
                "milestone",
                "sprint",
            ],
        )
        .and_then(sanitize_group),
        body: col(row, &["description", "body"]).map(str::to_string),
    })
}

fn jira(
    proj: &Project,
    store: &arca_store_md::MdStore,
    file: &Path,
    include_done: bool,
    json_out: bool,
    no_commit: bool,
) -> Result<i32, String> {
    let rows = read_csv(file)?;
    let scan = digest::scan(store)?;
    let notes = existing_notes(&scan);
    let mut imports = Vec::new();
    let mut warnings = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        match jira_row(row) {
            Some(r) => imports.push(r),
            // data rows are 1-based for humans
            None => warnings.push(format!(
                "{}: row {}: no summary or key — skipped",
                file.display(),
                i + 1
            )),
        }
    }
    for w in &warnings {
        eprintln!("absorb jira: {w}");
    }
    let (created, dup, skipped_done, archived, ids) =
        import_issues(store, &notes, &imports, include_done)?;
    finish(
        proj,
        "jira",
        json!({
            "source": "jira",
            "file": file,
            "found": imports.len(),
            "created": created,
            "deduped": dup,
            "skipped_done": skipped_done,
            "archived": archived,
            "items": ids,
            "warnings": warnings,
        }),
        created,
        json_out,
        no_commit,
    )
}

// ---------- github ----------

/// A GitHub label — `"bug"` or `{name: "bug"}`.
fn gh_label(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => sanitize_label(s),
        serde_json::Value::Object(o) => o.get("name")?.as_str().and_then(sanitize_label),
        _ => None,
    }
}

/// A GitHub login — `"devin"` or `{login: "devin"}`.
fn gh_login(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(o) => o
            .get("login")
            .or_else(|| o.get("name"))?
            .as_str()
            .map(str::to_string),
        _ => None,
    }
}

/// A GitHub issue number — `42` or `"42"`.
fn gh_number(v: Option<&serde_json::Value>) -> Option<String> {
    match v? {
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        _ => None,
    }
}

/// JSON rows → `BTreeMap` rows in the csv shape, so both formats
/// share the import path. Accepts a bare array or the GitHub API
/// list-issues wrapper (`{"issues": [...]}`, or `{"items": [...]}`
/// for search-result exports). A non-object element degrades to a
/// `warnings` entry — one bad issue shouldn't sink the file.
fn read_github_json(path: &Path) -> Result<(Rows, Vec<String>), String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v: serde_json::Value = serde_json::from_str(src.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let arr = match &v {
        serde_json::Value::Array(a) => a,
        serde_json::Value::Object(o) => o
            .get("issues")
            .or_else(|| o.get("items"))
            .and_then(|x| x.as_array())
            .ok_or_else(|| {
                format!(
                    "{}: expected an `issues`/`items` array in the JSON object",
                    path.display()
                )
            })?,
        _ => {
            return Err(format!(
                "{}: expected a JSON array of issues",
                path.display()
            ))
        }
    };
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    for (i, issue) in arr.iter().enumerate() {
        let mut row: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut put = |k: &str, v: Option<String>| {
            if let Some(v) = v.filter(|s| !s.trim().is_empty()) {
                row.entry(k.to_string()).or_default().push(v);
            }
        };
        let Some(obj) = issue.as_object() else {
            warnings.push(format!(
                "{}: issue #{i} isn't a JSON object — skipped",
                path.display()
            ));
            continue;
        };
        put(
            "key",
            gh_number(obj.get("number").or_else(|| obj.get("id"))),
        );
        put(
            "title",
            obj.get("title")
                .and_then(|t| t.as_str())
                .map(str::to_string),
        );
        // `state` is canonical; the API also reports `state_reason`
        // (`completed`/`not_planned`) and `gh issue list --json`
        // camelCases it — any of them can be the only signal
        put(
            "state",
            ["state", "state_reason", "stateReason"]
                .iter()
                .find_map(|k| obj.get(*k).and_then(|t| t.as_str()))
                .map(str::to_string),
        );
        for alias in ["labels", "assignee", "assignees", "epic", "milestone"] {
            if let Some(v) = obj.get(alias) {
                match v {
                    serde_json::Value::Array(items) => {
                        for it in items {
                            if alias == "labels" {
                                put("labels", gh_label(it));
                            } else {
                                put(
                                    alias,
                                    gh_login(it).or_else(|| it.as_str().map(str::to_string)),
                                );
                            }
                        }
                    }
                    serde_json::Value::Null => {}
                    serde_json::Value::String(s) => put(alias, Some(s.clone())),
                    serde_json::Value::Object(_) => {
                        if alias == "labels" {
                            put("labels", gh_label(v));
                        } else if alias == "milestone" {
                            // GH milestones: {title: "1.0"}
                            put(
                                "milestone",
                                v.get("title")
                                    .or_else(|| v.get("name"))
                                    .and_then(|t| t.as_str())
                                    .map(str::to_string),
                            );
                        } else {
                            put(alias, gh_login(v));
                        }
                    }
                    other => put(alias, Some(other.to_string())),
                }
            }
        }
        put(
            "priority",
            obj.get("priority")
                .and_then(|t| t.as_str())
                .map(str::to_string),
        );
        put(
            "body",
            obj.get("body").and_then(|t| t.as_str()).map(str::to_string),
        );
        rows.push(row);
    }
    Ok((rows, warnings))
}

/// A GitHub row (JSON-reduced or CSV) → [`ImportRow`]. `None` when
/// it carries no title and no number — reported as skipped.
fn github_row(row: &BTreeMap<String, Vec<String>>) -> Option<ImportRow> {
    let key = col(row, &["key", "number", "id", "issue", "issue number"]);
    let title = col(row, &["title", "summary"]);
    let title = clean_title(title.or(key).unwrap_or_default());
    if title.is_empty() {
        return None;
    }
    let marker = key.unwrap_or(&title).to_string();
    let state = col(row, &["state", "status", "state_reason", "statereason"]).map_or(
        ImportStatus::Open,
        |s| match s.trim().to_lowercase().as_str() {
            "open" => ImportStatus::Open,
            "closed" | "done" | "completed" => ImportStatus::Done,
            other => import_status(other),
        },
    );
    Some(ImportRow {
        title,
        status: state,
        priority: col(row, &["priority", "pri"]).map_or(Priority::Medium, import_priority),
        owner: col(row, &["assignee", "assignees", "owner"]).and_then(sanitize_actor),
        labels: labels_from(&col_all(row, &["labels", "label"])),
        note: format!("github:{marker}"),
        epic: col(row, &["epic"]).and_then(sanitize_group),
        milestone: col(row, &["milestone"]).and_then(sanitize_group),
        body: col(row, &["body", "description"]).map(str::to_string),
    })
}

fn github(
    proj: &Project,
    store: &arca_store_md::MdStore,
    file: &Path,
    include_done: bool,
    json_out: bool,
    no_commit: bool,
) -> Result<i32, String> {
    let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
    let (rows, mut warnings) = if ext == "json" {
        read_github_json(file)?
    } else if ext == "csv" {
        (read_csv(file)?, Vec::new())
    } else {
        // sniff: JSON starts with `[` (bare array) or `{` (wrapper)
        let src = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
        if src.trim_start().starts_with(['[', '{']) {
            read_github_json(file)?
        } else {
            (read_csv(file)?, Vec::new())
        }
    };
    let scan = digest::scan(store)?;
    let notes = existing_notes(&scan);
    let mut imports = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        match github_row(row) {
            Some(r) => imports.push(r),
            None => warnings.push(format!(
                "{}: row {}: no title or number — skipped",
                file.display(),
                i + 1
            )),
        }
    }
    for w in &warnings {
        eprintln!("absorb github: {w}");
    }
    let (created, dup, skipped_done, archived, ids) =
        import_issues(store, &notes, &imports, include_done)?;
    finish(
        proj,
        "github",
        json!({
            "source": "github",
            "file": file,
            "found": imports.len(),
            "created": created,
            "deduped": dup,
            "skipped_done": skipped_done,
            "archived": archived,
            "items": ids,
            "warnings": warnings,
        }),
        created,
        json_out,
        no_commit,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn rg_line_parses() {
        assert_eq!(
            parse_rg_line("src/x.rs:12:// TODO fix"),
            Some(("src/x.rs", 12, "// TODO fix"))
        );
        assert_eq!(
            parse_rg_line("./a/b:3:x FIXME y:z"),
            Some(("./a/b", 3, "x FIXME y:z"))
        );
        assert!(parse_rg_line("no colon").is_none());
        assert!(parse_rg_line("a:bc:todo").is_none()); // non-numeric line
        assert!(parse_rg_line("a:1").is_none());
    }

    #[test]
    fn comment_titles() {
        let kinds = vec!["TODO".to_string(), "FIXME".to_string()];
        assert_eq!(
            comment_title("// TODO: fix the thing", &kinds),
            "fix the thing"
        );
        assert_eq!(comment_title("# TODO(devin): wire it", &kinds), "wire it");
        assert_eq!(
            comment_title("x = 1 // FIXME later — bad", &kinds),
            "later — bad"
        );
        // earliest kind wins
        assert_eq!(
            comment_title("FIXME and TODO here", &kinds),
            "and TODO here"
        );
        // `·` is scrubbed, ws collapsed, long titles truncated
        assert_eq!(comment_title("TODO a · dot", &kinds), "a - dot");
        let long = format!("TODO {}", "x".repeat(100));
        assert_eq!(comment_title(&long, &kinds).chars().count(), 70);
        // no kind present → the whole text cleaned
        assert_eq!(comment_title("plain text", &kinds), "plain text");
    }

    #[test]
    fn sanitizers() {
        assert_eq!(sanitize_group("Q3 Launch!"), Some("Q3-Launch".into()));
        assert_eq!(sanitize_group("1.0 beta"), Some("1.0-beta".into()));
        assert_eq!(sanitize_group("…"), None);
        assert_eq!(
            sanitize_label("good first issue"),
            Some("good-first-issue".into())
        );
        assert_eq!(
            sanitize_actor("Luis Figueiredo").unwrap().as_str(),
            "luis-figueiredo"
        );
        assert_eq!(sanitize_actor("devin").unwrap().as_str(), "devin");
        assert!(sanitize_actor(" ").is_none());
        assert_eq!(
            labels_from(&["bug,help wanted".to_string(), "ui".to_string()]),
            vec![
                "bug".to_string(),
                "help-wanted".to_string(),
                "ui".to_string()
            ]
        );
    }

    #[test]
    fn dedup_markers_are_boundary_checked() {
        let notes = vec!["src/a.rs:10".to_string(), "jira:PROJ-100".to_string()];
        assert!(!deduped(&notes, "src/a.rs:1"), ":1 ≠ :10");
        assert!(!deduped(&notes, "jira:PROJ-10"));
        assert!(deduped(&notes, "src/a.rs:10"));
        assert!(deduped(&notes, "jira:PROJ-100"));
        // marker embedded in a longer note still matches
        let notes = vec!["moved from jira:PROJ-7 upstream".to_string()];
        assert!(deduped(&notes, "jira:PROJ-7"));
        assert!(!deduped(&notes, "jira:PROJ-70"));
        // a real hit later in the same note counts even when a
        // fused prefix shadowed the first occurrence
        let notes = vec!["src/a.rs:10 also src/a.rs:1".to_string()];
        assert!(deduped(&notes, "src/a.rs:1"));
    }

    #[test]
    fn status_and_priority_maps() {
        assert_eq!(import_status("To Do"), ImportStatus::Open);
        assert_eq!(import_status("In Progress"), ImportStatus::InProgress);
        assert_eq!(import_status("Blocked"), ImportStatus::Blocked);
        assert_eq!(import_status("Resolved"), ImportStatus::Done);
        assert_eq!(import_status("whatever"), ImportStatus::Open);
        assert_eq!(import_priority("Highest"), Priority::Critical);
        assert_eq!(import_priority("medium"), Priority::Medium);
        assert_eq!(import_priority("Lowest"), Priority::Low);
        assert_eq!(import_priority("???"), Priority::Medium);
    }

    #[test]
    fn gh_json_shapes() {
        let v = serde_json::json!({"name": "bug"});
        assert_eq!(gh_label(&v).as_deref(), Some("bug"));
        assert_eq!(gh_label(&serde_json::json!("ui")).as_deref(), Some("ui"));
        assert_eq!(gh_label(&serde_json::json!(3)), None);
        assert_eq!(
            gh_login(&serde_json::json!({"login": "devin"})).as_deref(),
            Some("devin")
        );
        assert_eq!(gh_login(&serde_json::json!("x")).as_deref(), Some("x"));
        assert_eq!(
            gh_number(Some(&serde_json::json!(42))).as_deref(),
            Some("42")
        );
        assert_eq!(
            gh_number(Some(&serde_json::json!("7"))).as_deref(),
            Some("7")
        );
        assert_eq!(gh_number(None), None);
        // non-scalar shapes → None
        assert_eq!(gh_login(&serde_json::json!(5)), None);
        assert_eq!(gh_number(Some(&serde_json::json!(true))), None);
        assert_eq!(gh_login(&serde_json::json!({})), None);
    }

    #[test]
    fn comment_title_paren_edges() {
        let kinds = vec!["TODO".to_string()];
        // unclosed attribution paren — kept as text
        assert_eq!(
            comment_title("// TODO(unclosed: thing", &kinds),
            "(unclosed: thing"
        );
        // paren closed but not followed by `:` or whitespace — the
        // `(x)` stays part of the title
        assert_eq!(comment_title("TODO(x)y", &kinds), "(x)y");
    }

    #[test]
    fn labels_dedup_and_csv_ragged() {
        // a repeated label isn't inserted twice, junk pieces drop
        assert_eq!(
            labels_from(&["bug,bug;bug,*,,ui".to_string()]),
            vec!["bug".to_string(), "ui".to_string()]
        );
        // col(): no alias present, and all-empty values → None
        let mut row = std::collections::BTreeMap::new();
        assert_eq!(col(&row, &["x", "y"]), None);
        row.insert("x".to_string(), vec!["  ".to_string()]);
        assert_eq!(col(&row, &["x", "y"]), None, "whitespace-only");
        row.insert("x".to_string(), vec!["".to_string(), "v".to_string()]);
        assert_eq!(col(&row, &["x", "y"]), Some("v"));
        // read_csv: ragged rows + empty cells are skipped
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("x.csv");
        std::fs::write(&f, "a,b,c\n1,,3\n1,2,3,4\n").unwrap();
        let rows = read_csv(&f).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get("b"), None, "empty cell not collected");
        assert!(rows[0]["a"].contains(&"1".to_string()));
        // 4th cell has no header → dropped
        assert_eq!(rows[1].values().flatten().count(), 3);
        // missing file → error
        assert!(read_csv(&d.path().join("nope.csv")).is_err());
    }

    #[test]
    fn github_json_rows_all_shapes() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("issues.json");
        std::fs::write(
            &f,
            r#"[
                {"number": 7, "title": "one", "state": "open",
                 "labels": [{"name": "bug"}, "ui", 5],
                 "assignees": [{"login": "a"}, {"name": "b"}, "c", 5],
                 "assignee": {"login": "d"}, "epic": "v2",
                 "milestone": {"title": "1.0"}, "priority": "high",
                 "body": "text"},
                {"id": "9", "title": "two", "state": "closed",
                 "labels": "solo", "milestone": "2.0", "epic": 42},
            {"number": 8, "title": "labels-obj", "state": "open",
                 "labels": {"name": "solitary"}, "milestone": null}
            ]"#,
        )
        .unwrap();
        let (rows, warnings) = read_github_json(&f).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["key"], vec!["7".to_string()]);
        assert_eq!(rows[0]["labels"], vec!["bug".to_string(), "ui".to_string()]);
        assert_eq!(
            rows[0]["assignees"],
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        assert_eq!(rows[0]["assignee"], vec!["d".to_string()]);
        assert_eq!(rows[0]["milestone"], vec!["1.0".to_string()]);
        assert_eq!(rows[0]["body"], vec!["text".to_string()]);
        // string label + scalar milestone/epic fall through
        assert_eq!(rows[1]["labels"], vec!["solo".to_string()]);
        assert_eq!(rows[1]["epic"], vec!["42".to_string()]);
        // malformed inputs error
        std::fs::write(&f, "not json").unwrap();
        assert!(read_github_json(&f).is_err());
        std::fs::write(&f, "{\"a\":1}").unwrap();
        assert!(read_github_json(&f).is_err());
        std::fs::write(&f, "5").unwrap();
        assert!(read_github_json(&f).is_err());
    }

    #[test]
    fn csv_bom_multiline_and_header_tolerance() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("j.csv");
        // BOM + uppercase + padded headers; a quoted multi-line
        // summary and a second, ragged row
        std::fs::write(
            &f,
            "\u{feff}  Issue KEY , SUMMARY ,Status\nPROJ-1,\"line one\nline two\",Done\nPROJ-2,plain\n",
        )
        .unwrap();
        let rows = read_csv(&f).unwrap();
        assert_eq!(rows.len(), 2);
        // the BOM didn't corrupt the first header
        assert_eq!(rows[0]["issue key"], vec!["PROJ-1".to_string()]);
        assert_eq!(rows[0]["summary"], vec!["line one\nline two".to_string()]);
        assert_eq!(rows[1]["issue key"], vec!["PROJ-2".to_string()]);
        // clean_title folds the newline for the ledger title
        let r = jira_row(&rows[0]).unwrap();
        assert_eq!(r.title, "line one line two");
        // optional columns absent → defaults, no error
        assert_eq!(r.priority, Priority::Medium);
        assert!(r.owner.is_none() && r.labels.is_empty());
        assert!(r.epic.is_none() && r.milestone.is_none());
    }

    #[test]
    fn csv_malformed_file_errors_cleanly() {
        let d = tempfile::tempdir().unwrap();
        // invalid UTF-8 → a clean error naming the file
        let f = d.path().join("bad.csv");
        std::fs::write(&f, b"Issue key,Summary\nPROJ-1,\xff\xfe\n").unwrap();
        let e = read_csv(&f).unwrap_err();
        assert!(e.contains("bad.csv"), "{e}");
        // a missing file is still a hard error naming the file
        let e = read_csv(&d.path().join("nope.csv")).unwrap_err();
        assert!(e.contains("nope.csv"), "{e}");
    }

    #[test]
    fn jira_row_aliases_and_status_synonyms() {
        let mut row = BTreeMap::new();
        let put = |r: &mut BTreeMap<String, Vec<String>>, k: &str, v: &str| {
            r.insert(k.to_string(), vec![v.to_string()]);
        };
        put(&mut row, "parent key", "EPIC-9");
        put(&mut row, "fix version/s", "2.4");
        put(&mut row, "issue", "PROJ-7");
        put(&mut row, "status", "Selected for Development");
        let r = jira_row(&row).unwrap();
        assert_eq!(r.note, "jira:PROJ-7");
        assert_eq!(r.epic.as_deref(), Some("EPIC-9"));
        assert_eq!(r.milestone.as_deref(), Some("2.4"));
        assert_eq!(r.status, ImportStatus::Open);
        // "Epic Name" variant + done-ish synonyms
        let mut row = BTreeMap::new();
        put(&mut row, "epic name", "launch");
        put(&mut row, "key", "P-2");
        put(&mut row, "status", "Won't Fix");
        let r = jira_row(&row).unwrap();
        assert_eq!(r.epic.as_deref(), Some("launch"));
        assert_eq!(r.status, ImportStatus::Done);
        // no summary and no key → skipped
        let row = BTreeMap::new();
        assert!(jira_row(&row).is_none());
    }

    #[test]
    fn github_json_wrapper_shapes_and_state_reason() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("issues.json");
        // API list wrapper + non-object element (warning, not error)
        // + state_reason-only + body absent
        std::fs::write(
            &f,
            r#"{"issues": [
                {"number": 1, "title": "one", "state_reason": "not_planned"},
                "oops",
                {"number": 2, "title": "two", "stateReason": "completed"},
                {"number": 3, "title": "three"}
            ]}"#,
        )
        .unwrap();
        let (rows, warnings) = read_github_json(&f).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("issue #1"), "{warnings:?}");
        assert_eq!(github_row(&rows[0]).unwrap().status, ImportStatus::Done);
        assert_eq!(github_row(&rows[1]).unwrap().status, ImportStatus::Done);
        assert_eq!(github_row(&rows[2]).unwrap().status, ImportStatus::Open);
        // {"items": [...]} search-result wrapper
        std::fs::write(&f, r#"{"items": [{"number": 4, "title": "four"}]}"#).unwrap();
        let (rows, _) = read_github_json(&f).unwrap();
        assert_eq!(rows.len(), 1);
        // an object without a recognizable array → clean error
        std::fs::write(&f, r#"{"data": []}"#).unwrap();
        let e = read_github_json(&f).unwrap_err();
        assert!(e.contains("issues"), "{e}");
    }
}

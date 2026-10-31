//! `arca org status|board` — cross-repo views over the index.
//!
//! Every registered repo's snapshot is checked for freshness
//! (stored `head_commit` vs the worktree's current HEAD); a stale
//! or missing snapshot is rebuilt on the spot — org reads never
//! serve old data silently. Repos whose worktree vanished keep
//! their last snapshot and report an error inline.

use std::collections::BTreeMap;

use arca_core::Status;
use arca_index::{Index, RepoSnapshot};
use clap::Subcommand;
use serde_json::json;

use crate::{board, digest, indexcmd, ops};

#[derive(Subcommand)]
pub enum Cmd {
    /// Per-repo counts + org totals + items grouped by owner.
    Status {
        /// Show repos with no live items too.
        #[arg(long)]
        all: bool,
    },
    /// Org kanban — repo-prefixed cards over every live item.
    Board {
        /// Emit a markdown table (for docs/READMEs).
        #[arg(long)]
        md: bool,
        /// Show every card — no row cap.
        #[arg(long)]
        all: bool,
        /// Only this repo's items (name or registered path).
        #[arg(long)]
        repo: Option<String>,
    },
    /// Re-run `init` on every registered repo — refreshes the
    /// worktree scaffold and root files (.gitignore, the AGENTS.md
    /// agent-awareness block). The rollout path for new scaffolding.
    Init,
}

/// One repo's snapshot load outcome for org views.
pub struct RepoView {
    /// The snapshot (may be the last good one on rescan failure).
    pub snap: Option<RepoSnapshot>,
    /// Rescan/load error, when the fresh snapshot couldn't be had.
    pub error: Option<String>,
    /// The snapshot was rebuilt during this read.
    pub rebuilt: bool,
}

/// A scan-shaped adapter so `digest::counts` runs on snapshots.
fn as_scan(s: &RepoSnapshot) -> digest::Scan {
    digest::Scan {
        name: s.name.clone(),
        items: s.items.clone(),
        archived: s.archived.clone(),
    }
}

/// Load every registered repo's snapshot, auto-rebuilding stale
/// ones (head mismatch or missing). The per-repo `git`/fs work is
/// parallel — redb opens lock the file, so index I/O stays serial.
pub fn load_fresh(index: &impl Index) -> Result<Vec<RepoView>, String> {
    let repos = index.repos().map_err(|e| e.to_string())?;
    let roots: Vec<std::path::PathBuf> = repos
        .iter()
        .map(|r| r.path.as_std_path().to_path_buf())
        .collect();
    let heads = indexcmd::par_map(&roots, |r| indexcmd::current_head(r));
    let mut cached = Vec::with_capacity(repos.len());
    for r in &repos {
        cached.push(index.snapshot(&r.path).map_err(|e| e.to_string())?);
    }
    let fresh =
        |i: usize| matches!((&cached[i], &heads[i]), (Some(s), Some(h)) if s.head_commit == *h);
    let stale: Vec<usize> = (0..repos.len()).filter(|i| !fresh(*i)).collect();
    let scans = indexcmd::par_map(&stale, |&i| indexcmd::scan_one(&roots[i]));
    let mut rebuilt: std::collections::BTreeMap<usize, Result<RepoSnapshot, String>> =
        stale.iter().copied().zip(scans).collect();
    let mut out = Vec::with_capacity(repos.len());
    for (i, c) in cached.iter().enumerate() {
        if fresh(i) {
            out.push(RepoView {
                snap: c.clone(),
                error: None,
                rebuilt: false,
            });
            continue;
        }
        match rebuilt
            .remove(&i)
            .unwrap_or_else(|| Err("rescan lost".into()))
        {
            Ok(snap) => {
                index.save_snapshot(&snap).map_err(|e| e.to_string())?;
                out.push(RepoView {
                    snap: Some(snap),
                    error: None,
                    rebuilt: true,
                });
            }
            Err(e) => out.push(RepoView {
                snap: cached[i].clone(),
                error: Some(e),
                rebuilt: false,
            }),
        }
    }
    Ok(out)
}

/// `owner → item refs` across all snapshots — active items only.
fn by_owner<'a>(snaps: impl Iterator<Item = &'a RepoSnapshot>) -> BTreeMap<String, Vec<String>> {
    let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for s in snaps {
        for i in s
            .live_items()
            .filter(|i| i.status != Status::Done && i.status != Status::Deferred)
        {
            if let Some(o) = &i.owner {
                m.entry(o.as_str().to_string())
                    .or_default()
                    .push(format!("{}:{} {}", s.name, i.id, i.title));
            }
        }
    }
    for v in m.values_mut() {
        v.sort();
    }
    m
}

/// Cap on items listed per owner in `org status` — the section is
/// a pointer to where work sits, not a full listing.
const OWNER_CAP: usize = 8;

fn status(views: &[RepoView], all: bool, json_out: bool) -> Result<i32, String> {
    let mut total = digest::Counts::default();
    let mut repos_json = Vec::new();
    let mut snaps = Vec::new();
    for v in views {
        let Some(s) = &v.snap else {
            repos_json.push(json!({"error": v.error}));
            continue;
        };
        snaps.push(s);
        let c = digest::counts(&as_scan(s));
        total.add(&c);
        repos_json.push(json!({
            "name": s.name,
            "path": s.path,
            "head_commit": s.head_commit,
            "rebuilt": v.rebuilt,
            "error": v.error,
            "counts": c.to_json(),
        }));
    }
    let owners = by_owner(snaps.iter().copied());
    if json_out {
        ops::print_json(&json!({
            "repos": repos_json,
            "totals": total.to_json(),
            "owners": owners,
        }));
        return Ok(0);
    }
    let mut out = format!("org status — {} repo(s)\n", views.len());
    let mut idle = 0usize;
    for v in views {
        // a repo with a clean snapshot but no live items is idle —
        // hundreds of registered repos bury the interesting ones;
        // errored repos always surface
        let idle_repo = v.error.is_none()
            && v.snap
                .as_ref()
                .is_some_and(|s| s.live_items().next().is_none());
        if idle_repo && !all {
            idle += 1;
            continue;
        }
        match &v.snap {
            Some(s) => {
                let c = digest::counts(&as_scan(s));
                let head = if s.head_commit.is_empty() {
                    "gone".to_string()
                } else {
                    s.head_commit[..s.head_commit.len().min(7)].to_string()
                };
                let mut line = format!("{:<16} {}  @{head}", s.name, c.oneline());
                if v.rebuilt {
                    line.push_str("  (rebuilt)");
                }
                if let Some(e) = &v.error {
                    line.push_str(&format!("  ⚠ {e}"));
                }
                out.push_str(&line);
                out.push('\n');
            }
            None => out.push_str(&format!(
                "{:<16} ⚠ {}\n",
                "?",
                v.error.as_deref().unwrap_or("unavailable")
            )),
        }
    }
    if idle > 0 {
        out.push_str(&format!("… +{idle} idle repos (`--all` to show)\n"));
    }
    out.push_str(&format!("totals: {}\n", total.oneline()));
    if !owners.is_empty() {
        out.push_str("by owner:\n");
        for (o, items) in &owners {
            let more = items.len().saturating_sub(OWNER_CAP);
            let shown: Vec<&str> = items.iter().take(OWNER_CAP).map(String::as_str).collect();
            let suffix = if more > 0 {
                format!(", … +{more} more")
            } else {
                String::new()
            };
            out.push_str(&format!("  {o}: {}{}\n", shown.join(", "), suffix));
        }
    }
    ops::print_paged(out.trim_end());
    Ok(0)
}

fn board_view(
    views: &[RepoView],
    md: bool,
    all: bool,
    repo: Option<String>,
    json_out: bool,
) -> Result<i32, String> {
    // `--repo` matches a snapshot by name or registered path
    let wanted = |s: &RepoSnapshot| repo.as_deref().is_none_or(|r| s.name == r || s.path == r);
    if repo.is_some() && !views.iter().any(|v| v.snap.as_ref().is_some_and(&wanted)) {
        return Err(format!(
            "no repo {:?} in the index — `arca index list`",
            repo.unwrap_or_default()
        ));
    }
    if json_out {
        // columns map with a `repo` tag on each item
        let mut merged = serde_json::Map::new();
        for (name, _) in board::COLUMNS {
            merged.insert(name.to_string(), json!([]));
        }
        for v in views {
            let Some(s) = &v.snap else { continue };
            if !wanted(s) {
                continue;
            }
            let cols = board::columns_json(s.live_items(), Some(&s.name));
            for (name, _) in board::COLUMNS {
                let mut dst = merged[name].as_array().cloned().unwrap_or_default();
                dst.extend(cols[name].as_array().cloned().unwrap_or_default());
                merged.insert(name.to_string(), json!(dst));
            }
        }
        ops::print_json(&serde_json::Value::Object(merged));
        return Ok(0);
    }
    // human board: one merged column set, repo-prefixed cards
    let tagged = views.iter().flat_map(|v| {
        v.snap.iter().filter(|s| wanted(s)).flat_map(|s| {
            s.live_items()
                .map(move |i| (format!("{}:{}", s.name, i.id), i))
        })
    });
    let cards = board::cards_by_column(tagged.map(|(id, i)| {
        let label = if i.title.is_empty() {
            id
        } else {
            format!("{id} {}", i.title)
        };
        (label, i)
    }));
    if cards.iter().all(Vec::is_empty) {
        println!("org board — nothing live across {} repo(s)", views.len());
    } else {
        ops::print_paged(&board::render_cards(&cards, md, board::Fit::terminal(all)));
    }
    Ok(0)
}

/// `init` over every registered repo — per-repo outcomes, errors
/// collected (one bad repo doesn't stop the rollout).
fn init_all(index: &impl Index, json_out: bool, no_commit: bool) -> Result<i32, String> {
    let repos = index.repos().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut failures = 0usize;
    for r in &repos {
        let p = r.path.as_std_path().to_path_buf();
        match crate::init::ensure(None, Some(&p), no_commit) {
            Ok(o) => out.push(json!({
                "path": r.path,
                "files_created": o.files_created,
                "gitignore_updated": o.gitignore_updated,
                "agents_md_updated": o.agents_md_updated,
                "committed": o.committed,
            })),
            Err(e) => {
                failures += 1;
                out.push(json!({"path": r.path, "error": e}));
            }
        }
    }
    if json_out {
        ops::print_json(&json!({"repos": out, "failures": failures}));
        return Ok(if failures == 0 { 0 } else { 1 });
    }
    for (r, o) in repos.iter().zip(&out) {
        if let Some(e) = o.get("error").and_then(|v| v.as_str()) {
            println!("{:<24} ⚠ {e}", r.path.as_str());
        } else {
            let mut notes = Vec::new();
            if o["gitignore_updated"].as_bool().unwrap_or(false) {
                notes.push(".gitignore");
            }
            if o["agents_md_updated"].as_bool().unwrap_or(false) {
                notes.push("AGENTS.md");
            }
            let n = o["files_created"].as_array().map_or(0, Vec::len);
            if n > 0 {
                notes.push("scaffold");
            }
            println!(
                "{:<24} {}",
                r.path.as_str(),
                if notes.is_empty() {
                    "already current".to_string()
                } else {
                    format!("updated {}", notes.join(" + "))
                }
            );
        }
    }
    Ok(if failures == 0 { 0 } else { 1 })
}

/// The `org` verb group.
pub fn run(cmd: Cmd, json_out: bool, no_commit: bool) -> Result<i32, String> {
    let index = indexcmd::open_index()?;
    if let Cmd::Init = cmd {
        return init_all(&index, json_out, no_commit);
    }
    let views = load_fresh(&index)?;
    match cmd {
        Cmd::Status { all } => status(&views, all, json_out),
        Cmd::Board { md, all, repo } => board_view(&views, md, all, repo, json_out),
        Cmd::Init => unreachable!(),
    }
}

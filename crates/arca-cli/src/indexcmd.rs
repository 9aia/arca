//! `arca index add|remove|list|rebuild` — the derived-cache
//! registry verbs.
//!
//! `add` validates the repo has a `project/` worktree (same rules
//! as discovery), registers it, and takes a first snapshot.
//! `rebuild` re-scans via `MdStore` and re-keys the snapshot on the
//! project-branch HEAD — freshness afterwards is the trivial
//! `stored head == current head` check `org` does before trusting
//! a snapshot.

use std::path::{Path, PathBuf};

use arca_index::{Index, RepoEntry, RepoSnapshot};
use arca_index_redb::RedbIndex;
use camino::Utf8PathBuf;
use clap::Subcommand;
use serde_json::json;

use crate::{digest, discover, ops};

#[derive(Subcommand)]
pub enum Cmd {
    /// Register a repo — `<path>` is a repo root (or its
    /// `project/` worktree). Takes an initial snapshot.
    Add {
        /// Repo root (or project worktree) to register.
        path: PathBuf,
    },
    /// Unregister a repo and drop its snapshot.
    Remove {
        /// Registered repo root (or project worktree).
        path: PathBuf,
    },
    /// List registered repos with snapshot freshness.
    List,
    /// Re-scan a registered repo (all of them without `<path>`).
    Rebuild {
        /// Registered repo root (or project worktree).
        path: Option<PathBuf>,
    },
}

/// Open the default index (`$ARCA_HOME` / `~/.local/share/arca`).
pub fn open_index() -> Result<RedbIndex, String> {
    RedbIndex::open().map_err(|e| e.to_string())
}

/// RFC 3339 now — `scanned_at`.
pub fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".into())
}

/// `<path>` → the repo root + project worktree. Accepts a repo
/// root (`<path>/project` must qualify) or the worktree itself.
/// `--dir` discovery doesn't qualify its path, so the check is
/// repeated here — `index add` on a plain dir must fail.
pub fn repo_at(path: &Path) -> Result<discover::Project, String> {
    if let Ok(p) = discover::discover(None, Some(path)) {
        return Ok(p);
    }
    match discover::discover(Some(path), None) {
        Ok(p) if discover::qualifies(&p.worktree) => Ok(p),
        _ => Err(format!(
            "{}: not a arca repo — needs a project/ worktree",
            path.display()
        )),
    }
}

/// Scan a repo's worktree into a snapshot.
pub fn scan_repo(proj: &discover::Project) -> Result<RepoSnapshot, String> {
    let store = ops::store_at(&proj.worktree)?;
    let scan = digest::scan(&store)?;
    let path = Utf8PathBuf::from_path_buf(proj.root.clone())
        .map_err(|p| format!("{}: non-utf8 path", p.display()))?;
    Ok(RepoSnapshot {
        path,
        name: scan.name,
        head_commit: arca_git::head_commit(&proj.worktree).unwrap_or_default(),
        scanned_at: now(),
        items: scan.items,
        archived: scan.archived,
    })
}

/// Freshness check — `None` when the repo isn't registered or its
/// worktree is gone. Used by `index list` and `org`'s
/// auto-rebuild.
pub fn current_head(repo_root: &Path) -> Option<String> {
    arca_git::head_commit(&repo_root.join(discover::DIR))
}

/// Map `items` across scoped worker threads, preserving order —
/// for the per-repo `git`/fs calls that dominate `org`/`index`
/// latency. Keep redb I/O out of `f`: the index locks per open.
pub fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let workers = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(16)
        .min(items.len().max(1));
    let next = AtomicUsize::new(0);
    let (tx, rx) = std::sync::mpsc::channel::<(usize, R)>();
    std::thread::scope(|s| {
        for _ in 0..workers {
            let tx = tx.clone();
            let (next, f) = (&next, &f);
            s.spawn(move || loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= items.len() {
                    return;
                }
                let _ = tx.send((i, f(&items[i])));
            });
        }
        drop(tx);
        let mut out: Vec<Option<R>> = items.iter().map(|_| None).collect();
        for (i, r) in rx {
            out[i] = Some(r);
        }
        out.into_iter()
            .map(|o| o.unwrap_or_else(|| unreachable!("par_map hole")))
            .collect()
    })
}

/// Re-scan a registered repo root without the redb write — fs +
/// git only, safe to run in parallel. Callers persist via
/// `index.save_snapshot` serially (the index locks per open).
pub fn scan_one(root: &Path) -> Result<RepoSnapshot, String> {
    let proj = repo_at(root)?;
    scan_repo(&proj)
}

fn registered_root(index: &impl Index, path: &Path) -> Result<PathBuf, String> {
    let proj = repo_at(path)?;
    let canon = proj
        .root
        .canonicalize()
        .map_err(|e| format!("{}: {e}", proj.root.display()))?;
    let key = Utf8PathBuf::from_path_buf(canon.clone())
        .map_err(|p| format!("{}: non-utf8 path", p.display()))?;
    if index
        .repos()
        .map_err(|e| e.to_string())?
        .iter()
        .any(|r| r.path == key)
    {
        Ok(canon)
    } else {
        Err(format!(
            "{}: not registered — `arca index add` it first",
            canon.display()
        ))
    }
}

fn snap_json(s: &RepoSnapshot) -> serde_json::Value {
    json!({
        "path": s.path,
        "name": s.name,
        "head_commit": s.head_commit,
        "scanned_at": s.scanned_at,
        "items": s.items.len(),
        "archived": s.archived.len(),
    })
}

/// The `index` verb group.
pub fn run(cmd: Cmd, json_out: bool) -> Result<i32, String> {
    let index = open_index()?;
    match cmd {
        Cmd::Add { path } => {
            let proj = repo_at(&path)?;
            let snap = scan_repo(&proj)?;
            let entry = RepoEntry::new(snap.path.clone(), snap.name.clone());
            index.register(&entry).map_err(|e| e.to_string())?;
            index.save_snapshot(&snap).map_err(|e| e.to_string())?;
            if json_out {
                ops::print_json(&json!({"registered": entry, "snapshot": snap_json(&snap)}));
            } else {
                println!(
                    "registered {} ({}) — {} items @ {}",
                    snap.name,
                    snap.path,
                    snap.items.len(),
                    &snap.head_commit[..snap.head_commit.len().min(7)],
                );
            }
            Ok(0)
        }
        Cmd::Remove { path } => {
            // resolve via discovery, else the raw path — a repo
            // whose worktree vanished must stay removable
            let root = match repo_at(&path) {
                Ok(p) => p.root,
                Err(_) => path.canonicalize().unwrap_or(path.clone()),
            };
            let root = root
                .canonicalize()
                .map_err(|e| format!("{}: {e}", root.display()))?;
            let key = Utf8PathBuf::from_path_buf(root)
                .map_err(|p| format!("{}: non-utf8 path", p.display()))?;
            if index.remove(&key).map_err(|e| e.to_string())? {
                if json_out {
                    ops::print_json(&json!({"removed": key}));
                } else {
                    println!("removed {key}");
                }
                Ok(0)
            } else {
                Err(format!("{key}: not registered"))
            }
        }
        Cmd::List => {
            let repos = index.repos().map_err(|e| e.to_string())?;
            let heads = par_map(&repos, |r| current_head(r.path.as_std_path()));
            let mut rows = Vec::new();
            let mut out = Vec::new();
            for (r, head) in repos.iter().zip(&heads) {
                let snap = index.snapshot(&r.path).map_err(|e| e.to_string())?;
                let stale = match (&snap, head) {
                    (Some(s), Some(h)) => s.head_commit != *h,
                    (Some(_), None) => true, // worktree gone
                    (None, _) => true,       // never scanned
                };
                out.push(json!({
                    "path": r.path,
                    "name": r.name,
                    "head_commit": snap.as_ref().map(|s| s.head_commit.clone()),
                    "scanned_at": snap.as_ref().map(|s| s.scanned_at.clone()),
                    "items": snap.as_ref().map(|s| s.items.len()),
                    "stale": stale,
                }));
                rows.push(format!(
                    "{}\t{}\t{}\t{}",
                    r.path,
                    r.name,
                    snap.as_ref()
                        .map(|s| s.items.len().to_string())
                        .unwrap_or_else(|| "-".into()),
                    if stale { "stale" } else { "fresh" },
                ));
            }
            if json_out {
                ops::print_json(&json!(out));
            } else if rows.is_empty() {
                println!("no repos registered — `arca index add <path>`");
            } else {
                ops::print_paged(&rows.join("\n"));
            }
            Ok(0)
        }
        Cmd::Rebuild { path } => {
            let roots: Vec<PathBuf> = match path {
                Some(p) => vec![registered_root(&index, &p)?],
                None => index
                    .repos()
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .map(|r| r.path.into_std_path_buf())
                    .collect(),
            };
            if roots.is_empty() {
                if json_out {
                    ops::print_json(&json!([]));
                } else {
                    println!("no repos registered — `arca index add <path>`");
                }
                return Ok(0);
            }
            // parallel rescans; serial saves + ordered reporting
            let snaps = par_map(&roots, |r| scan_one(r));
            let mut results = Vec::new();
            let mut failures = Vec::new();
            for (root, snap) in roots.iter().zip(snaps) {
                match snap.and_then(|s| {
                    index
                        .save_snapshot(&s)
                        .map_err(|e| e.to_string())
                        .map(|_| s)
                }) {
                    Ok(s) => {
                        if !json_out {
                            println!(
                                "rebuilt {} — {} items @ {}",
                                s.name,
                                s.items.len(),
                                &s.head_commit[..s.head_commit.len().min(7)],
                            );
                        }
                        results.push(
                            json!({"path": s.path, "status": "rebuilt", "snapshot": snap_json(&s)}),
                        );
                    }
                    Err(e) => {
                        failures.push(format!("{}: {e}", root.display()));
                        results.push(
                            json!({"path": root.to_string_lossy(), "status": "error", "error": e}),
                        );
                    }
                }
            }
            if json_out {
                ops::print_json(&json!(results));
            }
            if failures.is_empty() {
                Ok(0)
            } else {
                Err(format!(
                    "{} rebuild(s) failed: {}",
                    failures.len(),
                    failures.join("; ")
                ))
            }
        }
    }
}

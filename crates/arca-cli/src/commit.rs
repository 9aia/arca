//! `arca commit` — stage + commit worktree changes with a
//! conventional message. Git identity comes from the repo config,
//! like every other arca commit.

use std::path::PathBuf;

use serde_json::json;

use crate::discover;
use crate::ops;

/// Default message — conventional commits inside the worktree.
const DEFAULT_MSG: &str = "chore: update project ledger";

/// The `commit` verb.
pub fn run(
    dir: Option<PathBuf>,
    root: Option<PathBuf>,
    message: Option<String>,
    json_out: bool,
    no_commit: bool,
) -> Result<i32, String> {
    let proj = discover::discover(dir.as_deref(), root.as_deref())?;
    let msg = message.unwrap_or_else(|| DEFAULT_MSG.to_string());
    let enabled = ops::commits_enabled(no_commit);
    let dirty = arca_git::has_changes(&proj.worktree);
    let committed = if dirty && enabled {
        if !arca_git::commit_all(&proj.worktree, &msg) {
            return Err(format!("git commit failed in {}", proj.worktree.display()));
        }
        true
    } else {
        false
    };
    if json_out {
        ops::print_json(&json!({
            "worktree": proj.worktree.to_string_lossy(),
            "message": msg,
            "committed": committed,
            "skipped": !enabled || !dirty,
        }));
    } else if committed {
        println!("arca commit: {msg}");
    } else {
        println!(
            "arca commit: nothing to commit{}",
            if enabled { "" } else { " (--no-commit)" }
        );
    }
    Ok(0)
}

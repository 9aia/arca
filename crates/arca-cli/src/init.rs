//! `arca init` — orphan `project` branch (plumbing, never
//! `checkout --orphan`), `git worktree add project project`,
//! canonical scaffold, `project/` into the parent `.gitignore`,
//! the managed agent-awareness block into `AGENTS.md`.
//!
//! Idempotent: an existing branch is reused, an existing valid
//! worktree gets its scaffold verified/repaired instead of being
//! overwritten, and `AGENTS.md` prose outside the marked block is
//! preserved.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::discover;
use crate::ops;

/// The gitignore marker + entry appended on the main branch.
const IGNORE_COMMENT: &str = "# arca worktree mount point (created by `arca init`)";
const IGNORE_ENTRY: &str = "project/";

/// Outcome shared by the `init` verb and `migrate`'s ensure step.
#[derive(Debug)]
pub struct InitOutcome {
    /// Repo root.
    pub root: PathBuf,
    /// The worktree path.
    pub worktree: PathBuf,
    /// Scaffold files created (empty on a re-run).
    pub files_created: Vec<String>,
    /// `.gitignore` gained `project/` this run.
    pub gitignore_updated: bool,
    /// `AGENTS.md` gained/refreshed the managed block this run.
    pub agents_md_updated: bool,
    /// A commit was made (worktree scaffold or root files).
    pub committed: bool,
}

/// Ensure the project branch + worktree + scaffold exist. Safe to
/// call from any verb that wants a worktree to land on.
pub fn ensure(
    dir: Option<&Path>,
    root_hint: Option<&Path>,
    no_commit: bool,
) -> Result<InitOutcome, String> {
    let root = discover::repo_root_for(dir, root_hint)?;
    let worktree = discover::worktree_path(dir, &root);
    let enabled = ops::commits_enabled(no_commit);
    let mut committed = false;

    // refuse early: an existing non-worktree dir must not be
    // clobbered — check before creating any branch
    if worktree.exists()
        && arca_git::worktree_branch(&worktree).as_deref() != Some(discover::BRANCH)
    {
        return Err(format!(
            "{} exists but is not a arca project worktree — move it away or use --dir",
            worktree.display()
        ));
    }

    // branch: reuse or create as an orphan root via plumbing
    if !arca_git::branch_exists(&root, discover::BRANCH) {
        arca_git::create_orphan_branch(&root, discover::BRANCH, "chore: init project branch")
            .map_err(|e| format!("create project branch: {e}"))?;
    }

    // worktree: reuse a valid one, or `worktree add`
    if !worktree.exists() && !arca_git::worktree_add_branch(&root, &worktree, discover::BRANCH) {
        return Err(format!(
            "git worktree add {} {} failed — is the branch checked out elsewhere?",
            worktree.display(),
            discover::BRANCH
        ));
    }

    // scaffold via canonical emit — never overwrites
    let store = ops::store_at(&worktree)?;
    let name = repo_name(&root);
    let files_created = ops::scaffold(&store, &name).map_err(|e| e.to_string())?;
    if ops::autocommit(&worktree, "chore: init project ledger", enabled)? {
        committed = true;
    }

    // .gitignore on the current branch — only touch that file
    let gitignore_updated = ensure_gitignore(&root)?;
    if gitignore_updated
        && ops::commit_paths(
            &root,
            "chore: ignore project/ worktree mount point",
            &[".gitignore"],
            enabled,
        )?
    {
        committed = true;
    }

    // AGENTS.md managed block — same branch, own commit
    let agents_md_updated = crate::agentmd::sync(&root)? != crate::agentmd::Change::Unchanged;
    if agents_md_updated
        && ops::commit_paths(
            &root,
            "chore: manage AGENTS.md agent-awareness block",
            &["AGENTS.md"],
            enabled,
        )?
    {
        committed = true;
    }

    Ok(InitOutcome {
        root,
        worktree,
        files_created,
        gitignore_updated,
        agents_md_updated,
        committed,
    })
}

/// Repo name for `meta.toml`/titles — the repo dir's basename.
fn repo_name(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".to_string())
}

/// Append `project/` to `<root>/.gitignore` when absent. Returns
/// whether the file changed.
fn ensure_gitignore(root: &Path) -> Result<bool, String> {
    let path = root.join(".gitignore");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let present = existing
        .lines()
        .any(|l| matches!(l.trim(), "project/" | "project" | "/project/"));
    if present {
        return Ok(false);
    }
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out.push_str(IGNORE_COMMENT);
    out.push('\n');
    out.push_str(IGNORE_ENTRY);
    out.push('\n');
    std::fs::write(&path, out).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(true)
}

/// The `init` verb.
pub fn run(
    dir: Option<PathBuf>,
    root: Option<PathBuf>,
    json_out: bool,
    no_commit: bool,
) -> Result<i32, String> {
    let out = ensure(dir.as_deref(), root.as_deref(), no_commit)?;
    if json_out {
        ops::print_json(&json!({
            "branch": discover::BRANCH,
            "worktree": out.worktree.to_string_lossy(),
            "repo_root": out.root.to_string_lossy(),
            "files_created": out.files_created,
            "gitignore_updated": out.gitignore_updated,
            "agents_md_updated": out.agents_md_updated,
            "committed": out.committed,
        }));
    } else {
        println!(
            "arca init: {} → {} (branch {})",
            out.root.display(),
            out.worktree.display(),
            discover::BRANCH
        );
        if out.files_created.is_empty() {
            println!("  scaffold already complete — nothing to create");
        } else {
            for f in &out.files_created {
                println!("  created {f}");
            }
        }
        if out.gitignore_updated {
            println!("  .gitignore += project/");
        }
        if out.agents_md_updated {
            println!("  AGENTS.md += agent-awareness block");
        }
        if !out.committed {
            println!("  (no commits — nothing changed or --no-commit)");
        }
    }
    Ok(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {:?}", out.stderr);
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        git(d.path(), &["init", "-q"]);
        git(d.path(), &["config", "user.email", "t@t"]);
        git(d.path(), &["config", "user.name", "t"]);
        std::fs::write(d.path().join("README.md"), "x").unwrap();
        git(d.path(), &["add", "-A"]);
        git(d.path(), &["commit", "-m", "c0"]);
        d
    }

    #[test]
    fn ensure_full_flow_and_idempotent() {
        let d = repo();
        let out = ensure(None, Some(d.path()), false).unwrap();
        assert_eq!(out.worktree, d.path().join("project"));
        assert!(out.files_created.contains(&"meta.toml".to_string()));
        assert!(out.files_created.contains(&"TODO.md".to_string()));
        assert!(out.gitignore_updated);
        assert!(out.committed);
        // worktree files + commits exist
        let wt = out.worktree;
        assert!(wt.join("meta.toml").exists());
        assert!(wt.join("issues/.gitkeep").exists());
        let log = git(&wt, &["log", "--oneline"]);
        assert!(log.lines().count() >= 2, "orphan root + scaffold commit");
        let gi = std::fs::read_to_string(d.path().join(".gitignore")).unwrap();
        assert!(gi.contains("project/"));
        // AGENTS.md gained the managed block, committed on the
        // current branch
        assert!(out.agents_md_updated);
        let ag = std::fs::read_to_string(d.path().join("AGENTS.md")).unwrap();
        assert!(ag.contains(crate::agentmd::BEGIN));
        // meta name = repo dir name
        let meta = std::fs::read_to_string(wt.join("meta.toml")).unwrap();
        assert!(meta.contains(&format!(
            "name = \"{}\"",
            d.path().file_name().unwrap().to_string_lossy()
        )));

        // second run: nothing created, still Ok
        let out2 = ensure(None, Some(d.path()), false).unwrap();
        assert!(out2.files_created.is_empty());
        assert!(!out2.gitignore_updated);
        assert!(!out2.agents_md_updated);
    }

    #[test]
    fn ensure_respects_no_commit() {
        let d = repo();
        let out = ensure(None, Some(d.path()), true).unwrap();
        assert!(!out.committed);
        let wt = out.worktree;
        assert!(!git(&wt, &["status", "--porcelain"]).is_empty());
        // clean up so tempdir removal isn't blocked by the worktree
        git(d.path(), &["worktree", "remove", "--force", "project"]);
    }

    #[test]
    fn existing_plain_project_dir_errors() {
        let d = repo();
        std::fs::create_dir_all(d.path().join("project")).unwrap();
        let e = ensure(None, Some(d.path()), false).unwrap_err();
        assert!(e.contains("not a arca project worktree"), "{e}");
        // the dir conflict is detected before the branch is created
        assert!(!arca_git::branch_exists(d.path(), "project"));
    }

    #[test]
    fn gitignore_append_handles_missing_newline() {
        let d = repo();
        std::fs::write(d.path().join(".gitignore"), "/target").unwrap();
        assert!(ensure_gitignore(d.path()).unwrap());
        let gi = std::fs::read_to_string(d.path().join(".gitignore")).unwrap();
        assert!(gi.contains("/target\n"));
        assert!(gi.contains(IGNORE_ENTRY));
        // already-present variants are detected
        for content in ["project/\n", "project\n", "/project/\n"] {
            std::fs::write(d.path().join(".gitignore"), content).unwrap();
            assert!(!ensure_gitignore(d.path()).unwrap(), "{content}");
        }
    }

    #[test]
    fn repo_name_is_dirname() {
        assert_eq!(repo_name(Path::new("/a/b/arca")), "arca");
        assert_eq!(repo_name(Path::new("/")), "project");
    }
}

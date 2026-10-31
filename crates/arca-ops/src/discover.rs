//! Project discovery — resolve the `project/` worktree a verb
//! should act on.
//!
//! Order: `--dir` (explicit worktree) → `--root` (repo root →
//! `<root>/project`) → `arca.toml`'s `project_dir` → walk up from
//! cwd for a `project/` dir that is a git worktree on branch
//! `project` (or, weaker, any `project/` holding a `TODO.md`).
//! Running from *inside* the worktree works too.

use std::path::{Path, PathBuf};

use arca_config::ConfigSource;
use arca_config_toml::TomlSource;

/// The orphan branch + conventional dir name.
pub const BRANCH: &str = "project";
/// Conventional worktree dir at the repo root.
pub const DIR: &str = "project";

/// A resolved project: repo root + the `project/` worktree inside
/// (or pointed at via `--dir`).
#[derive(Debug)]
pub struct Project {
    /// Main worktree — the repo the ledger belongs to.
    pub root: PathBuf,
    /// The project worktree dir (`<root>/project` by convention).
    pub worktree: PathBuf,
}

/// `d` is usable as the project dir — a git worktree checked out
/// on `project`, or at minimum a dir holding `TODO.md`.
pub fn qualifies(d: &Path) -> bool {
    if !d.is_dir() {
        return false;
    }
    arca_git::worktree_branch(d).as_deref() == Some(BRANCH) || d.join("TODO.md").is_file()
}

/// The worktree root enclosing `cwd` is itself on `project` —
/// handles running arca from inside `project/` (any depth).
/// `rev-parse --show-toplevel` finds the real worktree root, so a
/// nested dir never gets mistaken for the ledger.
fn enclosing_project_worktree(cwd: &Path) -> Option<PathBuf> {
    let top = arca_git::repo_root(cwd).ok()?;
    (arca_git::worktree_branch(&top).as_deref() == Some(BRANCH)).then_some(top)
}

fn canonicalize(p: &Path) -> Result<PathBuf, String> {
    p.canonicalize()
        .map_err(|e| format!("{}: {e}", p.display()))
}

/// Resolve the project for verbs that need one. `dir`/`root` are
/// the global flags; everything else is discovery.
pub fn discover(dir: Option<&Path>, root: Option<&Path>) -> Result<Project, String> {
    // --dir: an explicit worktree path
    if let Some(d) = dir {
        let worktree = canonicalize(d)?;
        if !worktree.is_dir() {
            return Err(format!("--dir {} is not a directory", worktree.display()));
        }
        let root = match root {
            Some(r) => canonicalize(r)?,
            None => arca_git::main_worktree(&worktree)
                .or_else(|| worktree.parent().map(Path::to_path_buf))
                .unwrap_or_else(|| worktree.clone()),
        };
        return Ok(Project { root, worktree });
    }

    // --root: conventional <root>/project
    if let Some(r) = root {
        let root = canonicalize(r)?;
        let worktree = root.join(DIR);
        if !qualifies(&worktree) {
            return Err(format!(
                "{} is not a arca project worktree — run `arca init`",
                worktree.display()
            ));
        }
        return Ok(Project { root, worktree });
    }

    let cwd = std::env::current_dir().map_err(|e| format!("cwd: {e}"))?;

    // arca.toml project_dir, relative to the repo root
    if let Ok(cfg) = (TomlSource { path: None }).load() {
        if let Some(rel) = cfg.project_dir {
            if let Ok(root) = arca_git::repo_root(&cwd) {
                let worktree = root.join(rel);
                if qualifies(&worktree) {
                    return Ok(Project { root, worktree });
                }
            }
        }
    }

    // walk up: `<ancestor>/project` that qualifies
    for a in cwd.ancestors() {
        let candidate = a.join(DIR);
        if candidate != *a && qualifies(&candidate) {
            return Ok(Project {
                root: a.to_path_buf(),
                worktree: candidate,
            });
        }
    }

    // or cwd is inside the project worktree itself
    if let Some(wt) = enclosing_project_worktree(&cwd) {
        let root = arca_git::main_worktree(&wt)
            .or_else(|| wt.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| wt.clone());
        return Ok(Project { root, worktree: wt });
    }

    Err(no_project_error(&cwd))
}

/// Tailor the dead-end message — `init` can only help inside a repo.
fn no_project_error(cwd: &Path) -> String {
    match arca_git::repo_root(cwd) {
        Ok(root) => format!(
            "no project worktree found above {} — run `arca init` in {}",
            cwd.display(),
            root.display()
        ),
        Err(_) => format!(
            "no arca project above {} — not inside a git repository",
            cwd.display()
        ),
    }
}

/// Resolve the repo root for `arca init` / `migrate` — the verbs
/// that create the worktree rather than requiring it. `--root`
/// wins; `--dir`'s parent next; then the git repo containing cwd.
pub fn repo_root_for(dir: Option<&Path>, root: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(r) = root {
        return canonicalize(r);
    }
    if let Some(d) = dir {
        let parent = d.parent().unwrap_or(d);
        return arca_git::repo_root(parent)
            .map_err(|_| format!("{}: not inside a git repository", parent.display()));
    }
    let cwd = std::env::current_dir().map_err(|e| format!("cwd: {e}"))?;
    arca_git::repo_root(&cwd).map_err(|_| {
        format!(
            "arca init: {} is not inside a git repository",
            cwd.display()
        )
    })
}

/// The worktree path `init`/`migrate` should use — `--dir` or the
/// conventional `<root>/project`.
pub fn worktree_path(dir: Option<&Path>, root: &Path) -> PathBuf {
    dir.map(Path::to_path_buf).unwrap_or_else(|| root.join(DIR))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let st = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    }

    /// A repo with an initialized project worktree.
    fn repo_with_project() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        git(r, &["init", "-q"]);
        git(r, &["config", "user.email", "t@t"]);
        git(r, &["config", "user.name", "t"]);
        arca_git::create_orphan_branch(r, "project", "init").unwrap();
        git(r, &["worktree", "add", "project", "project"]);
        std::fs::write(r.join("project/TODO.md"), "# TODO — x\n").unwrap();
        d
    }

    #[test]
    fn dir_flag_resolves() {
        let d = repo_with_project();
        let p = discover(Some(&d.path().join("project")), None).unwrap();
        assert_eq!(p.worktree, d.path().join("project").canonicalize().unwrap());
        assert_eq!(p.root, d.path());
    }

    #[test]
    fn root_flag_resolves_and_errors() {
        let d = repo_with_project();
        let p = discover(None, Some(d.path())).unwrap();
        assert_eq!(p.worktree, d.path().join("project"));
        // root without a qualifying project/ → error
        let empty = tempfile::tempdir().unwrap();
        assert!(discover(None, Some(empty.path())).is_err());
    }

    #[test]
    fn dir_flag_missing_is_error() {
        assert!(discover(Some(Path::new("/definitely/not/here")), None).is_err());
    }

    #[test]
    fn qualifies_needs_dir() {
        assert!(!qualifies(Path::new("/nonexistent")));
        let d = tempfile::tempdir().unwrap();
        // plain dir without TODO.md or project branch → no
        assert!(!qualifies(d.path()));
        std::fs::write(d.path().join("TODO.md"), "x").unwrap();
        assert!(qualifies(d.path()));
    }

    #[test]
    fn no_project_error_adapts_to_git_context() {
        // inside a repo → init hint naming the root
        let d = tempfile::tempdir().unwrap();
        git(d.path(), &["init", "-q"]);
        let msg = no_project_error(d.path());
        assert!(msg.contains("run `arca init` in"), "{msg}");

        // outside any repo → no init hint
        let outside = tempfile::tempdir().unwrap();
        let msg = no_project_error(outside.path());
        assert!(msg.contains("not inside a git repository"), "{msg}");
    }

    #[test]
    fn repo_root_for_variants() {
        let d = repo_with_project();
        assert_eq!(repo_root_for(None, Some(d.path())).unwrap(), d.path());
        assert_eq!(
            repo_root_for(Some(&d.path().join("project")), None).unwrap(),
            d.path().canonicalize().unwrap()
        );
        assert!(repo_root_for(Some(Path::new("/not/here")), None).is_err());
    }

    #[test]
    fn worktree_path_prefers_dir() {
        let d = tempfile::tempdir().unwrap();
        let explicit = Path::new("/tmp/custom");
        assert_eq!(worktree_path(Some(explicit), d.path()), explicit);
        assert_eq!(worktree_path(None, d.path()), d.path().join("project"));
    }
}

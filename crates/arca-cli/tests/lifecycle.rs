//! End-to-end lifecycle tests — `arca init | check | migrate |
//! commit` driving the real binary against tempdir git repos.
//!
//! `ARCA_CONFIG` is pinned at a nonexistent path so a stray user
//! config can't leak `project_dir` into discovery.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const ARCA: &str = env!("CARGO_BIN_EXE_arca");

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q"]);
    git(d.path(), &["config", "user.email", "t@t"]);
    git(d.path(), &["config", "user.name", "t"]);
    std::fs::write(d.path().join("README.md"), "x").unwrap();
    git(d.path(), &["add", "-A"]);
    git(d.path(), &["commit", "-qm", "c0"]);
    d
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(ARCA)
        .current_dir(dir)
        .args(args)
        // deterministic config search: no arca.toml anywhere
        .env("ARCA_CONFIG", "/nonexistent/arca.toml")
        .env_remove("ARCA_NO_COMMIT")
        .output()
        .unwrap()
}

fn json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("bad json ({}): {}", e, String::from_utf8_lossy(&out.stdout)))
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

fn init(d: &Path) -> Value {
    let out = run(d, &["init", "--json"]);
    assert_eq!(code(&out), 0, "init failed: {out:?}");
    json(&out)
}

#[test]
fn init_scaffolds_worktree_and_is_idempotent() {
    let d = repo();
    let v = init(d.path());
    assert_eq!(v["branch"], "project");
    assert_eq!(v["committed"], true);
    let wt = PathBuf::from(v["worktree"].as_str().unwrap());
    assert_eq!(wt, d.path().join("project"));
    for f in [
        "meta.toml",
        "TODO.md",
        "BACKLOG.md",
        "index.md",
        "issues/.gitkeep",
        "proposals/.gitkeep",
        "adrs/.gitkeep",
        "plans/.gitkeep",
    ] {
        assert!(wt.join(f).exists(), "{f}");
        assert!(
            v["files_created"].as_array().unwrap().contains(&json_f(f)),
            "{f} created"
        );
    }
    // canonical content
    assert_eq!(
        std::fs::read_to_string(wt.join("TODO.md")).unwrap(),
        format!(
            "# TODO — {}\n",
            d.path().file_name().unwrap().to_string_lossy()
        )
    );
    let meta = std::fs::read_to_string(wt.join("meta.toml")).unwrap();
    assert!(meta.contains("[next]"));
    // worktree on the project branch, orphan root
    assert!(git(d.path(), &["worktree", "list"]).contains("project"));
    assert_eq!(git(&wt, &["branch", "--show-current"]), "project");
    let log = git(&wt, &["log", "--format=%s"]);
    assert!(log.contains("chore: init project ledger"));
    assert!(log.contains("chore: init project branch"));
    // .gitignore entry + AGENTS.md block, each committed on the
    // current branch
    let gi = std::fs::read_to_string(d.path().join(".gitignore")).unwrap();
    assert!(gi.contains("project/"));
    let ag = std::fs::read_to_string(d.path().join("AGENTS.md")).unwrap();
    assert!(ag.contains("<!-- arca:agent-awareness -->"));
    let main_log = git(d.path(), &["log", "--format=%s"]);
    assert!(main_log.contains("chore: ignore project/ worktree mount point"));
    assert!(main_log.contains("chore: manage AGENTS.md agent-awareness block"));
    // main's index/tree untouched: project/ contents were never
    // committed to the current branch
    assert_eq!(git(d.path(), &["status", "--porcelain"]), "");

    // second run — idempotent, nothing created
    let v2 = init(d.path());
    assert_eq!(v2["files_created"], serde_json::json!([]));
    assert_eq!(v2["gitignore_updated"], false);
    assert_eq!(v2["agents_md_updated"], false);
}

fn json_f(s: &str) -> Value {
    Value::String(s.to_string())
}

#[test]
fn init_outside_repo_errors() {
    let d = tempfile::tempdir().unwrap();
    let out = run(d.path(), &["init"]);
    assert_eq!(code(&out), 2);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not inside a git repository"), "{err}");
    // --json reports the same error on stdout
    let out = run(d.path(), &["init", "--json"]);
    assert_eq!(code(&out), 2);
    assert!(json(&out)["error"].as_str().unwrap().contains("git"));
}

#[test]
fn discovery_walks_up_and_inside_worktree() {
    let d = repo();
    init(d.path());
    let nested = d.path().join("a/b/c");
    std::fs::create_dir_all(&nested).unwrap();
    let out = run(&nested, &["check", "--json"]);
    assert_eq!(code(&out), 0, "check from nested dir");
    let v = json(&out);
    assert_eq!(
        v["worktree"].as_str().unwrap(),
        d.path().join("project").to_str().unwrap()
    );
    // from inside the worktree itself
    let inner = d.path().join("project/issues");
    let out = run(&inner, &["check"]);
    assert_eq!(code(&out), 0);
    // --dir override works from anywhere
    let out = run(
        std::path::Path::new("/"),
        &["check", "--dir", d.path().join("project").to_str().unwrap()],
    );
    assert_eq!(code(&out), 0);
}

#[test]
fn check_reports_findings_and_fix_repairs() {
    let d = repo();
    init(d.path());
    let wt = d.path().join("project");
    // broken: a checked item, drift in meta.toml, a stray item file
    std::fs::write(
        wt.join("TODO.md"),
        "# TODO — x\n\n## Now\n\n- [ ] T-001 open\n- [x] T-002 done\n",
    )
    .unwrap();
    std::fs::write(wt.join("meta.toml"), "name = \"x\"\n").unwrap();
    let out = run(d.path(), &["check", "--json"]);
    assert_eq!(code(&out), 1);
    let v = json(&out);
    assert_eq!(v["ok"], false);
    let codes: Vec<&str> = v["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"done-item"), "{codes:?}");
    assert!(codes.contains(&"parse"), "{codes:?}");
    assert!(codes.contains(&"emit-drift"), "{codes:?}");
    assert!(v["fixes_available"].as_u64().unwrap() >= 2);

    // --fix repairs: done item stripped, meta reconciled, commit
    let out = run(d.path(), &["check", "--fix", "--json"]);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let v = json(&out);
    assert_eq!(v["ok"], true);
    assert!(v["fixes_applied"].as_u64().unwrap() >= 2);
    let todo = std::fs::read_to_string(wt.join("TODO.md")).unwrap();
    assert!(!todo.contains("[x]"));
    assert!(todo.contains("T-001"));
    let meta = std::fs::read_to_string(wt.join("meta.toml")).unwrap();
    assert!(meta.contains("t = 2"), "counter reconciled past T-001");
    assert!(git(&wt, &["log", "-1", "--format=%s"]).starts_with("chore: check --fix"));
    // clean tree, clean check
    let out = run(d.path(), &["check"]);
    assert_eq!(code(&out), 0);
}

#[test]
fn check_flags_duplicates_dangling_and_filenames() {
    let d = repo();
    init(d.path());
    let wt = d.path().join("project");
    std::fs::write(
        wt.join("TODO.md"),
        "# TODO — x\n\n- [ ] T-001 a · blocks:T-099\n- [ ] T-001 dup\n",
    )
    .unwrap();
    std::fs::write(
        wt.join("issues/not-canonical.md"),
        "+++\nid = \"I-001\"\ntitle = \"t\"\nstatus = \"open\"\n+++\n",
    )
    .unwrap();
    let out = run(d.path(), &["check", "--json"]);
    assert_eq!(code(&out), 1);
    let v = json(&out);
    let codes: Vec<&str> = v["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"duplicate-id"), "{codes:?}");
    assert!(codes.contains(&"dangling-blocks"), "{codes:?}");
    assert!(codes.contains(&"filename"), "{codes:?}");
}

#[test]
fn check_fix_moves_misplaced_item_files() {
    let d = repo();
    init(d.path());
    let wt = d.path().join("project");
    // a proposal file sitting in issues/
    std::fs::write(
        wt.join("issues/proposal-0003-x.md"),
        "+++\nid = \"P-003\"\ntitle = \"x\"\nstatus = \"open\"\n+++\n",
    )
    .unwrap();
    let out = run(d.path(), &["check", "--fix", "--json"]);
    assert_eq!(code(&out), 0);
    assert!(wt.join("proposals/proposal-0003-x.md").exists());
    assert!(!wt.join("issues/proposal-0003-x.md").exists());
}

#[test]
fn migrate_normalizes_legacy_project() {
    let d = repo();
    init(d.path());
    let wt = d.path().join("project");
    std::fs::write(
        wt.join("TODO.md"),
        "# TODO — x\n\n## Phase 1\n\n- [x] done · status:done-ish?\n- [ ] alpha task\n- [ ] beta · pri:high\n",
    )
    .unwrap();
    std::fs::write(
        wt.join("BACKLOG.md"),
        "# Backlog — x\n\nIntro prose ending in a period.\n\n- parked one\n",
    )
    .unwrap();
    std::fs::write(
        wt.join("proposals/draft.md"),
        "# Draft proposal\n\nno frontmatter\n",
    )
    .unwrap();
    let out = run(d.path(), &["migrate", "--json"]);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let v = json(&out);
    assert_eq!(v["mode"], "project");
    assert_eq!(v["ids_allocated"], 4); // 2 todo + 1 backlog + 1 proposal
    assert_eq!(v["dropped_done"], 1);
    let todo = std::fs::read_to_string(wt.join("TODO.md")).unwrap();
    assert!(todo.contains("- [ ] T-001 alpha task"));
    assert!(todo.contains("- [ ] T-002 beta · pri:high"));
    let backlog = std::fs::read_to_string(wt.join("BACKLOG.md")).unwrap();
    assert!(backlog.contains("- T-003 parked one"));
    assert!(wt
        .join("proposals/proposal-0001-draft-proposal.md")
        .exists());
    // the migrate commit landed on the project branch
    assert!(git(&wt, &["log", "-1", "--format=%s"]).starts_with("chore: migrate project/"));
    // and check agrees the ledger is canonical now
    let out = run(d.path(), &["check"]);
    assert_eq!(code(&out), 0);
}

#[test]
fn migrate_pm_dir_converts_and_removes() {
    let d = repo();
    let pm = d.path().join("pm");
    std::fs::create_dir_all(&pm).unwrap();
    std::fs::write(
        pm.join("TODO.md"),
        "# TODO\n\n- [x] shipped\n- [ ] task one\n- [ ] task two\n",
    )
    .unwrap();
    std::fs::write(pm.join("BACKLOG.md"), "# Backlog\n\n- later idea\n").unwrap();
    std::fs::write(pm.join("ROADMAP.md"), "# Grand plan\n\nstuff\n").unwrap();
    std::fs::write(pm.join("NOTES.md"), "# random notes\n\nbody\n").unwrap();
    git(d.path(), &["add", "-A"]);
    git(d.path(), &["commit", "-qm", "pm files"]);

    let out = run(d.path(), &["migrate", "--json"]);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let v = json(&out);
    assert_eq!(v["mode"], "pm");
    assert_eq!(v["pm_removed"], true);
    assert!(!pm.exists());
    let wt = d.path().join("project");
    let todo = std::fs::read_to_string(wt.join("TODO.md")).unwrap();
    assert!(todo.contains("task one"));
    assert!(todo.contains("T-0"), "ids allocated: {todo}");
    assert!(wt.join("plans/pl-0001-grand-plan.md").exists());
    assert!(wt.join("issues/issue-0001-random-notes.md").exists());
    // pm/ removal committed on the current branch (not --allow-empty noise)
    let subjects = git(d.path(), &["log", "--format=%s"]);
    assert!(
        subjects.contains("chore: migrate pm/ → project/"),
        "{subjects}"
    );
    assert_eq!(git(d.path(), &["status", "--porcelain"]), "");
    // check on the migrated ledger is clean
    let out = run(d.path(), &["check"]);
    assert_eq!(code(&out), 0);
}

#[test]
fn migrate_pm_keeps_untracked_leftovers() {
    let d = repo();
    let pm = d.path().join("pm");
    std::fs::create_dir_all(&pm).unwrap();
    std::fs::write(pm.join("TODO.md"), "# TODO\n\n- [ ] t\n").unwrap();
    // a file that isn't *.md and isn't tracked — data must survive
    std::fs::write(pm.join("scratch.txt"), "keep me").unwrap();
    git(d.path(), &["add", "pm/TODO.md"]);
    git(d.path(), &["commit", "-qm", "pm"]);

    let out = run(d.path(), &["migrate", "--json"]);
    assert_eq!(code(&out), 0);
    let v = json(&out);
    assert_eq!(v["pm_removed"], false);
    assert!(pm.join("scratch.txt").exists(), "untracked file preserved");
    assert!(v["pm_leftover"]
        .as_array()
        .unwrap()
        .contains(&json_f("pm/scratch.txt")));
}

#[test]
fn commit_verb_commits_worktree_changes() {
    let d = repo();
    init(d.path());
    let wt = d.path().join("project");
    std::fs::write(wt.join("notes.txt"), "hello").unwrap();
    let out = run(d.path(), &["commit", "-m", "test: a message", "--json"]);
    assert_eq!(code(&out), 0);
    assert_eq!(json(&out)["committed"], true);
    assert_eq!(git(&wt, &["log", "-1", "--format=%s"]), "test: a message");
    // nothing to commit → committed:false, no new commit
    let out = run(d.path(), &["commit", "--json"]);
    assert_eq!(code(&out), 0);
    let v = json(&out);
    assert_eq!(v["committed"], false);
    assert_eq!(v["skipped"], true);
}

#[test]
fn no_commit_flag_and_env_suppress() {
    let d = repo();
    let out = run(d.path(), &["init", "--no-commit"]);
    assert_eq!(code(&out), 0);
    let wt = d.path().join("project");
    // scaffold written but uncommitted; branch+worktree exist
    assert!(wt.join("TODO.md").exists());
    assert!(!git(&wt, &["status", "--porcelain"]).is_empty());
    assert!(git(d.path(), &["status", "--porcelain"]).contains(".gitignore"));
    // env var suppresses too
    std::fs::write(wt.join("x.txt"), "x").unwrap();
    let out = Command::new(ARCA)
        .current_dir(&wt)
        .args(["commit", "--json"])
        .env("ARCA_CONFIG", "/nonexistent/arca.toml")
        .env("ARCA_NO_COMMIT", "1")
        .output()
        .unwrap();
    assert_eq!(json(&out)["committed"], false);
    // clean up the worktree so the tempdir drops cleanly
    git(d.path(), &["worktree", "remove", "--force", "project"]);
    let _ = std::fs::remove_file(d.path().join(".gitignore"));
}

#[test]
fn manifest_lists_implemented_verbs() {
    let out = run(std::path::Path::new("/"), &["manifest"]);
    assert_eq!(code(&out), 0);
    let v = json(&out);
    let implemented = v["implemented"].as_array().unwrap();
    for verb in ["init", "check", "migrate", "commit", "manifest"] {
        assert!(implemented.contains(&json_f(verb)), "{verb}");
    }
}

#[test]
fn schema_needs_no_project() {
    // schema is repo-free discovery; status needs a ledger
    let out = run(std::path::Path::new("/"), &["schema"]);
    assert_eq!(code(&out), 0);
    assert_eq!(json(&out)["version"], 1);
    let d = tempfile::tempdir().unwrap();
    let out = run(d.path(), &["status"]);
    assert_eq!(code(&out), 2);
}

#[test]
fn human_output_paths() {
    let d = repo();
    // init — human lines instead of json
    let out = run(d.path(), &["init"]);
    assert_eq!(code(&out), 0);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("arca init:"), "{stdout}");
    assert!(stdout.contains("created meta.toml"), "{stdout}");
    // again — the "already complete" branch
    let out = run(d.path(), &["init"]);
    assert_eq!(code(&out), 0);
    assert!(String::from_utf8_lossy(&out.stdout).contains("already complete"));

    // check — parse error renders a miette diagnostic on stderr
    let wt = d.path().join("project");
    std::fs::write(
        wt.join("TODO.md"),
        "# TODO — x\n\n- [ ] T-1 o\n- plain bullet\n",
    )
    .unwrap();
    let out = run(d.path(), &["check"]);
    assert_eq!(code(&out), 1);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("TODO.md"), "{stderr}");
    assert!(stderr.contains("finding"), "{stderr}");
    // --fix can't repair a fully-unparseable file — migrate can
    let out = run(d.path(), &["migrate"]);
    assert_eq!(code(&out), 0);
    assert!(String::from_utf8_lossy(&out.stdout).contains("arca migrate"));
    let out = run(d.path(), &["check"]);
    assert_eq!(code(&out), 0);
    assert!(String::from_utf8_lossy(&out.stderr).contains("clean"));

    // commit — human line + "nothing to commit" branch
    std::fs::write(wt.join("x.txt"), "x").unwrap();
    let out = run(d.path(), &["commit"]);
    assert_eq!(code(&out), 0);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("arca commit: chore: update project ledger")
    );
    let out = run(d.path(), &["commit"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("nothing to commit"));
}

#[test]
fn missing_worktree_is_a_clear_error() {
    let d = repo(); // git repo, but no project worktree
    let out = run(d.path(), &["check"]);
    assert_eq!(code(&out), 2);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("arca init"), "{stderr}");
    let out = run(d.path(), &["commit", "--json"]);
    assert_eq!(code(&out), 2);
    assert!(json(&out)["error"].as_str().unwrap().contains("arca init"));
}

#[test]
fn check_fix_strips_done_from_backlog() {
    let d = repo();
    init(d.path());
    let wt = d.path().join("project");
    std::fs::write(
        wt.join("BACKLOG.md"),
        "# Backlog\n\n- T-001 keep\n- T-002 drop · status:done\n",
    )
    .unwrap();
    let out = run(d.path(), &["check", "--fix"]);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let backlog = std::fs::read_to_string(wt.join("BACKLOG.md")).unwrap();
    assert!(backlog.contains("T-001 keep"));
    assert!(!backlog.contains("T-002"));
}

#[test]
fn check_flags_and_fixes_agents_md() {
    let d = repo();
    init(d.path());
    // a repo whose AGENTS.md lacks the managed block flags it
    std::fs::remove_file(d.path().join("AGENTS.md")).unwrap();
    let out = run(d.path(), &["check"]);
    assert_eq!(code(&out), 1);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("agents-md"), "{err}");
    // --fix creates + commits it on the current branch
    let out = run(d.path(), &["check", "--fix"]);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let ag = std::fs::read_to_string(d.path().join("AGENTS.md")).unwrap();
    assert!(ag.contains("<!-- arca:agent-awareness -->"));
    assert!(git(d.path(), &["log", "--format=%s"])
        .contains("chore: manage AGENTS.md agent-awareness block"));
    // and a hand-written section gets upgraded, not duplicated
    std::fs::write(
        d.path().join("AGENTS.md"),
        "# R\n\n## Agent awareness\n\n- custom\n",
    )
    .unwrap();
    let out = run(d.path(), &["check"]);
    assert_eq!(code(&out), 1);
    let out = run(d.path(), &["check", "--fix"]);
    assert_eq!(code(&out), 0);
    let ag = std::fs::read_to_string(d.path().join("AGENTS.md")).unwrap();
    assert!(!ag.contains("custom"));
    assert_eq!(ag.matches("<!-- arca:agent-awareness -->").count(), 1);
    assert!(ag.starts_with("# R\n"));
}

#[test]
fn check_json_report_shape_snapshot() {
    let d = repo();
    init(d.path());
    let wt = d.path().join("project");
    std::fs::write(wt.join("TODO.md"), "# TODO — x\n\n- [x] T-001 done\n").unwrap();
    let out = run(d.path(), &["check", "--json"]);
    assert_eq!(code(&out), 1);
    let mut v = json(&out);
    // redact machine-local paths
    v["worktree"] = json_f("[WT]");
    v["repo_root"] = json_f("[ROOT]");
    insta::assert_snapshot!(serde_json::to_string_pretty(&v).unwrap());
}

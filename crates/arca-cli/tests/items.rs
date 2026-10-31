//! End-to-end item-verb tests — `todo`, `backlog`, `issue`,
//! `proposal`, `plan`, `adr`, `item show|set` driving the real
//! binary against tempdir git repos.
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

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// A repo with an initialized project worktree.
fn project() -> (tempfile::TempDir, PathBuf) {
    let d = repo();
    let out = run(d.path(), &["init", "--json"]);
    assert_eq!(code(&out), 0, "init failed: {}", stderr(&out));
    let wt = PathBuf::from(json(&out)["worktree"].as_str().unwrap());
    (d, wt)
}

fn read(wt: &Path, rel: &str) -> String {
    std::fs::read_to_string(wt.join(rel)).unwrap()
}

// ---------- todo ----------

#[test]
fn todo_add_list_done() {
    let (d, wt) = project();
    let out = run(
        d.path(),
        &[
            "todo",
            "add",
            "write phase 4",
            "--pri",
            "high",
            "--owner",
            "devin",
            "--label",
            "core",
            "--label",
            "cli",
            "--blocks",
            "T-002",
            "--json",
        ],
    );
    assert_eq!(code(&out), 0);
    let v = json(&out);
    assert_eq!(v["item"]["id"], "T-001");
    assert_eq!(v["item"]["priority"], "high");
    assert_eq!(v["item"]["owner"], "devin");
    assert_eq!(v["item"]["labels"][0], "cli"); // sorted
    assert_eq!(v["item"]["blocks"][0], "T-002");
    assert_eq!(v["committed"], true);
    let todo = read(&wt, "TODO.md");
    assert!(
        todo.contains("- [ ] T-001 write phase 4 · pri:high · owner:devin"),
        "{todo}"
    );
    assert!(todo.contains("#cli · #core"), "{todo}");
    assert_eq!(git(&wt, &["log", "-1", "--format=%s"]), "chore: add T-001");

    // --section lands under the heading (created when absent)
    let out = run(
        d.path(),
        &["todo", "add", "later thing", "--section", "Later"],
    );
    assert_eq!(code(&out), 0);
    let todo = read(&wt, "TODO.md");
    assert!(
        todo.contains("## Later\n\n- [ ] T-002 later thing"),
        "{todo}"
    );

    // filters
    let out = run(d.path(), &["todo", "list", "--owner", "devin", "--json"]);
    let v = json(&out);
    assert_eq!(v.as_array().unwrap().len(), 1);
    let out = run(d.path(), &["todo", "list", "--label", "infra", "--json"]);
    assert_eq!(json(&out).as_array().unwrap().len(), 0);

    // done removes the line (T-001 carries the dangling blocks ref
    // with it); sections that empty out are dropped
    for id in ["T-001", "T-002"] {
        let out = run(d.path(), &["todo", "done", id, "--json"]);
        assert_eq!(code(&out), 0);
        assert_eq!(json(&out)["item"]["status"], "done");
    }
    let todo = read(&wt, "TODO.md");
    assert!(!todo.contains("T-00"), "{todo}");
    assert!(!todo.contains("## Later"), "{todo}");
    assert_eq!(git(&wt, &["log", "-1", "--format=%s"]), "chore: done T-002");

    // id never reused
    let out = run(d.path(), &["todo", "add", "fresh", "--json"]);
    assert_eq!(json(&out)["item"]["id"], "T-003");
    let out = run(d.path(), &["check"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
}

#[test]
fn todo_defer_and_backlog_promote() {
    let (d, wt) = project();
    run(d.path(), &["todo", "add", "task one"]);
    let out = run(d.path(), &["todo", "defer", "T-001", "--json"]);
    assert_eq!(code(&out), 0);
    assert_eq!(json(&out)["item"]["status"], "deferred");
    let todo = read(&wt, "TODO.md");
    assert!(!todo.contains("T-001"), "{todo}");
    assert!(read(&wt, "BACKLOG.md").contains("- T-001 task one"));
    assert_eq!(
        git(&wt, &["log", "-1", "--format=%s"]),
        "chore: defer T-001"
    );

    // defer again → already deferred
    let out = run(d.path(), &["todo", "defer", "T-001"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("already deferred"));

    // done on a backlog item → machine has no deferred→done path
    let out = run(d.path(), &["todo", "done", "T-001"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("promote"), "{}", stderr(&out));

    // promote back
    let out = run(d.path(), &["backlog", "promote", "T-001", "--json"]);
    assert_eq!(code(&out), 0);
    assert_eq!(json(&out)["item"]["status"], "open");
    assert!(read(&wt, "TODO.md").contains("T-001"));
    assert!(!read(&wt, "BACKLOG.md").contains("T-001"));
    // promote twice → not in backlog
    let out = run(d.path(), &["backlog", "promote", "T-001"]);
    assert_eq!(code(&out), 2);
}

#[test]
fn backlog_add_and_list() {
    let (d, wt) = project();
    let out = run(
        d.path(),
        &[
            "backlog",
            "add",
            "someday idea",
            "--label",
            "maybe",
            "--json",
        ],
    );
    assert_eq!(code(&out), 0);
    let v = json(&out);
    assert_eq!(v["item"]["status"], "deferred");
    assert_eq!(v["file"], "BACKLOG.md");
    let backlog = read(&wt, "BACKLOG.md");
    assert!(backlog.contains("- T-001 someday idea"), "{backlog}");
    assert!(backlog.contains("#maybe"), "{backlog}");

    let out = run(d.path(), &["backlog", "list"]);
    assert!(stdout(&out).contains("T-001 deferred pri:med #maybe someday idea"));
    let out = run(
        d.path(),
        &["backlog", "list", "--status", "deferred", "--json"],
    );
    assert_eq!(json(&out).as_array().unwrap().len(), 1);
    let out = run(d.path(), &["backlog", "list", "--status", "open", "--json"]);
    assert_eq!(json(&out).as_array().unwrap().len(), 0);
}

#[test]
fn todo_list_human_snapshot() {
    let (d, _wt) = project();
    run(d.path(), &["todo", "add", "alpha", "--pri", "high"]);
    run(
        d.path(),
        &["todo", "add", "beta", "--owner", "devin", "--label", "x"],
    );
    let out = run(d.path(), &["todo", "list"]);
    assert_eq!(code(&out), 0);
    insta::assert_snapshot!(stdout(&out));
}

#[test]
fn nested_tasks_lifecycle() {
    let (d, wt) = project();
    // --parent nests the new item under an existing task
    run(d.path(), &["todo", "add", "parent task"]);
    let out = run(
        d.path(),
        &["todo", "add", "sub one", "--parent", "T-001", "--json"],
    );
    assert_eq!(code(&out), 0);
    assert_eq!(json(&out)["parent"], "T-001");
    let out = run(d.path(), &["todo", "add", "sub two", "--parent", "T-001"]);
    assert_eq!(code(&out), 0);
    let todo = read(&wt, "TODO.md");
    for frag in [
        "- [ ] T-001 parent task",
        "  - [ ] T-002 sub one",
        "  - [ ] T-003 sub two",
    ] {
        assert!(todo.contains(frag), "{frag} in {todo}");
    }
    // --section and --parent conflict
    let out = run(
        d.path(),
        &["todo", "add", "x", "--parent", "T-001", "--section", "s"],
    );
    assert_eq!(code(&out), 2);
    // a missing or non-task parent is a clean error
    let out = run(d.path(), &["todo", "add", "x", "--parent", "T-099"]);
    assert_eq!(code(&out), 2);
    run(d.path(), &["issue", "new", "an issue"]);
    let out = run(d.path(), &["todo", "add", "x", "--parent", "I-001"]);
    assert_eq!(code(&out), 2);

    // list indents children under their parent; --json nests them
    let out = run(d.path(), &["todo", "list"]);
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines.iter().any(|l| l.starts_with("  T-002")),
        "{}",
        lines.join("\n")
    );
    let out = run(d.path(), &["todo", "list", "--json"]);
    let v = json(&out);
    assert!(
        v.as_array().unwrap().iter().any(|i| i["id"] == "T-002"),
        "{v}"
    );

    // item set reaches children
    let out = run(
        d.path(),
        &["item", "set", "T-002", "--status", "in-progress", "--json"],
    );
    assert_eq!(code(&out), 0);
    let todo = read(&wt, "TODO.md");
    assert!(
        todo.contains("  - [ ] T-002 sub one · status:in-progress"),
        "{todo}"
    );
    // item show finds a child id
    let out = run(d.path(), &["item", "show", "T-003", "--json"]);
    assert_eq!(json(&out)["location"], "TODO.md");

    // done on a child removes just the child — parent + sibling stay
    let out = run(d.path(), &["todo", "done", "T-002", "--json"]);
    assert_eq!(code(&out), 0);
    let todo = read(&wt, "TODO.md");
    assert!(todo.contains("T-001"), "{todo}");
    assert!(todo.contains("T-003"), "{todo}");
    assert!(!todo.contains("T-002"), "{todo}");

    // defer on a parent moves the whole subtree to BACKLOG.md
    let out = run(d.path(), &["todo", "defer", "T-001", "--json"]);
    assert_eq!(code(&out), 0);
    let todo = read(&wt, "TODO.md");
    assert!(!todo.contains("T-00"), "{todo}");
    let backlog = read(&wt, "BACKLOG.md");
    for frag in ["- T-001 parent task", "  - T-003 sub two"] {
        assert!(backlog.contains(frag), "{frag} in {backlog}");
    }
    let out2 = run(d.path(), &["backlog", "list"]);
    assert!(
        stdout(&out2).contains("  T-003"),
        "child indented in backlog list"
    );
    // children park with the parent — no status token needed
    assert!(!backlog.contains("status:"), "{backlog}");

    // a child added under a backlog parent is born deferred
    let out = run(d.path(), &["todo", "add", "late sub", "--parent", "T-001"]);
    assert_eq!(code(&out), 0);
    let backlog = read(&wt, "BACKLOG.md");
    assert!(backlog.contains("  - T-004 late sub"), "{backlog}");

    // promote restores the subtree — children unpark to open
    let out = run(d.path(), &["backlog", "promote", "T-001", "--json"]);
    assert_eq!(code(&out), 0);
    let todo = read(&wt, "TODO.md");
    for frag in [
        "- [ ] T-001 parent task",
        "  - [ ] T-003 sub two",
        "  - [ ] T-004 late sub",
    ] {
        assert!(todo.contains(frag), "{frag} in {todo}");
    }
    assert!(!todo.contains("status:"), "{todo}");

    // done on a parent removes the whole subtree
    let out = run(d.path(), &["todo", "done", "T-001", "--json"]);
    assert_eq!(code(&out), 0);
    let todo = read(&wt, "TODO.md");
    assert!(!todo.contains("T-00"), "{todo}");
    // and the ledger is clean afterwards
    let out = run(d.path(), &["check"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
}

#[test]
fn check_flags_nested_duplicate_and_dangling() {
    let (d, wt) = project();
    let out = run(d.path(), &["check", "--json"]);
    assert_eq!(code(&out), 0);
    std::fs::write(
        wt.join("TODO.md"),
        "# TODO — x\n\n- [ ] T-001 a\n  - [ ] T-002 sub · blocks:T-099\n- [ ] T-002 dup\n",
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
}

// ---------- issue ----------

#[test]
fn issue_new_list_resolve_archive_comment() {
    let (d, wt) = project();
    let out = run(
        d.path(),
        &[
            "issue",
            "new",
            "index corruption",
            "--pri",
            "crit",
            "--json",
        ],
    );
    assert_eq!(code(&out), 0);
    let v = json(&out);
    assert_eq!(v["item"]["id"], "I-001");
    assert_eq!(v["path"], "issues/issue-0001-index-corruption.md");
    let file = read(&wt, "issues/issue-0001-index-corruption.md");
    for frag in [
        "id = \"I-001\"",
        "status = \"open\"",
        "priority = \"critical\"",
        "## Description",
        "## Checklist",
        "## Resolution",
    ] {
        assert!(file.contains(frag), "{frag} in {file}");
    }
    assert_eq!(
        git(&wt, &["log", "-1", "--format=%s"]),
        "chore: new issue I-001"
    );

    // --text supplies the body
    let out = run(d.path(), &["issue", "new", "b", "--text", "custom body"]);
    assert_eq!(code(&out), 0);
    assert!(read(&wt, "issues/issue-0002-b.md").contains("custom body"));

    // list + filters
    let out = run(d.path(), &["issue", "list"]);
    assert_eq!(stdout(&out).lines().count(), 2);
    let out = run(d.path(), &["issue", "list", "--status", "open", "--json"]);
    assert_eq!(json(&out).as_array().unwrap().len(), 2);

    // comment appends a dated line under ## Comments
    let out = run(d.path(), &["issue", "comment", "I-001", "repro confirmed"]);
    assert_eq!(code(&out), 0);
    let file = read(&wt, "issues/issue-0001-index-corruption.md");
    assert!(file.contains("## Comments\n\n- 20"), "{file}");
    assert!(file.contains("repro confirmed"), "{file}");

    // archive before done → refused
    let out = run(d.path(), &["issue", "archive", "I-001"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("must be done"));

    // resolve walks open → done and appends the resolution
    let out = run(
        d.path(),
        &[
            "issue",
            "resolve",
            "I-001",
            "--resolution",
            "fixed the leak",
            "--json",
        ],
    );
    assert_eq!(code(&out), 0);
    assert_eq!(json(&out)["item"]["status"], "done");
    let file = read(&wt, "issues/issue-0001-index-corruption.md");
    assert!(file.contains("status = \"done\""), "{file}");
    assert!(file.contains("## Resolution\n\nfixed the leak"), "{file}");

    // archive moves the file; list --archived surfaces it
    let out = run(d.path(), &["issue", "archive", "I-001", "--json"]);
    assert_eq!(code(&out), 0);
    assert_eq!(
        json(&out)["archived_to"],
        "issues/archive/issue-0001-index-corruption.md"
    );
    assert!(wt
        .join("issues/archive/issue-0001-index-corruption.md")
        .exists());
    assert!(!wt.join("issues/issue-0001-index-corruption.md").exists());
    let out = run(d.path(), &["issue", "list", "--json"]);
    let v = json(&out);
    let ids: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["id"].as_str())
        .collect();
    assert_eq!(ids, ["I-002"]);
    let out = run(d.path(), &["issue", "list", "--archived", "--json"]);
    assert_eq!(json(&out).as_array().unwrap().len(), 2);
    // re-archive → error
    let out = run(d.path(), &["issue", "archive", "I-001"]);
    assert_eq!(code(&out), 2);
    // mutating an archived item is refused
    let out = run(d.path(), &["issue", "comment", "I-001", "late note"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("archived"));

    // archived ids stay reserved
    let out = run(d.path(), &["issue", "new", "third", "--json"]);
    assert_eq!(json(&out)["item"]["id"], "I-003");
    // check is clean with the archive dir present
    let out = run(d.path(), &["check"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
}

// ---------- proposal / plan / adr ----------

#[test]
fn proposal_lifecycle() {
    let (d, wt) = project();
    let out = run(d.path(), &["proposal", "new", "the big idea", "--json"]);
    assert_eq!(code(&out), 0);
    assert_eq!(json(&out)["item"]["id"], "P-001");
    let file = read(&wt, "proposals/proposal-0001-the-big-idea.md");
    for frag in [
        "Goal & Overview",
        "Requirements",
        "Technical Design",
        "Open questions",
    ] {
        assert!(file.contains(frag), "{frag} in {file}");
    }

    let out = run(d.path(), &["proposal", "list"]);
    assert!(stdout(&out).contains("P-001 open pri:med the big idea"));

    // status mapping: accepted → done (walked through the machine)
    let out = run(
        d.path(),
        &["proposal", "status", "P-001", "accepted", "--json"],
    );
    assert_eq!(code(&out), 0);
    let v = json(&out);
    assert_eq!(v["to"], "done");
    assert_eq!(v["from"], "open");
    assert!(read(&wt, "proposals/proposal-0001-the-big-idea.md").contains("status = \"done\""));

    // done is terminal — rejecting it now fails
    let out = run(d.path(), &["proposal", "status", "P-001", "rejected"]);
    assert_eq!(code(&out), 2);
    // bogus word
    let out = run(d.path(), &["proposal", "status", "P-001", "lgtm"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("draft|accepted|rejected"));

    // rejected on a fresh proposal works directly
    let out = run(d.path(), &["proposal", "new", "alt"]);
    assert_eq!(code(&out), 0);
    let out = run(d.path(), &["proposal", "status", "P-002", "rejected"]);
    assert_eq!(code(&out), 0);
}

#[test]
fn plan_new_and_list() {
    let (d, wt) = project();
    let out = run(d.path(), &["plan", "new", "v3 rollout", "--pri", "high"]);
    assert_eq!(code(&out), 0);
    let file = read(&wt, "plans/pl-0001-v3-rollout.md");
    assert!(file.contains("id = \"PL-001\""), "{file}");
    assert!(file.contains("- [ ] Phase 1"), "{file}");
    let out = run(d.path(), &["plan", "list", "--json"]);
    let v = json(&out);
    assert_eq!(v.as_array().unwrap()[0]["id"], "PL-001");
    assert_eq!(v.as_array().unwrap()[0]["priority"], "high");
}

#[test]
fn adr_new_and_supersede() {
    let (d, wt) = project();
    run(d.path(), &["adr", "new", "use markdown"]);
    run(d.path(), &["adr", "new", "use yaml instead"]);
    let file = read(&wt, "adrs/0001-use-markdown.md");
    for frag in [
        "## Context",
        "## Decision",
        "## Consequences",
        "Status: **proposed**",
    ] {
        assert!(file.contains(frag), "{frag} in {file}");
    }
    let out = run(d.path(), &["adr", "list"]);
    assert_eq!(stdout(&out).lines().count(), 2);

    let out = run(
        d.path(),
        &["adr", "supersede", "ADR-0001", "--by", "ADR-0002", "--json"],
    );
    assert_eq!(code(&out), 0);
    let v = json(&out);
    assert_eq!(v["superseded"]["superseded_by"], "ADR-0002");
    assert_eq!(v["by"]["supersedes"], "ADR-0001");
    let old = read(&wt, "adrs/0001-use-markdown.md");
    assert!(old.contains("status = \"done\""), "{old}");
    assert!(old.contains("superseded_by = \"ADR-0002\""), "{old}");
    assert!(read(&wt, "adrs/0002-use-yaml-instead.md").contains("supersedes = \"ADR-0001\""));
    assert_eq!(
        git(&wt, &["log", "-1", "--format=%s"]),
        "chore: supersede ADR-0001 by ADR-0002"
    );

    // self-supersede and missing target
    let out = run(
        d.path(),
        &["adr", "supersede", "ADR-0002", "--by", "ADR-0002"],
    );
    assert_eq!(code(&out), 2);
    let out = run(
        d.path(),
        &["adr", "supersede", "ADR-0002", "--by", "ADR-0099"],
    );
    assert_eq!(code(&out), 2);
    // superseding a done (superseded) ADR again → terminal
    let out = run(
        d.path(),
        &["adr", "supersede", "ADR-0001", "--by", "ADR-0002"],
    );
    assert_eq!(code(&out), 2);
}

// ---------- item show / set ----------

#[test]
fn item_show_finds_everything() {
    let (d, _wt) = project();
    run(d.path(), &["todo", "add", "a task"]);
    run(d.path(), &["issue", "new", "an issue"]);
    let out = run(d.path(), &["item", "show", "T-001", "--json"]);
    assert_eq!(code(&out), 0);
    let v = json(&out);
    assert_eq!(v["location"], "TODO.md");
    assert!(v["body"].is_null());
    let out = run(d.path(), &["item", "show", "I-001", "--json"]);
    let v = json(&out);
    assert_eq!(v["location"], "issues/issue-0001-an-issue.md");
    assert!(v["body"].as_str().unwrap().contains("## Description"));
    // human output prints body
    let out = run(d.path(), &["item", "show", "I-001"]);
    assert!(stdout(&out).contains("## Description"));
    // missing → error
    let out = run(d.path(), &["item", "show", "P-009"]);
    assert_eq!(code(&out), 2);
    // archived items still show
    run(d.path(), &["issue", "resolve", "I-001"]);
    run(d.path(), &["issue", "archive", "I-001"]);
    let out = run(d.path(), &["item", "show", "I-001", "--json"]);
    assert_eq!(
        json(&out)["location"],
        "issues/archive/issue-0001-an-issue.md"
    );
}

#[test]
fn item_set_semantics() {
    let (d, wt) = project();
    run(d.path(), &["todo", "add", "a task"]);

    // labels add/remove via +/- and --unlabel
    let out = run(
        d.path(),
        &[
            "item", "set", "T-001", "--label", "+a", "--label", "b", "--json",
        ],
    );
    assert_eq!(code(&out), 0);
    let out = run(
        d.path(),
        &[
            "item",
            "set",
            "T-001",
            "--label",
            "-a",
            "--unlabel",
            "b",
            "--label",
            "c",
        ],
    );
    assert_eq!(code(&out), 0);
    let line = read(&wt, "TODO.md");
    assert!(line.contains("#c"), "{line}");
    assert!(!line.contains("#a"), "{line}");

    // blocks add/remove
    run(d.path(), &["item", "set", "T-001", "--blocks", "+I-001"]);
    let out = run(
        d.path(),
        &["item", "set", "T-001", "--unblock", "I-001", "--json"],
    );
    assert!(json(&out)["item"]["blocks"].is_null());
    let out = run(d.path(), &["item", "set", "T-001", "--blocks", "bogus"]);
    assert_eq!(code(&out), 2);

    // pri + owner
    let out = run(
        d.path(),
        &[
            "item", "set", "T-001", "--pri", "crit", "--owner", "devin", "--json",
        ],
    );
    let v = json(&out);
    assert_eq!(v["item"]["priority"], "critical");
    assert_eq!(v["item"]["owner"], "devin");

    // invalid transition → error names the valid targets
    let out = run(d.path(), &["item", "set", "T-001", "--status", "done"]);
    assert_eq!(code(&out), 2);
    let e = stderr(&out);
    assert!(e.contains("open → done"), "{e}");
    assert!(e.contains("claimed, in-progress, deferred"), "{e}");

    // one step at a time: open → in-progress → done removes the line
    run(
        d.path(),
        &["item", "set", "T-001", "--status", "in-progress"],
    );
    let out = run(d.path(), &["item", "set", "T-001", "--status", "done"]);
    assert_eq!(code(&out), 0);
    assert!(!read(&wt, "TODO.md").contains("T-001"));

    // deferred status moves the line to BACKLOG.md
    run(d.path(), &["todo", "add", "move me"]);
    let out = run(d.path(), &["item", "set", "T-002", "--status", "deferred"]);
    assert_eq!(code(&out), 0);
    assert!(!read(&wt, "TODO.md").contains("T-002"));
    assert!(read(&wt, "BACKLOG.md").contains("T-002"));

    // and open moves it back
    let out = run(d.path(), &["item", "set", "T-002", "--status", "open"]);
    assert_eq!(code(&out), 0);
    assert!(read(&wt, "TODO.md").contains("T-002"));

    // noop → usage error
    let out = run(d.path(), &["item", "set", "T-002"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("nothing to change"));

    // file-backed items get the same treatment
    run(d.path(), &["issue", "new", "a bug"]);
    let out = run(
        d.path(),
        &["item", "set", "I-001", "--status", "in-progress", "--json"],
    );
    assert_eq!(code(&out), 0);
    assert!(read(&wt, "issues/issue-0001-a-bug.md").contains("status = \"in-progress\""));
    let out = run(
        d.path(),
        &["item", "set", "I-001", "--label", "+perf", "--json"],
    );
    assert_eq!(json(&out)["item"]["labels"][0], "perf");

    // set on an archived item is refused
    run(d.path(), &["issue", "resolve", "I-001"]);
    run(d.path(), &["issue", "archive", "I-001"]);
    let out = run(d.path(), &["item", "set", "I-001", "--pri", "low"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("archived"));

    // unknown id
    let out = run(d.path(), &["item", "set", "PL-009", "--pri", "low"]);
    assert_eq!(code(&out), 2);
}

#[test]
fn kind_locks_and_missing_ids() {
    let (d, _wt) = project();
    run(d.path(), &["todo", "add", "t"]);
    run(d.path(), &["issue", "new", "i"]);
    // a task id to the issue verbs → usage error, not lookup
    let out = run(d.path(), &["issue", "resolve", "T-001"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("I-NNN"), "{}", stderr(&out));
    let out = run(d.path(), &["todo", "done", "I-001"]);
    assert_eq!(code(&out), 2);
    let out = run(d.path(), &["todo", "done", "T-099"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("no such task"));
}

#[test]
fn issue_body_from_file_and_stdin() {
    let (d, wt) = project();
    std::fs::write(d.path().join("body.md"), "# from file\n\nfull body\n").unwrap();
    let out = run(
        d.path(),
        &[
            "issue",
            "new",
            "from file",
            "--body",
            d.path().join("body.md").to_str().unwrap(),
        ],
    );
    assert_eq!(code(&out), 0);
    assert!(read(&wt, "issues/issue-0001-from-file.md").contains("full body"));

    // --body - reads stdin
    let out = Command::new(ARCA)
        .current_dir(d.path())
        .args(["issue", "new", "piped", "--body", "-"])
        .env("ARCA_CONFIG", "/nonexistent/arca.toml")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            c.stdin.as_mut().unwrap().write_all(b"piped body")?;
            c.wait_with_output()
        })
        .unwrap();
    assert_eq!(code(&out), 0);
    assert!(read(&wt, "issues/issue-0002-piped.md").contains("piped body"));

    // --body and --text conflict (clap)
    let out = run(
        d.path(),
        &["issue", "new", "x", "--body", "f", "--text", "y"],
    );
    assert_eq!(code(&out), 2);
    // missing body file → error
    let out = run(d.path(), &["issue", "new", "x", "--body", "/nope.md"]);
    assert_eq!(code(&out), 2);
}

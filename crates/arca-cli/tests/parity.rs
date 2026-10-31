//! End-to-end Jira-parity tests — `epic`, `milestone`, `absorb`
//! (comments/jira/github), `board --epic/--milestone`, and the
//! `--epic`/`--milestone` flags on `add`/`new`/`item set`, driving
//! the real binary against tempdir git repos.
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

// ---------- epic/milestone metadata ----------

#[test]
fn epic_milestone_flags_roundtrip() {
    let (d, wt) = project();
    // todo add --epic/--milestone → canonical line tokens
    let out = run(
        d.path(),
        &[
            "todo",
            "add",
            "grouped task",
            "--epic",
            "v2",
            "--milestone",
            "1.0",
            "--json",
        ],
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["item"]["epic"], "v2");
    assert_eq!(v["item"]["milestone"], "1.0");
    let todo = read(&wt, "TODO.md");
    assert!(todo.contains("epic:v2 · ms:1.0"), "{todo}");

    // issue new --epic → frontmatter keys after supersedes
    let out = run(
        d.path(),
        &["issue", "new", "a bug", "--epic", "v2", "--json"],
    );
    assert_eq!(code(&out), 0);
    let file = read(&wt, "issues/issue-0001-a-bug.md");
    assert!(file.contains("epic = \"v2\""), "{file}");

    // item set --epic/--milestone on a line item
    let out = run(d.path(), &["item", "set", "T-001", "--milestone", "2.0"]);
    assert_eq!(code(&out), 0);
    let todo = read(&wt, "TODO.md");
    assert!(todo.contains("ms:2.0"), "{todo}");
    // --epic "" clears
    let out = run(d.path(), &["item", "set", "T-001", "--epic", "", "--json"]);
    assert_eq!(code(&out), 0);
    assert!(json(&out)["item"]["epic"].is_null());
    let todo = read(&wt, "TODO.md");
    assert!(!todo.contains("epic:"), "{todo}");

    // invalid group names are errors on both flag paths
    let out = run(d.path(), &["todo", "add", "x", "--epic", "has space"]);
    assert_eq!(code(&out), 2);
    let out = run(d.path(), &["item", "set", "T-001", "--milestone", "a/b"]);
    assert_eq!(code(&out), 2);
}

#[test]
fn epic_and_milestone_views() {
    let (d, _wt) = project();
    run(d.path(), &["todo", "add", "one", "--epic", "v2"]);
    run(
        d.path(),
        &["todo", "add", "two", "--epic", "v2", "--milestone", "1.0"],
    );
    run(d.path(), &["todo", "add", "three", "--epic", "platform"]);
    run(d.path(), &["backlog", "add", "parked", "--epic", "v2"]);
    run(d.path(), &["issue", "new", "bug", "--milestone", "1.0"]);
    run(
        d.path(),
        &["item", "set", "T-002", "--status", "in-progress"],
    );

    // bare `arca epic` — groups with live-item counts
    let out = run(d.path(), &["epic", "--json"]);
    assert_eq!(code(&out), 0);
    let v = json(&out);
    assert_eq!(v["group"], "epic");
    let groups = v["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2, "{v}");
    assert_eq!(groups[0]["name"], "platform");
    assert_eq!(groups[0]["count"], 1);
    assert_eq!(groups[1]["name"], "v2");
    assert_eq!(groups[1]["count"], 3);
    // human form lists name + count
    let out = run(d.path(), &["epic"]);
    let s = stdout(&out);
    assert!(s.contains("v2  3"), "{s}");
    assert!(s.contains("platform  1"), "{s}");

    // `arca epic v2` — board-style columns, scoped to the epic
    let v = json(&run(d.path(), &["epic", "v2", "--json"]));
    assert_eq!(v["open"].as_array().unwrap().len(), 1);
    assert_eq!(v["in-progress"].as_array().unwrap()[0]["id"], "T-002");
    assert_eq!(v["backlog"].as_array().unwrap().len(), 1);
    // human board render shows the member
    let out = run(d.path(), &["epic", "v2"]);
    assert!(stdout(&out).contains("T-001 one"), "{}", stdout(&out));
    // empty group → friendly message, not an error
    let out = run(d.path(), &["epic", "nope"]);
    assert_eq!(code(&out), 0);
    assert!(stdout(&out).contains("no items"));
    // invalid name → usage error
    let out = run(d.path(), &["epic", "bad name"]);
    assert_eq!(code(&out), 2);

    // `arca milestone` mirrors it
    let v = json(&run(d.path(), &["milestone", "--json"]));
    assert_eq!(v["group"], "milestone");
    let groups = v["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["name"], "1.0");
    assert_eq!(groups[0]["count"], 2);
    let v = json(&run(d.path(), &["milestone", "1.0", "--json"]));
    assert_eq!(v["open"].as_array().unwrap().len(), 1); // I-001; T-002 moved
    assert_eq!(v["in-progress"].as_array().unwrap()[0]["id"], "T-002");

    // board --epic / --milestone filters (AND when both)
    let v = json(&run(d.path(), &["board", "--epic", "v2", "--json"]));
    let open = v["open"].as_array().unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0]["id"], "T-001");
    let v = json(&run(
        d.path(),
        &["board", "--epic", "v2", "--milestone", "1.0", "--json"],
    ));
    assert!(v["open"].as_array().unwrap().is_empty());
    assert_eq!(v["in-progress"].as_array().unwrap()[0]["id"], "T-002");
    // md table output
    let out = run(d.path(), &["epic", "v2", "--md"]);
    assert!(stdout(&out).starts_with('|'));
}

// ---------- absorb comments ----------

/// Seed a repo with comment kinds across a few files.
fn seed_comments(d: &Path) {
    std::fs::create_dir_all(d.join("src")).unwrap();
    std::fs::write(
        d.join("src/main.rs"),
        "fn main() {}\n// TODO: wire the index\n// FIXME(devin): handle empty repo\n",
    )
    .unwrap();
    std::fs::write(d.join("NOTES.md"), "# notes\nno markers here\n").unwrap();
    std::fs::write(d.join("project/IGNORED.md"), "TODO: not scanned\n").unwrap();
    std::fs::create_dir_all(d.join("docs")).unwrap();
    std::fs::write(d.join("docs/TODO.md"), "TODO: not scanned either\n").unwrap();
}

#[test]
fn absorb_comments_to_backlog_dedups() {
    let (d, wt) = project();
    seed_comments(d.path());

    // --dry reports only — no items, no commit
    let out = run(d.path(), &["absorb", "comments", "--dry", "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["source"], "comments");
    assert_eq!(v["dry"], true);
    assert_eq!(v["found"], 2, "{v}"); // project/ + docs/ excluded
    assert_eq!(v["created"], 0);
    let backlog = read(&wt, "BACKLOG.md");
    assert!(!backlog.contains("T-0"), "{backlog}");

    // real run → deferred backlog items with note = file:line
    let out = run(d.path(), &["absorb", "comments", "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["found"], 2);
    assert_eq!(v["created"], 2);
    assert_eq!(v["deduped"], 0);
    assert_eq!(v["committed"], true);
    let backlog = read(&wt, "BACKLOG.md");
    assert!(backlog.contains("wire the index"), "{backlog}");
    assert!(backlog.contains("handle empty repo"), "{backlog}");
    assert!(!backlog.contains("status:deferred"), "{backlog}"); // default status omitted
    assert_eq!(
        git(&wt, &["log", "-1", "--format=%s"]),
        "chore: absorb comments (2 items)"
    );
    // notes carry file:line
    let out = run(d.path(), &["item", "show", "T-001", "--json"]);
    assert_eq!(
        json(&out)["item"]["note"],
        "src/main.rs:2",
        "dedup note = file:line"
    );

    // re-run → all deduped, nothing created, no commit
    let out = run(d.path(), &["absorb", "comments", "--json"]);
    assert_eq!(code(&out), 0);
    let v = json(&out);
    assert_eq!(v["created"], 0);
    assert_eq!(v["deduped"], 2);
    assert_eq!(v["committed"], false);

    // --kinds narrows the sweep (already-absorbed TODO stays deduped)
    std::fs::write(d.path().join("src/extra.rs"), "/* HACK: tighten this */\n").unwrap();
    let out = run(
        d.path(),
        &["absorb", "comments", "--kinds", "HACK", "--json"],
    );
    let v = json(&out);
    assert_eq!(v["found"], 1);
    assert_eq!(v["created"], 1);

    // --to issues writes issue files
    std::fs::write(d.path().join("src/more.rs"), "// TODO more work\n").unwrap();
    let out = run(
        d.path(),
        &["absorb", "comments", "--to", "issues", "--json"],
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["created"], 1);
    assert_eq!(v["to"], "issues");
    let issue = wt.join("issues/issue-0001-more-work.md");
    assert!(issue.exists(), "issue file created");
    let f = read(&wt, "issues/issue-0001-more-work.md");
    assert!(f.contains("note = \"src/more.rs:1\""), "{f}");

    // --to validation + --path override
    let out = run(d.path(), &["absorb", "comments", "--to", "nowhere"]);
    assert_eq!(code(&out), 2);
    let sub = tempfile::tempdir().unwrap();
    std::fs::write(sub.path().join("x.c"), "// TODO subdir\n").unwrap();
    let out = run(
        d.path(),
        &[
            "absorb",
            "comments",
            "--path",
            sub.path().to_str().unwrap(),
            "--dry",
            "--json",
        ],
    );
    let v = json(&out);
    assert_eq!(v["found"], 1);
    // invalid kind rejected
    let out = run(
        d.path(),
        &["absorb", "comments", "--kinds", "TODO;rm -rf /", "--dry"],
    );
    assert_eq!(code(&out), 2);
    // a repo with nothing to find → found 0, created 0, still ok
    let (d2, _wt2) = project();
    let out = run(d2.path(), &["absorb", "comments", "--json"]);
    assert_eq!(code(&out), 0);
    let v = json(&out);
    assert_eq!(v["found"], 0);
    assert_eq!(v["created"], 0);
}

// ---------- absorb jira ----------

#[test]
fn absorb_jira_csv_imports_and_dedups() {
    let (d, wt) = project();
    let csv_path = d.path().join("jira.csv");
    std::fs::write(
        &csv_path,
        "Issue key,Summary,Status,Priority,Assignee,Labels,Epic Link,Fix Version/s\n\
         PROJ-1,Crash on startup,To Do,Highest,Luis Figueiredo,\"bug, help wanted\",EPIC-9,1.0\n\
         PROJ-2,Slow index,In Progress,High,devin,perf,,\n\
         PROJ-3,Blocked dep,Blocked,Medium,,ops,,\n\
         PROJ-4,Already shipped,Done,Low,,old,,\n\
         PROJ-5,Also done,Resolved,Medium,,old,,\n",
    )
    .unwrap();

    let out = run(
        d.path(),
        &["absorb", "jira", csv_path.to_str().unwrap(), "--json"],
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["source"], "jira");
    assert_eq!(v["found"], 5);
    assert_eq!(v["created"], 3, "{v}"); // done rows skipped
    assert_eq!(v["skipped_done"], 2);
    assert_eq!(v["archived"], 0);
    assert_eq!(
        git(&wt, &["log", "-1", "--format=%s"]),
        "chore: absorb jira (3 items)"
    );

    // status + priority + owner + labels + epic/ms mapped
    let i1 = read(&wt, "issues/issue-0001-crash-on-startup.md");
    for frag in [
        "status = \"open\"",
        "priority = \"critical\"",
        "owner = \"luis-figueiredo\"",
        "labels = [\"bug\", \"help-wanted\"]",
        "epic = \"EPIC-9\"",
        "milestone = \"1.0\"",
        "note = \"jira:PROJ-1\"",
    ] {
        assert!(i1.contains(frag), "{frag} in {i1}");
    }
    let i2 = read(&wt, "issues/issue-0002-slow-index.md");
    assert!(i2.contains("status = \"in-progress\""), "{i2}");
    let i3 = read(&wt, "issues/issue-0003-blocked-dep.md");
    assert!(i3.contains("status = \"blocked\""), "{i3}");

    // --include-done recreates done rows into archive/
    let out = run(
        d.path(),
        &[
            "absorb",
            "jira",
            csv_path.to_str().unwrap(),
            "--include-done",
            "--json",
        ],
    );
    assert_eq!(code(&out), 0);
    let v = json(&out);
    assert_eq!(v["created"], 2, "{v}"); // live rows are deduped now
    assert_eq!(v["deduped"], 3);
    assert_eq!(v["archived"], 2);
    assert!(wt
        .join("issues/archive/issue-0004-already-shipped.md")
        .exists());
    let done_file = read(&wt, "issues/archive/issue-0004-already-shipped.md");
    assert!(done_file.contains("status = \"done\""), "{done_file}");

    // third run — everything deduped
    let out = run(
        d.path(),
        &[
            "absorb",
            "jira",
            csv_path.to_str().unwrap(),
            "--include-done",
            "--json",
        ],
    );
    assert_eq!(json(&out)["created"], 0);
    assert_eq!(json(&out)["deduped"], 5);

    // check stays clean over imported files
    let out = run(d.path(), &["check"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
}

// ---------- absorb github ----------

#[test]
fn absorb_github_json_and_csv() {
    let (d, wt) = project();
    let json_path = d.path().join("issues.json");
    std::fs::write(
        &json_path,
        r#"[
          {"number": 42, "title": "index corruption", "state": "open",
           "labels": [{"name": "bug"}, "ui"], "assignee": {"login": "devin"},
           "body": "the index corrupts on rebuild", "milestone": {"title": "1.0"}},
          {"number": 43, "title": "old fix", "state": "closed", "labels": [], "body": ""}
        ]"#,
    )
    .unwrap();

    let out = run(
        d.path(),
        &["absorb", "github", json_path.to_str().unwrap(), "--json"],
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["source"], "github");
    assert_eq!(v["created"], 1, "{v}");
    assert_eq!(v["skipped_done"], 1);
    let f = read(&wt, "issues/issue-0001-index-corruption.md");
    for frag in [
        "owner = \"devin\"",
        "labels = [\"bug\", \"ui\"]",
        "milestone = \"1.0\"",
        "note = \"github:42\"",
        "the index corrupts on rebuild",
    ] {
        assert!(f.contains(frag), "{frag} in {f}");
    }
    assert_eq!(
        git(&wt, &["log", "-1", "--format=%s"]),
        "chore: absorb github (1 items)"
    );

    // csv path — same verb, header-tolerant columns
    let csv_path = d.path().join("gh.csv");
    std::fs::write(
        &csv_path,
        "number,title,state,labels,assignee,body\n\
         7,csv issue,open,\"csv-label,two\",devin,a body line\n\
         8,csv closed,closed,,,\n",
    )
    .unwrap();
    let out = run(
        d.path(),
        &[
            "absorb",
            "github",
            csv_path.to_str().unwrap(),
            "--include-done",
            "--json",
        ],
    );
    let v = json(&out);
    assert_eq!(v["created"], 2, "{v}");
    assert_eq!(v["archived"], 1);
    let f = read(&wt, "issues/issue-0002-csv-issue.md");
    assert!(f.contains("note = \"github:7\""), "{f}");
    assert!(f.contains("a body line"), "{f}");
    assert!(wt.join("issues/archive/issue-0003-csv-closed.md").exists());

    // dedup across formats — the json row was already imported
    let out = run(
        d.path(),
        &[
            "absorb",
            "github",
            json_path.to_str().unwrap(),
            "--include-done",
            "--json",
        ],
    );
    let v = json(&out);
    assert_eq!(v["created"], 1, "{v}"); // only the closed #43 lands now
    assert_eq!(v["deduped"], 1);
    assert_eq!(v["archived"], 1);
    assert!(wt.join("issues/archive/issue-0004-old-fix.md").exists());

    let out = run(d.path(), &["check"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
}

#[test]
fn manifest_lists_parity_verbs() {
    let out = run(std::path::Path::new("/"), &["manifest", "--json"]);
    let v = json(&out);
    let implemented = v["implemented"].as_array().unwrap();
    for verb in ["epic", "milestone", "absorb"] {
        assert!(implemented.contains(&Value::String(verb.into())), "{verb}");
    }
}

//! End-to-end continuity tests — `arca handoff` / `arca resume`
//! over `handoffs/<ID>.md`, plus `check`'s orphan/parse handling —
//! driving the real binary against tempdir git repos.
//!
//! `ARCA_CONFIG` is pinned at a nonexistent path so a stray user
//! config can't leak `project_dir` into discovery.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

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

fn run_stdin(dir: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(ARCA)
        .current_dir(dir)
        .args(args)
        .env("ARCA_CONFIG", "/nonexistent/arca.toml")
        .env_remove("ARCA_NO_COMMIT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
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

/// The `code` fields of a `check --json` error list.
fn codes(out: &Output) -> Vec<String> {
    json(out)["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["code"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn init_scaffolds_handoffs_dir() {
    let (_d, wt) = project();
    assert!(wt.join("handoffs/.gitkeep").exists());
    // an existing project without handoffs/ is repaired by check --fix
    let (d2, wt2) = project();
    std::fs::remove_dir_all(wt2.join("handoffs")).unwrap();
    let out = run(d2.path(), &["check", "--json"]);
    assert_eq!(code(&out), 1);
    let v = json(&out);
    let codes: Vec<&str> = v["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"missing-file"), "{codes:?}");
    let out = run(d2.path(), &["check", "--fix"]);
    assert_eq!(code(&out), 0);
    assert!(wt2.join("handoffs/.gitkeep").exists());
}

#[test]
fn handoff_every_kind_and_append_only() {
    let (d, wt) = project();
    // one item of every kind
    run(d.path(), &["todo", "add", "a task"]);
    run(d.path(), &["issue", "new", "an issue"]);
    run(d.path(), &["proposal", "new", "a proposal"]);
    run(d.path(), &["adr", "new", "a decision"]);
    run(d.path(), &["plan", "new", "a plan"]);

    for id in ["T-001", "I-001", "P-001", "ADR-0001", "PL-001"] {
        let out = run(
            d.path(),
            &[
                "handoff",
                id,
                "--note",
                "Done: x. Next: y.",
                "--actor",
                "devin",
                "--session",
                "cli:test",
                "--json",
            ],
        );
        assert_eq!(code(&out), 0, "{id}: {}", stderr(&out));
        let v = json(&out);
        assert_eq!(v["path"], format!("handoffs/{id}.md"));
        assert_eq!(v["entry"]["actor"], "devin");
        assert_eq!(v["entry"]["session"], "cli:test");
        assert_eq!(v["entry"]["note"], "Done: x. Next: y.");
        assert_eq!(v["committed"], true);
        let file = read(&wt, &format!("handoffs/{id}.md"));
        assert!(file.contains(&format!("item = \"{id}\"")), "{file}");
        assert!(file.contains(&format!("# Handoffs — {id}")), "{file}");
        assert!(file.contains(" · devin · session:cli:test"), "{file}");
    }
    assert_eq!(
        git(&wt, &["log", "-1", "--format=%s"]),
        "chore: handoff PL-001"
    );

    // append-only: second entry lands after the first, bytes kept
    let before = read(&wt, "handoffs/T-001.md");
    let out = run(
        d.path(),
        &["handoff", "T-001", "--note", "second note", "--json"],
    );
    assert_eq!(code(&out), 0);
    let v = json(&out);
    // default actor is `cli`; no session key
    assert_eq!(v["entry"]["actor"], "cli");
    assert!(v["entry"].as_object().unwrap().get("session").is_none());
    let after = read(&wt, "handoffs/T-001.md");
    assert!(after.starts_with(&before), "{after}");
    assert!(after.contains("\n## "), "{after}");
    assert!(after.contains("second note"), "{after}");

    // --note - reads stdin
    let out = run_stdin(
        d.path(),
        &["handoff", "T-001", "--note", "-"],
        "stdin note\n",
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(read(&wt, "handoffs/T-001.md").contains("stdin note"));
}

#[test]
fn handoff_errors() {
    let (d, _wt) = project();
    // missing item
    let out = run(d.path(), &["handoff", "T-009", "--note", "x"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("no such item"), "{}", stderr(&out));
    // bad session id
    run(d.path(), &["todo", "add", "t"]);
    let out = run(
        d.path(),
        &["handoff", "T-001", "--note", "x", "--session", "a·b"],
    );
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("session"), "{}", stderr(&out));
    // empty note
    let out = run(d.path(), &["handoff", "T-001", "--note", "  "]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("empty"), "{}", stderr(&out));
    // a `##` inside the note would corrupt the entry grammar
    let out = run(
        d.path(),
        &["handoff", "T-001", "--note", "a\n\n## 2020-01-01 · x\nb"],
    );
    assert_eq!(code(&out), 2, "{}", stdout(&out));
    // archived items still take handoffs (appended to the live file)
    run(d.path(), &["issue", "new", "i"]);
    run(d.path(), &["issue", "resolve", "I-001"]);
    run(d.path(), &["issue", "archive", "I-001"]);
    let out = run(d.path(), &["handoff", "I-001", "--note", "late note"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(d.path().join("project/handoffs/I-001.md").exists());
}

#[test]
fn resume_json_and_prompt_block() {
    let (d, wt) = project();
    run(d.path(), &["todo", "add", "blocker one"]);
    run(d.path(), &["issue", "new", "the bug", "--blocks", "T-001"]);
    run(
        d.path(),
        &[
            "handoff",
            "I-001",
            "--note",
            "Done: repro. Next: fix.",
            "--actor",
            "devin",
            "--session",
            "cli:abc123",
        ],
    );
    let out = run(d.path(), &["resume", "I-001", "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["item"]["id"], "I-001");
    assert_eq!(v["location"], "issues/issue-0001-the-bug.md");
    assert_eq!(
        v["blocks_titles"][0],
        serde_json::json!({"id": "T-001", "title": "blocker one"})
    );
    assert_eq!(v["handoffs"].as_array().unwrap().len(), 1);
    assert_eq!(v["handoffs"][0]["session"], "cli:abc123");
    assert_eq!(v["handoffs_total"], 1);
    // the issue template's `- [ ] Action item` line is open
    assert_eq!(v["open_checklist"][0], "Action item");

    // human output is the compact prompt block
    let out = run(d.path(), &["resume", "I-001"]);
    let s = stdout(&out);
    assert!(s.contains("I-001 open pri:med the bug"), "{s}");
    assert!(s.contains("blocks:"), "{s}");
    assert!(s.contains("T-001 blocker one"), "{s}");
    assert!(s.contains("handoffs:"), "{s}");
    assert!(s.contains("Done: repro. Next: fix."), "{s}");
    assert!(s.contains("open checklist:"), "{s}");

    // dangling blocks report a null title
    run(d.path(), &["issue", "new", "dangling", "--blocks", "T-099"]);
    let out = run(d.path(), &["resume", "I-002", "--json"]);
    assert_eq!(json(&out)["blocks_titles"][0]["title"], Value::Null);

    // T-001 has no handoffs/blocks/checklist → empty sections
    let out = run(d.path(), &["resume", "T-001", "--json"]);
    assert_eq!(code(&out), 0);
    let v = json(&out);
    assert!(v["handoffs"].as_array().unwrap().is_empty());
    assert!(v["blocks_titles"].as_array().unwrap().is_empty());
    assert!(v["open_checklist"].as_array().unwrap().is_empty());
    let out = run(d.path(), &["resume", "T-001"]);
    let s = stdout(&out);
    assert!(!s.contains("handoffs:"), "{s}");

    // missing item errors
    let out = run(d.path(), &["resume", "P-009"]);
    assert_eq!(code(&out), 2);

    // archived items resume too
    run(d.path(), &["issue", "resolve", "I-001"]);
    run(d.path(), &["issue", "archive", "I-001"]);
    let out = run(d.path(), &["resume", "I-001", "--json"]);
    assert_eq!(code(&out), 0);
    assert!(json(&out)["location"].as_str().unwrap().contains("archive"));
    let _ = wt;
}

#[test]
fn resume_last_caps_entries() {
    let (d, _wt) = project();
    run(d.path(), &["todo", "add", "t"]);
    for n in 1..=4 {
        run(
            d.path(),
            &["handoff", "T-001", "--note", &format!("note {n}")],
        );
    }
    let out = run(d.path(), &["resume", "T-001", "--last", "2", "--json"]);
    let v = json(&out);
    let hs = v["handoffs"].as_array().unwrap();
    assert_eq!(hs.len(), 2);
    // latest two, in file order
    assert_eq!(hs[0]["note"], "note 3");
    assert_eq!(hs[1]["note"], "note 4");
    assert_eq!(v["handoffs_total"], 4);
    // default cap is 10 — all 4 shown
    let out = run(d.path(), &["resume", "T-001", "--json"]);
    assert_eq!(json(&out)["handoffs"].as_array().unwrap().len(), 4);
    // human output shows the truncation marker
    let out = run(d.path(), &["resume", "T-001", "--last", "1"]);
    let s = stdout(&out);
    assert!(s.contains("note 4"), "{s}");
    assert!(s.contains("… +3 earlier entries"), "{s}");
}

#[test]
fn check_orphans_and_malformed_handoffs() {
    let (d, wt) = project();
    run(d.path(), &["todo", "add", "t"]);
    run(d.path(), &["handoff", "T-001", "--note", "keep me"]);
    // done removes the task — the handoff file becomes an orphan
    run(
        d.path(),
        &["item", "set", "T-001", "--status", "in-progress"],
    );
    run(d.path(), &["item", "set", "T-001", "--status", "done"]);
    let out = run(d.path(), &["check", "--json"]);
    assert_eq!(code(&out), 1);
    assert!(codes(&out).contains(&"orphan-handoff".into()));
    // --fix moves it to handoffs/archive/ — knowledge kept, dir clean
    let out = run(d.path(), &["check", "--fix"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(!wt.join("handoffs/T-001.md").exists());
    let archived = read(&wt, "handoffs/archive/T-001.md");
    assert!(archived.contains("keep me"), "{archived}");
    // an archived item's handoff is NOT an orphan
    run(d.path(), &["issue", "new", "i"]);
    run(d.path(), &["handoff", "I-001", "--note", "n"]);
    run(d.path(), &["issue", "resolve", "I-001"]);
    run(d.path(), &["issue", "archive", "I-001"]);
    let out = run(d.path(), &["check", "--json"]);
    let found = codes(&out);
    assert!(
        !found.contains(&"orphan-handoff".into()),
        "archived items keep live handoffs: {found:?}"
    );

    // malformed entry heading → parse finding, not fixable
    std::fs::write(
        wt.join("handoffs/I-002.md"),
        "+++\nitem = \"I-002\"\n+++\n\n# Handoffs — I-002\n\n## not a heading\nnote\n",
    )
    .unwrap();
    let out = run(d.path(), &["check", "--json"]);
    assert_eq!(code(&out), 1);
    let v = json(&out);
    let parse_finding = v["errors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["code"] == "parse" && e["file"] == "handoffs/I-002.md");
    assert!(parse_finding.is_some(), "{v}");
    // non-canonical filename gets renamed by --fix (the item must
    // exist, else it's an orphan first)
    run(d.path(), &["todo", "add", "t2"]); // T-002 exists
    std::fs::write(
        wt.join("handoffs/T-2.md"),
        "+++\nitem = \"T-002\"\n+++\n\n# Handoffs — T-002\n",
    )
    .unwrap();
    let out = run(d.path(), &["check", "--json"]);
    assert!(codes(&out).contains(&"filename".into()));
    let out = run(d.path(), &["check", "--fix"]);
    assert!(wt.join("handoffs/T-002.md").exists(), "{}", stderr(&out));
    assert!(!wt.join("handoffs/T-2.md").exists());
    // unexpected junk in the dir
    std::fs::write(wt.join("handoffs/notes.txt"), "x").unwrap();
    let out = run(d.path(), &["check", "--json"]);
    assert!(codes(&out).contains(&"unexpected-file".into()));
}

#[test]
fn item_show_surfaces_handoff_count() {
    let (d, _wt) = project();
    run(d.path(), &["todo", "add", "t"]);
    let out = run(d.path(), &["item", "show", "T-001", "--json"]);
    assert_eq!(json(&out)["handoffs"], 0);
    run(d.path(), &["handoff", "T-001", "--note", "n"]);
    run(d.path(), &["handoff", "T-001", "--note", "n2"]);
    let out = run(d.path(), &["item", "show", "T-001", "--json"]);
    assert_eq!(json(&out)["handoffs"], 2);
    let out = run(d.path(), &["item", "show", "T-001"]);
    assert!(stdout(&out).contains("handoffs: 2"));
}

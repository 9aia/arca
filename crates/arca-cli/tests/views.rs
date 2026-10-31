//! End-to-end view tests — `schema`, `status`, `board`, `report`,
//! `index`, `org` driving the real binary against tempdir git
//! repos. `ARCA_HOME` pins the index at a tempdir per test;
//! `ARCA_CONFIG` is pinned nonexistent so a stray user config
//! can't leak into discovery.

#![allow(clippy::unwrap_used)]

use std::path::Path;
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

fn run(dir: &Path, args: &[&str], arca_home: Option<&Path>) -> Output {
    let mut c = Command::new(ARCA);
    c.current_dir(dir)
        .args(args)
        .env("ARCA_CONFIG", "/nonexistent/arca.toml")
        .env_remove("ARCA_NO_COMMIT");
    match arca_home {
        Some(h) => c.env("ARCA_HOME", h),
        None => c.env_remove("ARCA_HOME"),
    };
    c.output().unwrap()
}

fn json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("bad json ({}): {}", e, String::from_utf8_lossy(&out.stdout)))
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

fn init(d: &Path) {
    let out = run(d, &["init", "--json"], None);
    assert_eq!(code(&out), 0, "init failed: {out:?}");
}

fn ok(d: &Path, args: &[&str]) -> Output {
    let out = run(d, args, None);
    assert_eq!(code(&out), 0, "{args:?} failed: {out:?}");
    out
}

/// A repo with init + a mixed ledger (name = dir basename).
fn seeded() -> tempfile::TempDir {
    let d = repo();
    init(d.path());
    ok(d.path(), &["todo", "add", "plain task"]);
    ok(d.path(), &["todo", "add", "hot task", "--pri", "high"]);
    ok(d.path(), &["issue", "new", "a bug", "--owner", "devin"]);
    ok(d.path(), &["backlog", "add", "someday idea"]);
    d
}

/// The project name `arca init` recorded (repo dir basename).
fn name_of(d: &Path) -> String {
    d.file_name().unwrap().to_string_lossy().into_owned()
}

#[test]
fn schema_document_snapshot() {
    let out = run(Path::new("/"), &["schema"], None);
    assert_eq!(code(&out), 0);
    let v = json(&out);
    assert_eq!(v["version"], 1);
    assert!(v["types"]["Item"].is_object());
    insta::assert_snapshot!(serde_json::to_string_pretty(&v).unwrap());
}

#[test]
fn status_views() {
    let d = seeded();
    // human
    let out = ok(d.path(), &["status"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("tasks 2o/0c/0ip/0b"), "{s}");
    assert!(s.contains("backlog 1"), "{s}");
    assert!(s.contains("issues 1o"), "{s}");
    // json
    let v = json(&ok(d.path(), &["status", "--json"]));
    assert_eq!(v["counts"]["tasks"]["open"], 2);
    assert_eq!(v["counts"]["backlog"], 1);
    assert_eq!(v["counts"]["issues"]["open"], 1);
    // prompt
    let out = ok(d.path(), &["status", "--prompt"]);
    let s = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = s.lines().collect();
    assert!(lines.len() <= 60, "{}", lines.len());
    assert!(lines[0].starts_with("repo:"), "{s}");
    assert!(s.contains("open:"), "{s}");
    assert!(s.contains("T-002 high hot task"), "{s}");
    assert!(!s.contains("someday idea"), "deferred stays out: {s}");
}

#[test]
fn prompt_block_stays_under_sixty_lines() {
    let d = repo();
    init(d.path());
    // hand-seed a huge TODO.md — 80 open tasks
    let wt = d.path().join("project");
    let name = name_of(d.path());
    let mut todo = format!("# TODO — {name}\n\n## big\n\n");
    for n in 1..=80 {
        todo.push_str(&format!("- [ ] T-{n:03} task number {n}\n"));
    }
    std::fs::write(wt.join("TODO.md"), todo).unwrap();
    std::fs::write(
        wt.join("meta.toml"),
        format!("name = \"{name}\"\n\n[next]\nt = 81\n"),
    )
    .unwrap();
    let out = ok(d.path(), &["status", "--prompt"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.lines().count() <= 60, "{}", s.lines().count());
    assert!(s.contains("… +"), "{s}");
}

#[test]
fn board_views() {
    let d = seeded();
    ok(
        d.path(),
        &["item", "set", "T-002", "--status", "in-progress"],
    );
    ok(
        d.path(),
        &["item", "set", "I-001", "--status", "in-progress"],
    );
    ok(d.path(), &["item", "set", "I-001", "--status", "blocked"]);
    // human grid
    let out = ok(d.path(), &["board"]);
    let s = String::from_utf8_lossy(&out.stdout);
    for col in ["Backlog", "Open", "Claimed", "In Progress", "Blocked"] {
        assert!(s.contains(col), "{col} missing:\n{s}");
    }
    assert!(s.contains("T-002 hot task"), "{s}");
    assert!(s.contains("I-001 a bug"), "{s}");
    // done items have no column
    // md output — padded markdown table
    let out = ok(d.path(), &["board", "--md"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.lines().next().unwrap_or("").starts_with('|'), "{s}");
    assert!(s.contains("Backlog"), "{s}");
    assert!(s.contains("---"), "{s}");
    // json columns map
    let v = json(&ok(d.path(), &["board", "--json"]));
    assert_eq!(v["backlog"].as_array().unwrap().len(), 1);
    assert_eq!(v["open"].as_array().unwrap().len(), 1);
    assert_eq!(v["in-progress"].as_array().unwrap()[0]["id"], "T-002");
    assert_eq!(v["blocked"].as_array().unwrap()[0]["id"], "I-001");
    assert!(v["claimed"].as_array().unwrap().is_empty());
}

#[test]
fn report_aging_and_velocity() {
    let d = seeded();
    // synthetic history: one add (todo add) + one done
    ok(d.path(), &["todo", "done", "T-001"]);
    let v = json(&ok(d.path(), &["report", "--json"]));
    assert!(v["aging"]["<7d"].as_u64().unwrap() >= 1);
    let vel = v["velocity"].as_array().unwrap();
    let new: u64 = vel.iter().map(|w| w["new"].as_u64().unwrap()).sum();
    let done: u64 = vel.iter().map(|w| w["done"].as_u64().unwrap()).sum();
    // 2 todo adds + 1 issue new + 1 backlog add = 4 creations;
    // 1 todo done = 1 completion
    assert_eq!(new, 4, "{vel:?}");
    assert_eq!(done, 1, "{vel:?}");
    // human
    let out = ok(d.path(), &["report"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("aging:"), "{s}");
    assert!(s.contains("velocity:"), "{s}");
    assert!(s.contains("+"), "{s}");
}

#[test]
fn index_add_list_rebuild_remove() {
    let home = tempfile::tempdir().unwrap();
    let d = seeded();
    let canon = d.path().canonicalize().unwrap();
    // add
    let out = run(
        d.path(),
        &["index", "add", &canon.to_string_lossy(), "--json"],
        Some(home.path()),
    );
    assert_eq!(code(&out), 0, "{out:?}");
    let v = json(&out);
    assert_eq!(v["registered"]["name"], name_of(d.path()));
    assert_eq!(v["snapshot"]["items"].as_u64().unwrap(), 4);
    // index files exist under ARCA_HOME
    assert!(home.path().join("index.redb").exists());
    assert!(home.path().join("repos.toml").exists());
    // list — fresh right after add
    let out = run(d.path(), &["index", "list", "--json"], Some(home.path()));
    let v = json(&out);
    assert_eq!(v[0]["stale"], false);
    // a ledger change moves HEAD → snapshot goes stale
    ok(d.path(), &["todo", "add", "another"]);
    let out = run(d.path(), &["index", "list", "--json"], Some(home.path()));
    assert_eq!(json(&out)[0]["stale"], true);
    // rebuild restores freshness
    let out = run(
        d.path(),
        &["index", "rebuild", &canon.to_string_lossy(), "--json"],
        Some(home.path()),
    );
    assert_eq!(code(&out), 0, "{out:?}");
    let v = json(&out);
    assert_eq!(v[0]["status"], "rebuilt");
    assert_eq!(v[0]["snapshot"]["items"].as_u64().unwrap(), 5);
    let out = run(d.path(), &["index", "list", "--json"], Some(home.path()));
    assert_eq!(json(&out)[0]["stale"], false);
    // remove
    let out = run(
        d.path(),
        &["index", "remove", &canon.to_string_lossy()],
        Some(home.path()),
    );
    assert_eq!(code(&out), 0);
    let v = json(&run(
        d.path(),
        &["index", "list", "--json"],
        Some(home.path()),
    ));
    assert!(v.as_array().unwrap().is_empty());
    // unregistered remove errors
    let out = run(
        d.path(),
        &["index", "remove", &canon.to_string_lossy()],
        Some(home.path()),
    );
    assert_eq!(code(&out), 2);
}

#[test]
fn index_rejects_non_repos() {
    let home = tempfile::tempdir().unwrap();
    let plain = tempfile::tempdir().unwrap();
    let out = run(
        plain.path(),
        &["index", "add", &plain.path().to_string_lossy()],
        Some(home.path()),
    );
    assert_eq!(code(&out), 2);
    // rebuild of an unregistered repo errors
    let d = seeded();
    let canon = d.path().canonicalize().unwrap();
    let out = run(
        d.path(),
        &["index", "rebuild", &canon.to_string_lossy()],
        Some(home.path()),
    );
    assert_eq!(code(&out), 2);
    assert!(String::from_utf8_lossy(&out.stderr).contains("not registered"));
}

#[test]
fn org_status_and_board_across_repos() {
    let home = tempfile::tempdir().unwrap();
    let a = seeded();
    let b = seeded();
    for r in [&a, &b] {
        let canon = r.path().canonicalize().unwrap();
        let out = run(
            r.path(),
            &["index", "add", &canon.to_string_lossy()],
            Some(home.path()),
        );
        assert_eq!(code(&out), 0, "{out:?}");
    }
    let (na, nb) = (name_of(a.path()), name_of(b.path()));
    let out = run(a.path(), &["org", "status", "--json"], Some(home.path()));
    assert_eq!(code(&out), 0, "{out:?}");
    let v = json(&out);
    assert_eq!(v["repos"].as_array().unwrap().len(), 2);
    assert_eq!(v["totals"]["tasks"]["open"], 4); // 2 per repo
    assert_eq!(v["totals"]["backlog"], 2);
    assert_eq!(v["totals"]["issues"]["open"], 2);
    // owner grouping: devin owns I-001 in both repos
    let owners = &v["owners"]["devin"].as_array().unwrap();
    assert_eq!(owners.len(), 2, "{owners:?}");
    assert!(owners[0].as_str().unwrap().contains("I-001"));
    // org board — repo-prefixed cards (2 tasks + 1 issue open each)
    let v = json(&run(
        a.path(),
        &["org", "board", "--json"],
        Some(home.path()),
    ));
    assert_eq!(v["open"].as_array().unwrap().len(), 6);
    let repos: std::collections::BTreeSet<&str> = v["open"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["repo"].as_str().unwrap())
        .collect();
    let mut want = vec![na.clone(), nb.clone()];
    want.sort();
    assert_eq!(repos.iter().copied().collect::<Vec<_>>(), want);
    // human org board mentions repo prefixes
    let out = run(a.path(), &["org", "board"], Some(home.path()));
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains(&format!("{na}:T-")), "{s}");
    assert!(s.contains(&format!("{nb}:T-")), "{s}");
    // org status auto-rebuilds a stale repo
    ok(a.path(), &["todo", "add", "fresh work"]);
    let out = run(a.path(), &["org", "status", "--json"], Some(home.path()));
    let v = json(&out);
    let rebuilt: Vec<&Value> = v["repos"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["rebuilt"] == true)
        .collect();
    assert_eq!(rebuilt.len(), 1, "{v}");
    assert_eq!(v["totals"]["tasks"]["open"], 5);
}

#[test]
fn index_add_accepts_worktree_and_rebuild_all() {
    let home = tempfile::tempdir().unwrap();
    let d = seeded();
    let wt = d.path().join("project").canonicalize().unwrap();
    // registering the worktree path resolves to the repo root
    let out = run(
        d.path(),
        &["index", "add", &wt.to_string_lossy(), "--json"],
        Some(home.path()),
    );
    assert_eq!(code(&out), 0, "{out:?}");
    assert_eq!(
        json(&out)["registered"]["path"].as_str().unwrap(),
        d.path().canonicalize().unwrap().to_string_lossy()
    );
    // `index rebuild` with no path rebuilds every registered repo
    ok(d.path(), &["todo", "add", "one more"]);
    let out = run(d.path(), &["index", "rebuild", "--json"], Some(home.path()));
    let v = json(&out);
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["status"], "rebuilt");
    assert_eq!(v[0]["snapshot"]["items"].as_u64().unwrap(), 5);
    // human list line
    let out = run(d.path(), &["index", "list"], Some(home.path()));
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("fresh"), "{s}");
}

#[test]
fn org_empty_index() {
    let home = tempfile::tempdir().unwrap();
    let d = seeded();
    let out = run(d.path(), &["org", "status", "--json"], Some(home.path()));
    assert_eq!(code(&out), 0);
    let v = json(&out);
    assert_eq!(v["repos"].as_array().unwrap().len(), 0);
    assert_eq!(v["totals"]["tasks"]["open"], 0);
}

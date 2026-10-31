//! Id-preservation tests — `arca migrate` honoring filename
//! numbers when adopting frontmatter-less item files, and
//! `arca check --fix` repairing `id-title-mismatch` (the
//! renumbering repair), driving the real binary against tempdir
//! git repos.
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

fn write(wt: &Path, rel: &str, src: &str) {
    let p = wt.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, src).unwrap();
}

fn read(wt: &Path, rel: &str) -> String {
    std::fs::read_to_string(wt.join(rel)).unwrap()
}

fn adr_files(wt: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(wt.join("adrs"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|n| n.ends_with(".md"))
        .collect();
    v.sort();
    v
}

fn item_file(id: &str, title: &str, extra_fm: &str, body: &str) -> String {
    format!("+++\nid = \"{id}\"\ntitle = \"{title}\"\nstatus = \"open\"\npriority = \"medium\"\n{extra_fm}+++\n\n{body}")
}

/// `migrate` adoption honors the filename number —
/// `adrs/0001-legacy.md` becomes `ADR-0001`, not the next counter.
#[test]
fn migrate_adoption_honors_filename_number() {
    let (d, wt) = project();
    write(
        &wt,
        "adrs/0002-second.md",
        "# ADR-0002: Second decision\n\nBody.\n",
    );
    write(&wt, "adrs/0001-first.md", "# ADR-0001: First\n\nBody.\n");
    write(&wt, "issues/issue-0007-bug.md", "# Bug\n\nBody.\n");
    let out = run(d.path(), &["migrate", "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let names = adr_files(&wt);
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names[0].starts_with("0001-"), "{names:?}");
    assert!(names[1].starts_with("0002-"), "{names:?}");
    for (f, id) in [
        (names[0].as_str(), "ADR-0001"),
        (names[1].as_str(), "ADR-0002"),
    ] {
        assert!(read(&wt, &format!("adrs/{f}")).contains(&format!("id = \"{id}\"")));
    }
    let issue = read(&wt, "issues/issue-0007-bug.md");
    assert!(issue.contains("id = \"I-007\""), "{issue}");
    // meta counters reconcile past the honored numbers
    let meta = read(&wt, "meta.toml");
    assert!(meta.contains("adr = 3"), "{meta}");
    assert!(meta.contains("i = 8"), "{meta}");
}

/// Duplicate filename numbers: first claim wins; the loser probes
/// past numbers other adoptees still want — `0001-a`,`0001-b`,
/// `0002-c` land 1, 3, 2 with a warning.
#[test]
fn migrate_adoption_collision_probes() {
    let (d, wt) = project();
    write(&wt, "adrs/0001-a.md", "# Alpha\n\nBody.\n");
    write(&wt, "adrs/0001-b.md", "# Beta\n\nBody.\n");
    write(&wt, "adrs/0002-c.md", "# Gamma\n\nBody.\n");
    let out = run(d.path(), &["migrate", "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let j = json(&out);
    let warnings = j["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{j}");
    let files = adr_files(&wt);
    assert_eq!(files, ["0001-alpha.md", "0002-gamma.md", "0003-beta.md"]);
    for (f, id) in [
        ("0001-alpha.md", "ADR-0001"),
        ("0002-gamma.md", "ADR-0002"),
        ("0003-beta.md", "ADR-0003"),
    ] {
        let src = read(&wt, &format!("adrs/{f}"));
        assert!(src.contains(&format!("id = \"{id}\"")), "{src}");
    }
}

/// `check` flags a frontmatter id that disagrees with the title's
/// `ADR-NNNN:` prefix; `--fix` renumbers id + filename and repoints
/// index.md links.
#[test]
fn check_fixes_id_title_mismatch() {
    let (d, wt) = project();
    write(
        &wt,
        "adrs/0003-adr-0001-gha-compat.md",
        &item_file(
            "ADR-0003",
            "ADR-0001: GHA compat",
            "",
            "# ADR-0001: GHA compat\n\nBody.\n",
        ),
    );
    write(
        &wt,
        "index.md",
        "# index\n\n- [a](adrs/0003-adr-0001-gha-compat.md)\n",
    );
    // detect
    let out = run(d.path(), &["check", "--json"]);
    assert_eq!(code(&out), 1);
    let j = json(&out);
    let errs = j["errors"].as_array().unwrap();
    assert!(
        errs.iter()
            .any(|e| e["code"] == "id-title-mismatch" && e["fixable"] == true),
        "{j}"
    );
    // fix
    let out = run(d.path(), &["check", "--fix", "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let names = adr_files(&wt);
    assert_eq!(names, ["0001-adr-0001-gha-compat.md"]);
    let src = read(&wt, "adrs/0001-adr-0001-gha-compat.md");
    assert!(src.contains("id = \"ADR-0001\""), "{src}");
    let index = read(&wt, "index.md");
    assert!(
        index.contains("adrs/0001-adr-0001-gha-compat.md"),
        "{index}"
    );
}

/// The renumber repoints structured references — another item's
/// `blocks:` frontmatter and a TODO item's `blocks:` token —
/// while a held id stays untouched.
#[test]
fn check_fix_repoints_references() {
    let (d, wt) = project();
    write(
        &wt,
        "adrs/0003-adr-0001-a.md",
        &item_file("ADR-0003", "ADR-0001: A", "", "# ADR-0001: A\n\nBody.\n"),
    );
    write(
        &wt,
        "adrs/0002-b.md",
        &item_file(
            "ADR-0002",
            "B",
            "blocks = [\"ADR-0003\"]\n",
            "# B\n\nBody.\n",
        ),
    );
    write(
        &wt,
        "TODO.md",
        "# TODO — x\n\n- [ ] T-001 do · blocks:ADR-0003\n",
    );
    let out = run(d.path(), &["check", "--fix", "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let b = read(&wt, "adrs/0002-b.md");
    assert!(b.contains("blocks = [\"ADR-0001\"]"), "{b}");
    let todo = read(&wt, "TODO.md");
    assert!(todo.contains("blocks:ADR-0001"), "{todo}");
}

/// Two files claiming the same title number: the first (filename
/// order) wins the renumber; the loser is reported and left for
/// manual resolution.
#[test]
fn check_fix_collision_skips_loser() {
    let (d, wt) = project();
    write(
        &wt,
        "adrs/0003-adr-0001-a.md",
        &item_file("ADR-0003", "ADR-0001: A", "", "# ADR-0001: A\n\nBody.\n"),
    );
    write(
        &wt,
        "adrs/0004-adr-0001-b.md",
        &item_file("ADR-0004", "ADR-0001: B", "", "# ADR-0001: B\n\nBody.\n"),
    );
    let out = run(d.path(), &["check", "--fix", "--json"]);
    assert_eq!(code(&out), 1, "winner fixed, loser must remain");
    let j = json(&out);
    let errs = j["errors"].as_array().unwrap();
    let mismatch: Vec<&Value> = errs
        .iter()
        .filter(|e| e["code"] == "id-title-mismatch")
        .collect();
    assert_eq!(mismatch.len(), 1, "{j}");
    assert_eq!(mismatch[0]["file"], "adrs/0004-adr-0001-b.md");
    assert_eq!(mismatch[0]["fixable"], false);
    // winner landed on 0001; loser kept its fresh id
    let names = adr_files(&wt);
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names[0].starts_with("0001-"), "{names:?}");
    assert_eq!(names[1], "0004-adr-0001-b.md");
}

/// A title claiming a number another live item already owns is
/// reported and skipped — the holder keeps its id.
#[test]
fn check_fix_skips_when_number_held() {
    let (d, wt) = project();
    write(
        &wt,
        "adrs/0001-real.md",
        &item_file("ADR-0001", "Real", "", "# Real\n\nBody.\n"),
    );
    write(
        &wt,
        "adrs/0003-adr-0001-stale.md",
        &item_file(
            "ADR-0003",
            "ADR-0001: Stale",
            "",
            "# ADR-0001: Stale\n\nBody.\n",
        ),
    );
    let out = run(d.path(), &["check", "--fix", "--json"]);
    assert_eq!(code(&out), 1);
    let j = json(&out);
    let errs = j["errors"].as_array().unwrap();
    let m: Vec<&Value> = errs
        .iter()
        .filter(|e| e["code"] == "id-title-mismatch")
        .collect();
    assert_eq!(m.len(), 1, "{j}");
    assert_eq!(m[0]["fixable"], false, "{j}");
    assert!(m[0]["msg"].as_str().unwrap().contains("held"), "{j}");
    // nothing moved
    let names = adr_files(&wt);
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(
        names[0].starts_with("0001-") && names[1].starts_with("0003-"),
        "{names:?}"
    );
}

/// A frontmatter-less file whose name carries no number takes a
/// fresh counter alloc; a live handoff for a renumbered item is
/// repointed along with it.
#[test]
fn migrate_numberless_adoptee_allocs_and_check_moves_handoff() {
    let (d, wt) = project();
    write(&wt, "adrs/notes.md", "# Loose notes\n\nBody.\n");
    let out = run(d.path(), &["migrate", "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let names = adr_files(&wt);
    assert_eq!(names, ["0001-loose-notes.md"]);
    let src = read(&wt, "adrs/0001-loose-notes.md");
    assert!(src.contains("id = \"ADR-0001\""), "{src}");

    // now damage it: id drifts from its title, with a handoff attached
    write(
        &wt,
        "adrs/0007-adr-0001-loose.md",
        &item_file(
            "ADR-0007",
            "ADR-0001: Loose",
            "",
            "# ADR-0001: Loose\n\nBody.\n",
        ),
    );
    std::fs::remove_file(wt.join("adrs/0001-loose-notes.md")).unwrap();
    write(
        &wt,
        "handoffs/ADR-0007.md",
        "+++\nitem = \"ADR-0007\"\n+++\n\n# Handoffs — ADR-0007\n\n## 2026-01-01 · devin\nnote\n",
    );
    let out = run(d.path(), &["check", "--fix", "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(!wt.join("handoffs/ADR-0007.md").exists());
    let h = read(&wt, "handoffs/ADR-0001.md");
    assert!(h.contains("item = \"ADR-0001\""), "{h}");
    assert!(h.contains("# Handoffs — ADR-0001"), "{h}");
}

/// A title/H1 disagreeing with each other is reported without a
/// fix (ambiguous which number is real).
#[test]
fn check_conflict_claim_not_fixed() {
    let (d, wt) = project();
    write(
        &wt,
        "adrs/0003-x.md",
        &item_file("ADR-0003", "ADR-0001: X", "", "# ADR-0002: X\n\nBody.\n"),
    );
    let out = run(d.path(), &["check", "--fix", "--json"]);
    assert_eq!(code(&out), 1);
    let j = json(&out);
    let errs = j["errors"].as_array().unwrap();
    assert!(
        errs.iter()
            .any(|e| e["code"] == "id-title-mismatch" && e["fixable"] == false),
        "{j}"
    );
    // the file keeps id ADR-0003 — no renumber happened
    let names = adr_files(&wt);
    assert_eq!(names.len(), 1, "{names:?}");
    let src = read(&wt, &format!("adrs/{}", names[0]));
    assert!(src.contains("id = \"ADR-0003\""), "{src}");
}

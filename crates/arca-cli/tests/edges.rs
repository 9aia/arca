//! Edge-path e2e coverage — error arms and rarely-hit code paths
//! across verbs, driven against the real binary on tempdir repos.
//! `ARCA_CONFIG` is pinned nonexistent and `ARCA_HOME` points at a
//! per-test tempdir unless a test says otherwise.

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
    run_env(dir, args, &[])
}

fn run_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(ARCA);
    c.current_dir(dir)
        .args(args)
        .env("ARCA_CONFIG", "/nonexistent/arca.toml")
        .env_remove("ARCA_NO_COMMIT")
        .env_remove("ARCA_HOME");
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
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

fn write(wt: &Path, rel: &str, src: &str) {
    let p = wt.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, src).unwrap();
}

/// A repo with an initialized project worktree.
fn project() -> (tempfile::TempDir, PathBuf) {
    let d = repo();
    let out = run(d.path(), &["init", "--json"]);
    assert_eq!(code(&out), 0, "init failed: {}", stderr(&out));
    let wt = PathBuf::from(json(&out)["worktree"].as_str().unwrap());
    (d, wt)
}

// ---------- discover ----------

#[test]
fn discover_dir_flag_rejects_a_file() {
    let d = repo();
    let out = run(d.path(), &["--dir", "README.md", "--root", ".", "status"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("is not a directory"), "{out:?}");
}

#[test]
fn discover_via_config_project_dir() {
    let (_d, _wt) = project();
    let d = _d.path().to_path_buf();
    // a ledger dir that ISN'T `project/` — only the config's
    // project_dir points arca at it (walk-up can't)
    std::fs::create_dir_all(d.join("other")).unwrap();
    std::fs::write(d.join("other/TODO.md"), "# TODO — cfg\n").unwrap();
    let cfg = d.join("cfg.toml");
    std::fs::write(&cfg, "project_dir = \"other\"\n").unwrap();
    let out = run_env(
        &d,
        &["status", "--json"],
        &[("ARCA_CONFIG", cfg.to_str().unwrap())],
    );
    assert_eq!(code(&out), 0, "{out:?}");
    // "other" proves the config arm ran — walk-up only knows
    // `project/`
    assert_eq!(json(&out)["repo"], "other", "{out:?}");
}

#[test]
fn discover_config_without_project_dir_and_missing() {
    let (d, _wt) = project();
    // config exists but has no project_dir → `if let Some` else
    let cfg = d.path().join("cfg1.toml");
    std::fs::write(&cfg, "nothing = \"here\"\n").unwrap();
    let out = run_env(
        d.path(),
        &["status", "--json"],
        &[("ARCA_CONFIG", cfg.to_str().unwrap())],
    );
    assert_eq!(code(&out), 0, "{out:?}");
    // project_dir points somewhere that doesn't qualify → falls
    // through to the ancestor walk
    let cfg = d.path().join("cfg2.toml");
    std::fs::write(&cfg, "project_dir = \"missing\"\n").unwrap();
    let out = run_env(
        d.path(),
        &["status", "--json"],
        &[("ARCA_CONFIG", cfg.to_str().unwrap())],
    );
    assert_eq!(code(&out), 0, "{out:?}");
    // a valid-but-empty config → `project_dir` is None
    let cfg = d.path().join("cfg3.toml");
    std::fs::write(&cfg, "# no keys\n").unwrap();
    let out = run_env(
        d.path(),
        &["status", "--json"],
        &[("ARCA_CONFIG", cfg.to_str().unwrap())],
    );
    assert_eq!(code(&out), 0, "{out:?}");
    // not inside a repo at all → repo_root fails inside the cfg
    // block → discovery errors out
    let bare = tempfile::tempdir().unwrap();
    std::fs::write(bare.path().join("my.toml"), "project_dir = \"x\"\n").unwrap();
    let out = run_env(
        bare.path(),
        &["status", "--json"],
        &[("ARCA_CONFIG", bare.path().join("my.toml").to_str().unwrap())],
    );
    assert_eq!(code(&out), 2, "{out:?}");
}

#[test]
fn discover_from_inside_a_moved_worktree() {
    let (d, _wt) = project();
    // move the worktree off the `project/` name — ancestors of cwd
    // no longer offer `<x>/project`, so the enclosing-worktree arm
    // must find it
    git(d.path(), &["worktree", "move", "project", "ledger"]);
    let out = run(&d.path().join("ledger"), &["status", "--json"]);
    assert_eq!(code(&out), 0, "{out:?}");
}

#[test]
fn discover_explicit_root_flag() {
    let (d, _wt) = project();
    // --dir + --root together: the explicit-root arm
    let out = run(
        d.path(),
        &["--dir", "project", "--root", ".", "status", "--json"],
    );
    assert_eq!(code(&out), 0, "{out:?}");
}

#[test]
fn discover_from_inside_the_worktree() {
    let (_d, wt) = project();
    let nested = wt.join("issues");
    // running with cwd inside project/ resolves the enclosing
    // worktree — flags not needed
    let out = run(&nested, &["status", "--json"]);
    assert_eq!(code(&out), 0, "{out:?}");
}

// ---------- init ----------

#[test]
fn init_fails_when_branch_checked_out_elsewhere() {
    let d = repo();
    // create + check out the project branch in another worktree —
    // `git worktree add project project` then refuses
    arca_git_shim(d.path());
    git(d.path(), &["worktree", "add", "other", "project"]);
    let out = run(d.path(), &["init"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("worktree add"), "{out:?}");
    git(d.path(), &["worktree", "remove", "--force", "other"]);
}

/// Create the orphan `project` branch the way init does.
fn arca_git_shim(root: &Path) {
    let out = run(root, &["init", "--json"]);
    assert_eq!(code(&out), 0, "{out:?}");
    // drop the worktree but keep the branch
    git(root, &["worktree", "remove", "--force", "project"]);
}

#[test]
fn init_reports_gitignore_commit_failure() {
    let d = repo();
    // first init, then a commit-msg hook that only fails the
    // .gitignore commit (the worktree commit passes)
    let out = run(d.path(), &["init", "--json"]);
    assert_eq!(code(&out), 0);
    std::fs::write(
        d.path().join(".git/hooks/commit-msg"),
        "#!/bin/sh\ngrep -q 'ignore project/' \"$1\" && exit 1\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            d.path().join(".git/hooks/commit-msg"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    // remove the project/ line so ensure_gitignore re-appends it
    std::fs::write(d.path().join(".gitignore"), "target/\n").unwrap();
    git(d.path(), &["add", "-A"]);
    git(d.path(), &["commit", "-qm", "reset gitignore"]);
    let out = run(d.path(), &["init"]);
    assert_eq!(code(&out), 2, "{out:?}");
}

// ---------- commit / autocommit failure ----------

fn failing_commit_hook(root: &Path) {
    let hook = root.join(".git/hooks/commit-msg");
    std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn commit_and_verb_failures_surface() {
    let (d, wt) = project();
    // `arca commit` with a dirty worktree + failing hook → error
    write(&wt, "TODO.md", "# TODO — x\n\n- [ ] T-001 a\n");
    write(&wt, "stray.txt", "x");
    failing_commit_hook(d.path());
    let out = run(d.path(), &["commit"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("git commit failed"), "{out:?}");
    // a mutating verb's autocommit surfaces the same way
    let out = run(d.path(), &["todo", "add", "another task"]);
    assert_eq!(code(&out), 2);
    // absorb's shared finish tail too
    let csv = d.path().join("j.csv");
    std::fs::write(&csv, "Issue key,Summary,Status\nPROJ-1,One,Done\n").unwrap();
    let out = run(d.path(), &["absorb", "jira", csv.to_str().unwrap()]);
    assert_eq!(code(&out), 2);
    // new-file verbs + status/supersede paths — the items must
    // exist, so create them before the hook goes in
    let hook = d.path().join(".git/hooks/commit-msg");
    std::fs::remove_file(&hook).unwrap();
    assert_eq!(code(&run(d.path(), &["proposal", "new", "prop"])), 0);
    assert_eq!(code(&run(d.path(), &["adr", "new", "decision one"])), 0);
    assert_eq!(code(&run(d.path(), &["adr", "new", "decision two"])), 0);
    failing_commit_hook(d.path());
    let out = run(d.path(), &["issue", "new", "bug one"]);
    assert_eq!(code(&out), 2);
    let out = run(d.path(), &["proposal", "status", "P-001", "accepted"]);
    assert_eq!(code(&out), 2);
    let out = run(
        d.path(),
        &["adr", "supersede", "ADR-0001", "--by", "ADR-0002"],
    );
    assert_eq!(code(&out), 2);
}

// ---------- misc verb paths ----------

#[test]
fn gen_cli_docs_prints_markdown() {
    let out = run(Path::new("/"), &["gen-cli-docs"]);
    assert_eq!(code(&out), 0);
    assert!(String::from_utf8_lossy(&out.stdout).contains("arca"));
}

#[test]
fn group_epic_empty_and_board_empty() {
    let (d, _wt) = project();
    let out = run(d.path(), &["epic"]);
    assert_eq!(code(&out), 0);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("no epics"),
        "{out:?}"
    );
    let out = run(d.path(), &["board"]);
    assert_eq!(code(&out), 0);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("empty board"),
        "{out:?}"
    );
}

#[test]
fn status_human_lists_groups() {
    let (d, _wt) = project();
    assert_eq!(code(&run(d.path(), &["todo", "add", "work"])), 0);
    assert_eq!(
        code(&run(
            d.path(),
            &["item", "set", "T-001", "--status", "in-progress"]
        )),
        0
    );
    let out = run(d.path(), &["status"]);
    assert_eq!(code(&out), 0);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("in progress:"), "{s}");
    assert!(s.contains("T-001"), "{s}");
}

#[test]
fn report_human_paths() {
    let (d, _wt) = project();
    // fresh repo: the init commit isn't a chore-item commit →
    // no velocity history
    let out = run(d.path(), &["report"]);
    assert_eq!(code(&out), 0);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("no chore: history"),
        "{out:?}"
    );
    // a blocked item shows in the list
    assert_eq!(code(&run(d.path(), &["todo", "add", "stuck"])), 0);
    assert_eq!(
        code(&run(
            d.path(),
            &["item", "set", "T-001", "--status", "in-progress"]
        )),
        0
    );
    assert_eq!(
        code(&run(
            d.path(),
            &["item", "set", "T-001", "--status", "blocked"]
        )),
        0
    );
    let out = run(d.path(), &["report"]);
    assert_eq!(code(&out), 0);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("blocked:"), "{s}");
    assert!(s.contains("T-001"), "{s}");
}

#[test]
fn item_flags_edges() {
    let (d, _wt) = project();
    // NB: a rejected `add` still burns its id — the label-error
    // probes come after the real items exist
    assert_eq!(code(&run(d.path(), &["todo", "add", "a"])), 0);
    assert_eq!(code(&run(d.path(), &["todo", "add", "b"])), 0);
    // invalid labels error on both add and set
    let out = run(d.path(), &["todo", "add", "x", "--label", "bad label"]);
    assert_eq!(code(&out), 2, "{out:?}");
    let out = run(d.path(), &["item", "set", "T-001", "--label", "bad label"]);
    assert_eq!(code(&out), 2, "{out:?}");
    // removing a blocks dep via `-T-NNN`
    let out = run(d.path(), &["item", "set", "T-001", "--blocks", "-T-002"]);
    assert_eq!(code(&out), 0, "{out:?}");
    // adding to a named section twice hits the existing section
    assert_eq!(
        code(&run(d.path(), &["todo", "add", "s1", "--section", "later"])),
        0
    );
    assert_eq!(
        code(&run(d.path(), &["todo", "add", "s2", "--section", "later"])),
        0
    );
    let todo = std::fs::read_to_string(d.path().join("project/TODO.md")).unwrap();
    assert_eq!(todo.matches("## later").count(), 1, "{todo}");
}

#[test]
fn item_show_propagates_store_errors() {
    let (d, wt) = project();
    // an item file that's unparseable — item() errors, not NotFound
    write(
        &wt,
        "issues/issue-0001-broken.md",
        "+++\nid = \"I-001\"\ntitle = [bad\n+++\n",
    );
    let out = run(d.path(), &["item", "show", "I-001"]);
    assert_eq!(code(&out), 2, "{out:?}");
    // duplicate files for one id → an Invalid error, not NotFound
    write(
        &wt,
        "issues/issue-0002-a.md",
        "+++\nid = \"I-002\"\ntitle = \"a\"\nstatus = \"open\"\n+++\n",
    );
    write(
        &wt,
        "issues/issue-0002-b.md",
        "+++\nid = \"I-002\"\ntitle = \"b\"\nstatus = \"open\"\n+++\n",
    );
    let out = run(d.path(), &["item", "show", "I-002"]);
    assert_eq!(code(&out), 2, "{out:?}");
    assert!(stderr(&out).contains("duplicate"), "{out:?}");
}

#[test]
fn handoff_to_archived_file_stays_archived() {
    let (d, wt) = project();
    assert_eq!(code(&run(d.path(), &["issue", "new", "bug"])), 0);
    assert_eq!(
        code(&run(d.path(), &["handoff", "I-001", "--note", "first"])),
        0
    );
    // cold-storage the file; the next append lands in archive
    let arch = wt.join("handoffs/archive");
    std::fs::create_dir_all(&arch).unwrap();
    std::fs::rename(wt.join("handoffs/I-001.md"), arch.join("I-001.md")).unwrap();
    let out = run(
        d.path(),
        &["handoff", "I-001", "--note", "second", "--json"],
    );
    assert_eq!(code(&out), 0);
    let v = json(&out);
    assert_eq!(v["path"], "handoffs/archive/I-001.md", "{v}");
}

// ---------- absorb ----------

#[test]
fn absorb_comments_errors_and_dry_run() {
    let (d, _wt) = project();
    // --path must be a directory
    let out = run(d.path(), &["absorb", "comments", "--path", "README.md"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("not a directory"), "{out:?}");
    // invalid kind / --to
    let out = run(d.path(), &["absorb", "comments", "--kinds", "bad-kind"]);
    assert_eq!(code(&out), 2);
    let out = run(d.path(), &["absorb", "comments", "--to", "nowhere"]);
    assert_eq!(code(&out), 2);
    // a bare `// TODO` gets a synthesized title; human --dry prints
    std::fs::write(d.path().join("src.rs"), "// TODO\nfn main() {}\n").unwrap();
    let out = run(d.path(), &["absorb", "comments", "--dry"]);
    assert_eq!(code(&out), 0, "{out:?}");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("would be created"), "{s}");
    assert!(s.contains("TODO at src.rs"), "{s}");
    // real import into issues
    let out = run(
        d.path(),
        &["absorb", "comments", "--to", "issues", "--json"],
    );
    assert_eq!(code(&out), 0, "{out:?}");
    assert_eq!(json(&out)["created"], 1);
}

#[test]
fn absorb_comments_rg_failure_and_garbage_output() {
    let (d, _wt) = project();
    let bin = d.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // a fake rg that prints unparseable lines then exits 0
    let rg = bin.join("rg");
    std::fs::write(
        &rg,
        "#!/bin/sh\nprintf 'garbage line\\nsrc.rs:3:// TODO x\\n'\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&rg, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path_env = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let out = run_env(
        d.path(),
        &["absorb", "comments", "--json"],
        &[("PATH", &path_env)],
    );
    assert_eq!(code(&out), 0, "{out:?}");
    // fake rg exits non-zero → surfaced
    std::fs::write(&rg, "#!/bin/sh\necho boom >&2\nexit 2\n").unwrap();
    let out = run_env(d.path(), &["absorb", "comments"], &[("PATH", &path_env)]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("rg exited"), "{out:?}");
}

#[test]
fn absorb_jira_csv() {
    let (d, _wt) = project();
    let csv = d.path().join("j.csv");
    std::fs::write(
        &csv,
        "Issue key,Summary,Status,Labels\n\
         PROJ-1,First,Done,\"bug,bug\"\n\
         PROJ-2,Second,In Progress,ui\n\
         ,,,\n\
         PROJ-1,First,Done,bug\n",
    )
    .unwrap();
    let out = run(
        d.path(),
        &[
            "absorb",
            "jira",
            csv.to_str().unwrap(),
            "--include-done",
            "--json",
        ],
    );
    assert_eq!(code(&out), 0, "{out:?}");
    let v = json(&out);
    assert_eq!(v["created"], 3);
    assert_eq!(v["archived"], 2);
    // a re-run dedupes every row by its jira:KEY note
    let out = run(
        d.path(),
        &[
            "absorb",
            "jira",
            csv.to_str().unwrap(),
            "--include-done",
            "--json",
        ],
    );
    assert_eq!(code(&out), 0, "{out:?}");
    let v = json(&out);
    assert_eq!(v["created"], 0);
    assert_eq!(v["deduped"], 3);
    // the done rows filed straight into archive/
    assert!(d.path().join("project/issues/archive").is_dir());
}

#[test]
fn absorb_github_sniffing_and_states() {
    let (d, _wt) = project();
    // extension-less file sniffed as JSON
    let dump = d.path().join("dump");
    std::fs::write(
        &dump,
        r#"[{"number": 1, "title": "open one", "state": "in progress"},
            {"number": 2, "title": "closed two", "state": "closed"},
            {"state": "open"}]"#,
    )
    .unwrap();
    let out = run(
        d.path(),
        &["absorb", "github", dump.to_str().unwrap(), "--json"],
    );
    assert_eq!(code(&out), 0, "{out:?}");
    let v = json(&out);
    assert_eq!(v["created"], 1);
    assert_eq!(v["skipped_done"], 1);
    // extension-less CSV → csv path
    std::fs::write(&dump, "number,title,state\n3,three,open\n,,\n").unwrap();
    let out = run(
        d.path(),
        &["absorb", "github", dump.to_str().unwrap(), "--json"],
    );
    assert_eq!(code(&out), 0, "{out:?}");
    assert_eq!(json(&out)["created"], 1);
}

#[test]
fn absorb_jira_tolerant_csv() {
    let (d, wt) = project();
    let csv = d.path().join("j.csv");
    // a real-world export: UTF-8 BOM, shouting/padded headers, a
    // quoted multi-line Summary, optional columns present for one
    // row and absent values elsewhere, one dead row mid-file
    std::fs::write(
        &csv,
        "\u{feff} Issue KEY , SUMMARY , Epic Name , Fix Version/s , Status , Assignee\n\
         PROJ-1,\"multi\nline summary\",launch,2.0,Selected for Development,\n\
         PROJ-2,second,,,Done,Luis Figueiredo\n\
         ,,,,,\n\
         PROJ-3,third,,,In Review,\n",
    )
    .unwrap();
    let out = run(
        d.path(),
        &[
            "absorb",
            "jira",
            csv.to_str().unwrap(),
            "--include-done",
            "--json",
        ],
    );
    assert_eq!(code(&out), 0, "{out:?}");
    let v = json(&out);
    assert_eq!(v["created"], 3, "{v}");
    assert_eq!(v["archived"], 1, "{v}"); // the Done row files cold
                                         // the empty row is a warning, not an abort
    let w = v["warnings"].as_array().unwrap();
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].as_str().unwrap().contains("row 3"), "{w:?}");
    assert!(stderr(&out).contains("absorb jira"), "{}", stderr(&out));
    // epic/milestone/status/assignee survived the trip
    let i1 = std::fs::read_to_string(wt.join("issues/issue-0001-multi-line-summary.md")).unwrap();
    assert!(i1.contains("epic = \"launch\""), "{i1}");
    assert!(i1.contains("milestone = \"2.0\""), "{i1}");
    let i3 = std::fs::read_to_string(wt.join("issues/issue-0003-third.md")).unwrap();
    assert!(i3.contains("status = \"in-progress\""), "{i3}");
    let i2 = std::fs::read_to_string(wt.join("issues/archive/issue-0002-second.md")).unwrap();
    assert!(i2.contains("owner = \"luis-figueiredo\""), "{i2}");
}

#[test]
fn absorb_github_wrapped_and_row_warnings() {
    let (d, _wt) = project();
    let f = d.path().join("issues.json");
    // the API list-issues wrapper, a non-object element (skipped,
    // not fatal), a state_reason-only row, a body-less row
    std::fs::write(
        &f,
        r#"{"issues": [
            {"number": 10, "title": "wrapped one", "state_reason": "not_planned"},
            42,
            {"number": 11, "title": "open two", "labels": [{"name": "bug"}]}
        ]}"#,
    )
    .unwrap();
    let out = run(
        d.path(),
        &[
            "absorb",
            "github",
            f.to_str().unwrap(),
            "--include-done",
            "--json",
        ],
    );
    assert_eq!(code(&out), 0, "{out:?}");
    let v = json(&out);
    assert_eq!(v["created"], 2, "{v}");
    assert_eq!(v["archived"], 1, "{v}"); // not_planned → done-ish
    let w = v["warnings"].as_array().unwrap();
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].as_str().unwrap().contains("issue #1"), "{w:?}");
    // human mode re-run: rows dedupe, the warning still prints
    let out = run(d.path(), &["absorb", "github", f.to_str().unwrap()]);
    assert_eq!(code(&out), 0, "{out:?}");
    assert!(
        stderr(&out).contains("isn't a JSON object"),
        "{}",
        stderr(&out)
    );
    // a malformed file errors cleanly naming it
    std::fs::write(&f, "{not json").unwrap();
    let out = run(d.path(), &["absorb", "github", f.to_str().unwrap()]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("issues.json"), "{}", stderr(&out));
}

// ---------- check: dangling-ref ----------

#[test]
fn check_flags_dangling_prose_refs() {
    let (d, wt) = project();
    assert_eq!(code(&run(d.path(), &["adr", "new", "first"])), 0);
    assert_eq!(code(&run(d.path(), &["todo", "add", "a task"])), 0);
    // a prose mention of a missing id in the ADR body — mentions of
    // live ids, self and code-fenced refs stay clean
    let dir = wt.join("adrs");
    let name = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_str().unwrap_or("").ends_with(".md"))
        .unwrap()
        .file_name()
        .to_str()
        .unwrap()
        .to_string();
    let p = dir.join(&name);
    let mut src = std::fs::read_to_string(&p).unwrap();
    src.push_str("\nSee ADR-0099 and T-001 here.\n\n```\nI-999 inside a fence\n```\n");
    std::fs::write(&p, src).unwrap();
    // index.md is verbatim prose — a stale ref flags there too
    write(&wt, "index.md", "# x\n\nmentions PL-042 here\n");
    let out = run(d.path(), &["check", "--json"]);
    assert_eq!(code(&out), 1, "{out:?}");
    let v = json(&out);
    let refs: Vec<&Value> = v["errors"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["code"] == "dangling-ref")
        .collect();
    assert_eq!(refs.len(), 2, "{refs:?}");
    let adr_ref = refs
        .iter()
        .find(|e| e["file"] == format!("adrs/{name}"))
        .expect("adr finding");
    assert!(adr_ref["line"].as_u64().unwrap() > 0, "{refs:?}");
    assert!(adr_ref["col"].as_u64().unwrap() > 0, "{refs:?}");
    assert_eq!(adr_ref["fixable"], false);
    assert!(
        adr_ref["msg"].as_str().unwrap().contains("ADR-0099"),
        "{refs:?}"
    );
    let idx_ref = refs
        .iter()
        .find(|e| e["file"] == "index.md")
        .expect("index finding");
    assert!(
        idx_ref["msg"].as_str().unwrap().contains("PL-042"),
        "{refs:?}"
    );
    // --fix repairs everything it can but the stale refs remain —
    // informational, intent unknown
    let out = run(d.path(), &["check", "--fix", "--json"]);
    assert_eq!(code(&out), 1, "{out:?}");
    let v = json(&out);
    assert!(v["errors"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["code"] == "dangling-ref"));
}

// ---------- migrate ----------

#[test]
fn migrate_pm_full_flow() {
    let d = repo();
    std::fs::create_dir_all(d.path().join("pm")).unwrap();
    std::fs::write(
        d.path().join("pm/TODO.md"),
        "# TODO — legacy\n\n- one\n- [x] done already\n",
    )
    .unwrap();
    std::fs::write(d.path().join("pm/BACKLOG.md"), "- idea\n").unwrap();
    std::fs::write(d.path().join("pm/ROADMAP.md"), "# Roadmap\n\nphases\n").unwrap();
    std::fs::write(d.path().join("pm/NOTES.md"), "# Notes\n\nmisc\n").unwrap();
    std::fs::write(d.path().join("pm/keep.txt"), "stays\n").unwrap();
    git(d.path(), &["add", "-A"]);
    git(d.path(), &["commit", "-qm", "pm legacy"]);
    // human output prints moved files
    let out = run(d.path(), &["migrate"]);
    assert_eq!(code(&out), 0, "{out:?}");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("pm/TODO.md"), "{s}");
    // keep.txt survives → pm/ stays
    assert!(d.path().join("pm/keep.txt").exists());
    // committed conversion items
    let wt = d.path().join("project");
    assert!(wt.join("meta.toml").exists());
    assert!(wt.join("plans/pl-0001-roadmap.md").exists());
    assert!(d
        .path()
        .join("project/issues")
        .read_dir()
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().contains("notes")));
}

#[test]
fn migrate_pm_commit_edges() {
    let d = repo();
    std::fs::create_dir_all(d.path().join("pm")).unwrap();
    std::fs::write(d.path().join("pm/TODO.md"), "- one\n").unwrap();
    let out = run(d.path(), &["migrate", "--no-commit", "--json"]);
    assert_eq!(code(&out), 0, "{out:?}");
    assert_eq!(json(&out)["committed"], false);
    git(d.path(), &["worktree", "remove", "--force", "project"]);

    // pm/ fully untracked → nothing to commit on the main side
    let d = repo();
    std::fs::create_dir_all(d.path().join("pm")).unwrap();
    std::fs::write(d.path().join("pm/TODO.md"), "- two\n").unwrap();
    let out = run(d.path(), &["migrate", "--json"]);
    assert_eq!(code(&out), 0, "{out:?}");

    // pm/ tracked but with no convertible files → add -u stages
    // nothing → the clean-tree arm of commit_pm_removal
    let d = repo();
    std::fs::create_dir_all(d.path().join("pm")).unwrap();
    std::fs::write(d.path().join("pm/keep.txt"), "stays\n").unwrap();
    git(d.path(), &["add", "-A"]);
    git(d.path(), &["commit", "-qm", "pm tracked"]);
    let out = run(d.path(), &["migrate", "--json"]);
    assert_eq!(code(&out), 0, "{out:?}");
    assert_eq!(json(&out)["converted_items"], 0);
    assert!(d.path().join("pm/keep.txt").exists());
}

#[test]
fn migrate_project_normalizes() {
    let (d, wt) = project();
    // a worktree with legacy-shaped + misplaced + broken files
    std::fs::remove_file(wt.join("meta.toml")).unwrap();
    std::fs::remove_file(wt.join("TODO.md")).unwrap();
    std::fs::remove_file(wt.join("BACKLOG.md")).unwrap();
    std::fs::remove_dir_all(wt.join("plans")).unwrap();
    write(&wt, "index.md", "# x\n[i](issues/issue-0001-baz.md)\n");
    // adoptees: number honoring + collisions + a number-less name
    write(&wt, "issues/issue-0001-bar.md", "# bar\n");
    write(&wt, "issues/issue-0001-baz.md", "# baz\n");
    write(&wt, "issues/issue-0001-baz2.md", "# baz two\n");
    // `#` alone isn't a title → filename stem is the fallback
    write(&wt, "issues/numberless.md", "#\nnum\n");
    write(&wt, "issues/.hidden.md", "ignored dotfile\n");
    write(&wt, "issues/notes.txt", "ignored non-md\n");
    // real item with frontmatter (owned id)
    write(
        &wt,
        "issues/issue-0009-real.md",
        "+++\nid = \"I-009\"\ntitle = \"real\"\nstatus = \"open\"\n+++\n\nbody\n",
    );
    // a drifted real file → canonical rename
    write(
        &wt,
        "adrs/zzz.md",
        "+++\nid = \"ADR-0003\"\ntitle = \"three\"\nstatus = \"open\"\n+++\n\nbody\n",
    );
    // a different-kind file → moved + re-emitted
    write(
        &wt,
        "proposals/moved.md",
        "+++\nid = \"I-005\"\ntitle = \"five\"\nstatus = \"open\"\n+++\n\nbody\n",
    );
    // frontmatter id of a non-file kind → unparsed
    write(
        &wt,
        "adrs/odd.md",
        "+++\nid = \"T-001\"\ntitle = \"t\"\n+++\n",
    );
    // same-kind but unparseable → unparsed
    write(&wt, "plans/bad.md", "+++\nid = \"PL-002\"\n+++\n");
    // non-utf8 file content → skipped
    std::fs::write(wt.join("issues/noutf8.md"), [0xff, 0x00]).unwrap();
    // non-utf8 filename → skipped in the claims walk
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::fs::write(
            wt.join("issues")
                .join(std::ffi::OsStr::from_bytes(b"\xff.md")),
            "x",
        )
        .unwrap();
    }
    // archived file keeps its number reserved
    write(&wt, "adrs/archive/0007-old.md", "# old\n");
    // human output covers moved/unparsed/warnings lines
    let out = run(d.path(), &["migrate"]);
    assert_eq!(code(&out), 0, "{out:?}");
    let err = stderr(&out);
    assert!(err.contains("unparsed:"), "{err}");
    assert!(err.contains("warning:"), "{err}");
    // adoptees: 0001-bar honored, 0001-baz collided to the next free
    let issues: Vec<String> = std::fs::read_dir(wt.join("issues"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        issues.iter().any(|n| n.contains("issue-0001")),
        "{issues:?}"
    );
    assert!(
        issues.iter().any(|n| n.contains("issue-0002")),
        "{issues:?}"
    );
    // moved to issues/
    assert!(
        issues.iter().any(|n| n.contains("issue-0005")),
        "{issues:?}"
    );
    // the drifted ADR is canonically named
    assert!(wt.join("adrs/0003-three.md").exists());
    assert!(!wt.join("adrs/zzz.md").exists());
    // index.md repointed at the rename (baz collided → I-002)
    let index = std::fs::read_to_string(wt.join("index.md")).unwrap();
    assert!(index.contains("issue-0002-baz"), "{index}");
}

#[test]
fn migrate_project_missing_item_dir() {
    let (d, wt) = project();
    // `plans` simply isn't there: the claim pass tolerates the
    // missing read_dir (`let Ok else`), the adopt walk skips it on
    // `!is_dir`, and with no renames the index repoint is skipped
    std::fs::remove_dir_all(wt.join("plans")).unwrap();
    let out = run(d.path(), &["migrate", "--json"]);
    assert_eq!(code(&out), 0, "{out:?}");
}

#[test]
fn migrate_project_no_index_file() {
    let (d, wt) = project();
    // a rename happens but index.md is absent → the
    // `if let Some(index)` else edge
    std::fs::remove_file(wt.join("index.md")).unwrap();
    write(&wt, "issues/foo.md", "# foo\n");
    let out = run(d.path(), &["migrate", "--json"]);
    assert_eq!(code(&out), 0, "{out:?}");
    assert!(wt.join("issues/issue-0001-foo.md").exists());
}

#[test]
fn migrate_project_index_save_error() {
    let (d, wt) = project();
    // adoptable file + index link → a rename → save_index; make
    // the worktree root read-only so the write fails
    std::fs::remove_file(wt.join("TODO.md")).unwrap();
    std::fs::remove_file(wt.join("BACKLOG.md")).unwrap();
    // no index.md at all → the `if let Some(index)` else edge;
    // the root-write failure hits later, at meta.toml
    std::fs::remove_file(wt.join("index.md")).unwrap();
    write(&wt, "issues/foo.md", "# foo\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wt, std::fs::Permissions::from_mode(0o555)).unwrap();
    }
    let out = run(d.path(), &["migrate"]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wt, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert_eq!(code(&out), 2, "{out:?}");
}

// ---------- index + org ----------

#[test]
fn index_lifecycle_and_rebuild() {
    let (d, _wt) = project();
    let home = tempfile::tempdir().unwrap();
    let env = [("ARCA_HOME", home.path().to_str().unwrap())];
    let root = d.path().to_str().unwrap();
    // empty registry list (human + json)
    let out = run_env(d.path(), &["index", "list"], &env);
    assert_eq!(code(&out), 0);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("no repos"),
        "{out:?}"
    );
    // add (human + json) and list fresh
    let out = run_env(d.path(), &["index", "add", root], &env);
    assert_eq!(code(&out), 0, "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("registered"));
    let out = run_env(d.path(), &["index", "list"], &env);
    assert!(String::from_utf8_lossy(&out.stdout).contains("fresh"));
    // rebuild all (human) and a specific path
    let out = run_env(d.path(), &["index", "rebuild"], &env);
    assert_eq!(code(&out), 0, "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("rebuilt"));
    let out = run_env(d.path(), &["index", "rebuild", root, "--json"], &env);
    assert_eq!(code(&out), 0, "{out:?}");
    // stale: a new worktree commit makes the snapshot old
    assert_eq!(code(&run(d.path(), &["todo", "add", "stale me"])), 0);
    let out = run_env(d.path(), &["index", "list", "--json"], &env);
    assert_eq!(json(&out)[0]["stale"], true);
    // remove (json + human)
    let out = run_env(d.path(), &["index", "remove", root, "--json"], &env);
    assert_eq!(code(&out), 0, "{out:?}");
    assert_eq!(
        json(&out)["removed"],
        d.path().canonicalize().unwrap().to_str().unwrap()
    );
    let out = run_env(d.path(), &["index", "add", root], &env);
    assert_eq!(code(&out), 0);
    let out = run_env(d.path(), &["index", "remove", root], &env);
    assert!(String::from_utf8_lossy(&out.stdout).contains("removed"));
    // removing a non-registered path errors
    let out = run_env(d.path(), &["index", "remove", root], &env);
    assert_eq!(code(&out), 2, "{out:?}");
}

#[test]
fn index_rebuild_empty_registry() {
    let home = tempfile::tempdir().unwrap();
    let env = [("ARCA_HOME", home.path().to_str().unwrap())];
    let d = repo();
    let out = run_env(d.path(), &["index", "rebuild"], &env);
    assert_eq!(code(&out), 0, "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("no repos"),
        "{out:?}"
    );
    let out = run_env(d.path(), &["index", "rebuild", "--json"], &env);
    assert_eq!(code(&out), 0, "{out:?}");
}

#[test]
fn index_rebuild_failures_and_gone_worktree() {
    let (d, _wt) = project();
    let home = tempfile::tempdir().unwrap();
    let env = [("ARCA_HOME", home.path().to_str().unwrap())];
    let root = d.path().to_str().unwrap();
    assert_eq!(code(&run_env(d.path(), &["index", "add", root], &env)), 0);
    // the worktree vanishes → list marks stale, rebuild errors
    git(d.path(), &["worktree", "remove", "--force", "project"]);
    let out = run_env(d.path(), &["index", "list", "--json"], &env);
    assert_eq!(json(&out)[0]["stale"], true);
    let out = run_env(d.path(), &["index", "rebuild"], &env);
    assert_eq!(code(&out), 2, "{out:?}");
    assert!(stderr(&out).contains("rebuild(s) failed"), "{out:?}");
    // org status: the repo errored — error kept with its snapshot
    let out = run_env(d.path(), &["org", "status", "--json"], &env);
    assert_eq!(code(&out), 0, "{out:?}");
    let out = run_env(d.path(), &["org", "status"], &env);
    assert_eq!(code(&out), 0, "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains('⚠'),
        "{out:?}"
    );
    // a registered repo never scanned → (None, _) stale arm
    std::fs::write(
        home.path().join("repos.toml"),
        format!("[[repo]]\npath = \"{}\"\nname = \"mine\"\n\n[[repo]]\npath = \"/nonexistent/ghost\"\nname = \"ghost\"\n", d.path().canonicalize().unwrap().display()),
    )
    .unwrap();
    let out = run_env(d.path(), &["index", "list", "--json"], &env);
    assert_eq!(code(&out), 0, "{out:?}");
    assert!(
        json(&out)
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["stale"] == true),
        "{out:?}"
    );
    // a registered repo whose ROOT vanishes → snapshot exists but
    // head can't be read → stale via the (Some, None) arm
    let d2 = repo();
    assert_eq!(code(&run(d2.path(), &["init", "--json"])), 0);
    let root2 = d2.path().to_str().unwrap().to_string();
    assert_eq!(
        code(&run_env(d2.path(), &["index", "add", &root2], &env)),
        0
    );
    std::fs::remove_dir_all(d2.path()).unwrap();
    let out = run_env(d.path(), &["index", "list", "--json"], &env);
    assert!(
        json(&out)
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["stale"] == true),
        "{out:?}"
    );
    // still removable
    let out = run_env(d.path(), &["index", "remove", root], &env);
    assert_eq!(code(&out), 0, "{out:?}");
}

#[test]
fn org_status_and_board() {
    let (d, _wt) = project();
    let home = tempfile::tempdir().unwrap();
    let env = [("ARCA_HOME", home.path().to_str().unwrap())];
    let root = d.path().to_str().unwrap();
    assert_eq!(code(&run(d.path(), &["todo", "add", "task one"])), 0);
    assert_eq!(
        code(&run(
            d.path(),
            &["item", "set", "T-001", "--owner", "devin"]
        )),
        0
    );
    assert_eq!(code(&run_env(d.path(), &["index", "add", root], &env)), 0);
    // a hand-registered repo with no snapshot → error/None arms in
    // org status (json + human "unavailable" rows)
    std::fs::write(
        home.path().join("repos.toml"),
        format!("[[repo]]\npath = \"{}\"\nname = \"mine\"\n\n[[repo]]\npath = \"/nonexistent/ghost\"\nname = \"ghost\"\n", d.path().canonicalize().unwrap().display()),
    )
    .unwrap();
    // a new worktree commit makes the snapshot stale → org status
    // rebuilds → the human run FIRST gets the "(rebuilt)" arm
    assert_eq!(code(&run(d.path(), &["todo", "add", "task two"])), 0);
    let out = run_env(d.path(), &["org", "status"], &env);
    assert_eq!(code(&out), 0, "{out:?}");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("org status — 2 repo(s)"), "{s}");
    assert!(s.contains("totals:"), "{s}");
    assert!(s.contains("devin"), "{s}");
    assert!(s.contains('⚠'), "{s}");
    // json
    let out = run_env(d.path(), &["org", "status", "--json"], &env);
    assert_eq!(code(&out), 0);
    assert!(String::from_utf8_lossy(&out.stdout).contains("rebuilt"));
    // a bare `- [ ] T-009` line has an empty title → card is the
    // bare id; commit it so the snapshot stales + rebuilds
    let todo = _wt.join("TODO.md");
    let mut t = std::fs::read_to_string(&todo).unwrap();
    t.push_str("- [ ] T-009\n");
    std::fs::write(&todo, t).unwrap();
    git(&_wt, &["add", "-A"]);
    git(&_wt, &["commit", "-qm", "chore: add T-009"]);
    // org board human (with cards) + md + json
    let out = run_env(d.path(), &["org", "board"], &env);
    assert_eq!(code(&out), 0, "{out:?}");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("T-001"), "{s}");
    let out = run_env(d.path(), &["org", "board", "--md"], &env);
    assert_eq!(code(&out), 0);
    let out = run_env(d.path(), &["org", "board", "--json"], &env);
    assert_eq!(code(&out), 0);
    // an org board with nothing live
    let (d2, _wt2) = project();
    let home2 = tempfile::tempdir().unwrap();
    let env2 = [("ARCA_HOME", home2.path().to_str().unwrap())];
    assert_eq!(
        code(&run_env(
            d2.path(),
            &["index", "add", d2.path().to_str().unwrap()],
            &env2
        )),
        0
    );
    let out = run_env(d2.path(), &["org", "board"], &env2);
    assert_eq!(code(&out), 0, "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("nothing live"),
        "{out:?}"
    );
}

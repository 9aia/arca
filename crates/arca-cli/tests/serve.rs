//! End-to-end `arca serve` — spawn the real binary, pipe JSON-RPC
//! lines through it, and check both the wire behavior (id echo,
//! error codes, resilience, EOF exit) and the verb semantics
//! (writes commit on the project branch, in order).

#![allow(clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

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

fn init(d: &Path) {
    let out = Command::new(ARCA)
        .current_dir(d)
        .args(["init", "--json"])
        .env("ARCA_CONFIG", "/nonexistent/arca.toml")
        .output()
        .unwrap();
    assert!(out.status.success(), "init failed: {out:?}");
}

/// A spawned `arca serve` — write requests, read responses.
struct Serve {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Serve {
    fn spawn(dir: &Path) -> Serve {
        let mut child = Command::new(ARCA)
            .current_dir(dir)
            .arg("serve")
            .env("ARCA_CONFIG", "/nonexistent/arca.toml")
            .env_remove("ARCA_NO_COMMIT")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Serve {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
        }
    }

    /// One request line out, one parsed response back.
    fn rpc(&mut self, req: &Value) -> Value {
        self.line(&serde_json::to_string(req).unwrap())
    }

    /// One raw line out, one parsed response back.
    fn line(&mut self, line: &str) -> Value {
        writeln!(self.stdin, "{line}").unwrap();
        self.stdin.flush().unwrap();
        let mut buf = String::new();
        let n = self.stdout.read_line(&mut buf).unwrap();
        assert!(n > 0, "serve closed stdout on {line:?}");
        serde_json::from_str(&buf).unwrap_or_else(|e| panic!("bad response json ({e}): {buf}"))
    }

    /// Close stdin — the serve loop should exit 0 on EOF.
    fn finish(mut self) -> i32 {
        drop(self.stdin);
        self.child.wait().unwrap().code().unwrap_or(-1)
    }
}

#[test]
fn serve_roundtrip_writes_and_errors() {
    let d = repo();
    init(d.path());
    let wt = d.path().join("project");
    let mut s = Serve::spawn(d.path());

    // initialize + manifest — id echo, result shapes
    let v = s.rpc(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}));
    assert_eq!(v["id"], 1);
    assert_eq!(v["result"]["protocol"], 1);
    assert_eq!(v["result"]["wire"], "jsonrpc-2.0");
    let v = s.rpc(&json!({"jsonrpc": "2.0", "id": "m", "method": "manifest"}));
    assert_eq!(v["id"], "m");
    assert_eq!(v["result"]["name"], "arca");
    assert_eq!(v["result"]["capabilities"]["serve"]["wire"], "jsonrpc-2.0");

    // status on the fresh ledger — structured digest
    let v = s.rpc(&json!({"id": 3, "method": "status"}));
    assert_eq!(v["id"], 3);
    assert!(v["result"]["repo"].is_string());
    assert_eq!(v["result"]["counts"]["tasks"]["open"], 0);
    let v = s.rpc(&json!({"id": 4, "method": "status", "params": {"prompt": true}}));
    assert!(v["result"]["prompt"].as_str().unwrap().contains("repo:"));

    // malformed line → -32700, and the server keeps serving
    let v = s.line("this is not json");
    assert_eq!(v["id"], Value::Null);
    assert_eq!(v["error"]["code"], -32700);
    let v = s.rpc(&json!({"id": 5, "method": "nope"}));
    assert_eq!(v["error"]["code"], -32601);
    let v = s.rpc(&json!({"id": 6, "method": "todo.add", "params": {}}));
    assert_eq!(v["error"]["code"], -32602);
    let v = s.rpc(&json!({"id": 7, "method": "status"}));
    assert!(v["result"]["repo"].is_string(), "still serving: {v}");

    // two writes → two `chore:` commits on the project branch,
    // in order
    let v =
        s.rpc(&json!({"id": 8, "method": "todo.add", "params": {"title": "first", "pri": "high"}}));
    assert_eq!(v["result"]["item"]["id"], "T-001");
    assert_eq!(v["result"]["committed"], true);
    let v = s.rpc(&json!({"id": 9, "method": "todo.add", "params": {"title": "second"}}));
    assert_eq!(v["result"]["item"]["id"], "T-002");
    assert_eq!(v["result"]["committed"], true);
    let log = git(&wt, &["log", "--format=%s", "-3"]);
    let mut subjects = log.lines();
    assert_eq!(subjects.next(), Some("chore: add T-002"));
    assert_eq!(subjects.next(), Some("chore: add T-001"));

    // item.set with an illegal status step → -32001 + valid targets
    let v = s
        .rpc(&json!({"id": 10, "method": "item.set", "params": {"id": "T-001", "status": "done"}}));
    assert_eq!(v["id"], 10);
    assert_eq!(v["error"]["code"], -32001);
    assert_eq!(v["error"]["data"]["from"], "open");
    assert_eq!(v["error"]["data"]["to"], "done");
    assert!(v["error"]["data"]["valid"]
        .as_array()
        .unwrap()
        .contains(&json!("in-progress")));
    // a legal step works
    let v = s.rpc(&json!({"id": 11, "method": "item.set", "params": {"id": "T-001", "status": "in-progress"}}));
    assert_eq!(v["result"]["item"]["status"], "in-progress");

    // reads see the writes — the store re-opens per request
    let v = s.rpc(&json!({"id": 12, "method": "todo.list"}));
    assert_eq!(v["result"].as_array().unwrap().len(), 2);
    let v = s.rpc(&json!({"id": 13, "method": "item.show", "params": {"id": "T-002"}}));
    assert_eq!(v["result"]["item"]["title"], "second");
    let v = s.rpc(&json!({"id": 14, "method": "handoff", "params": {"id": "T-001", "note": "wip", "actor": "devin"}}));
    assert_eq!(v["result"]["committed"], true);
    let v = s.rpc(&json!({"id": 15, "method": "resume", "params": {"id": "T-001"}}));
    assert_eq!(v["result"]["handoffs_total"], 1);

    // note "-" can't read stdin — stdin IS the wire
    let v = s.rpc(&json!({"id": 16, "method": "handoff", "params": {"id": "T-001", "note": "-"}}));
    assert_eq!(v["error"]["code"], -32602);

    // EOF → exit 0
    assert_eq!(s.finish(), 0);
    // worktree is clean — every write committed
    assert_eq!(git(&wt, &["status", "--porcelain"]), "");
}

#[test]
fn serve_no_commit_flag_and_env() {
    let d = repo();
    init(d.path());
    // --no-commit: writes land but stay uncommitted
    let mut child = Command::new(ARCA)
        .current_dir(d.path())
        .args(["serve", "--no-commit"])
        .env("ARCA_CONFIG", "/nonexistent/arca.toml")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    writeln!(
        stdin,
        "{}",
        json!({"id": 1, "method": "todo.add", "params": {"title": "x"}})
    )
    .unwrap();
    stdin.flush().unwrap();
    let mut buf = String::new();
    stdout.read_line(&mut buf).unwrap();
    let v: Value = serde_json::from_str(&buf).unwrap();
    assert_eq!(v["result"]["item"]["id"], "T-001");
    assert_eq!(v["result"]["committed"], false);
    drop(stdin);
    assert_eq!(child.wait().unwrap().code().unwrap_or(-1), 0);
    let wt = d.path().join("project");
    assert!(!git(&wt, &["status", "--porcelain"]).is_empty());
    // clean up the worktree so the tempdir drops cleanly
    git(d.path(), &["worktree", "remove", "--force", "project"]);
}

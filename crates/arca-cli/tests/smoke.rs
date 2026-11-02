//! End-to-end against a temp HOME: `init` → `put` (stdin) →
//! `get`/`list`/`run`/`env` → `rm`/`check`. Values are fake by
//! rule — nothing here is a real secret.

#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const FAKE: &str = "fake-value-not-a-real-secret";

fn arca(home: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_arca"));
    c.args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("xdg-config"))
        .env("XDG_DATA_HOME", home.join("xdg-data"))
        .env_remove("ARCA_CONFIG")
        .env_remove("ARCA_ENV")
        .env_remove("ARCA_ACTOR")
        .env("ARCA_IDENTITY_FILE", home.join("xdg-config/arca/identity.txt"));
    c
}

fn run(home: &Path, args: &[&str]) -> Output {
    arca(home, args).output().unwrap()
}

fn run_stdin(home: &Path, args: &[&str], input: &str) -> Output {
    let mut child = arca(home, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn happy_path() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();

    // init — generates the 0600 identity + opens the vault
    let o = run(home, &["init"]);
    assert!(o.status.success(), "init: {}", stderr(&o));
    let identity = home.join("xdg-config/arca/identity.txt");
    assert!(identity.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&identity).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    // put — value on stdin, audit event on stderr
    let o = run_stdin(
        home,
        &["put", "arca://billing/stripe"],
        &format!("{FAKE}\n"),
    );
    assert!(o.status.success(), "put: {}", stderr(&o));
    assert!(stderr(&o).contains("\"op\":\"put\""), "{}", stderr(&o));

    // ciphertext at rest — no plaintext in the vault file
    let blob = std::fs::read(home.join("xdg-data/arca/vault.age")).unwrap();
    assert!(!String::from_utf8_lossy(&blob).contains(FAKE));

    // get — value on stdout, materialization event on stderr
    let o = run(home, &["get", "arca://billing/stripe"]);
    assert!(o.status.success(), "get: {}", stderr(&o));
    assert_eq!(stdout(&o).trim_end(), FAKE);
    assert!(stderr(&o).contains("\"op\":\"get\""));

    // list
    let o = run(home, &["list"]);
    assert!(o.status.success());
    assert_eq!(stdout(&o).trim(), "arca://billing/stripe");

    // run — ad-hoc -s injection reaches the child env only
    let o = run(
        home,
        &[
            "run",
            "-s",
            "FAKE_IT=arca://billing/stripe",
            "--",
            "printenv",
            "FAKE_IT",
        ],
    );
    assert!(o.status.success(), "run: {}", stderr(&o));
    assert_eq!(stdout(&o).trim_end(), FAKE);

    // env — export lines for eval
    let o = run(home, &["env", "-s", "FAKE_IT=arca://billing/stripe"]);
    assert!(o.status.success(), "env: {}", stderr(&o));
    assert_eq!(stdout(&o).trim(), format!("export FAKE_IT='{FAKE}'"));

    // env pin — arca://prod/… resolves under prod regardless
    let o = run_stdin(home, &["put", "arca://prod/token"], "fake-prod\n");
    assert!(o.status.success(), "pin put: {}", stderr(&o));
    let o = run(home, &["list", "--env", "prod"]);
    assert_eq!(stdout(&o).trim(), "arca://token");
    let o = run(home, &["get", "arca://prod/token"]);
    assert_eq!(stdout(&o).trim_end(), "fake-prod");

    // rotate — bumps version
    let o = run_stdin(home, &["rotate", "arca://billing/stripe"], "fake-v2\n");
    assert!(o.status.success(), "rotate: {}", stderr(&o));
    let o = run(home, &["get", "--json", "arca://billing/stripe"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!(v["version"], 2);
    assert_eq!(v["value"], "fake-v2");

    // check + manifest + schema
    for verb in ["check", "manifest", "schema"] {
        let o = run(home, &[verb]);
        assert!(o.status.success(), "{verb}: {}", stderr(&o));
    }

    // rm — then get fails
    let o = run(home, &["rm", "arca://billing/stripe"]);
    assert!(o.status.success(), "rm: {}", stderr(&o));
    let o = run(home, &["get", "arca://billing/stripe"]);
    assert!(!o.status.success());
}

#[test]
fn put_rejects_value_args_and_tty() {
    let home = tempfile::tempdir().unwrap();
    run(home.path(), &["init"]);
    // empty stdin → error, nothing stored
    let o = run_stdin(home.path(), &["put", "arca://x/y"], "");
    assert!(!o.status.success());
    let o = run(home.path(), &["get", "arca://x/y"]);
    assert!(!o.status.success());
}

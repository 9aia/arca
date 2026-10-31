//! Edge cases — non-json output arms, error exits, tty-only code
//! paths (via `script`'s pty), and failure modes a unit test can't
//! reach. Temp-HOME isolated; values are fake by rule.

#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::path::{Path, PathBuf};
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
        .env_remove("ARCA_IDENTITY_FILE");
    c
}

fn run(home: &Path, args: &[&str]) -> Output {
    arca(home, args).output().unwrap()
}

fn run_stdin(home: &Path, args: &[&str], input: &[u8]) -> Output {
    let mut child = arca(home, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

/// Run under a pty via `script` — stdin goes in as terminal input.
fn run_pty(home: &Path, args: &[&str], input: &[u8]) -> Output {
    let arca = env!("CARGO_BIN_EXE_arca");
    let cmdline = format!(
        "env -i HOME='{}' XDG_CONFIG_HOME='{}' XDG_DATA_HOME='{}' TERM=xterm '{}' {}",
        home.display(),
        home.join("xdg-config").display(),
        home.join("xdg-data").display(),
        arca,
        args.join(" ")
    );
    let mut child = Command::new("script")
        .args(["-qec", &cmdline, "/dev/null"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn config(home: &Path, name: &str, body: &str) -> PathBuf {
    let p = home.join(name);
    std::fs::write(&p, body).unwrap();
    p
}

fn run_cfg(home: &Path, cfg: &Path, args: &[&str]) -> Output {
    let mut v = vec!["--config".to_string(), cfg.to_string_lossy().into_owned()];
    v.extend(args.iter().map(|s| s.to_string()));
    let refs: Vec<&str> = v.iter().map(String::as_str).collect();
    run(home, &refs)
}

fn init(home: &Path) {
    assert!(run(home, &["init"]).status.success());
}

#[test]
fn introspection_verbs_run_before_config_load() {
    let home = tempfile::tempdir().unwrap();
    // none of these need a config — they work in a bare HOME
    let o = run(home.path(), &["manifest"]);
    assert!(o.status.success());
    assert!(stdout(&o).contains("\"arca\""));
    let o = run(home.path(), &["schema"]);
    assert!(o.status.success());
    assert!(stdout(&o).contains("vaults"), "{}", stdout(&o));
    let o = run(home.path(), &["gen-cli-docs"]);
    assert!(o.status.success());
    assert!(stdout(&o).contains("arca"), "{}", stdout(&o));
}

#[test]
fn load_failures_audit_and_fail_clean() {
    let home = tempfile::tempdir().unwrap();
    // explicit --config missing → error + error audit event
    let o = run(home.path(), &["--config", "/nonexistent/arca.toml", "list"]);
    assert!(!o.status.success());
    let e = stderr(&o);
    assert!(e.contains("\"outcome\":\"error\""), "{e}");
    assert!(e.contains("\"op\":\"list\""), "{e}");
    // same but --json → {"error":…} on stderr
    let o = run(
        home.path(),
        &["--config", "/nonexistent/arca.toml", "--json", "list"],
    );
    assert!(!o.status.success());
    // a config that exists but won't parse → same path
    let bad = config(home.path(), "bad.toml", "[[[ not toml");
    let o = run(home.path(), &["--config", bad.to_str().unwrap(), "list"]);
    assert!(!o.status.success());
    // and with a ref arg — the fallback audit still records it raw
    let o = run(
        home.path(),
        &["--config", "/nonexistent.toml", "get", "not-a-ref"],
    );
    assert!(!o.status.success());
    assert!(stderr(&o).contains("not-a-ref"), "{}", stderr(&o));
}

#[test]
fn bad_ref_args_fail_before_touching_the_vault() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    init(home);
    for args in [
        vec!["get", "not-a-ref"],
        vec!["rm", "not-a-ref"],
        vec!["deploy", "not-a-ref"],
    ] {
        let o = run(home, &args);
        assert!(!o.status.success(), "{args:?}");
        let e = stderr(&o);
        // the error audit event carries the raw unparseable ref
        assert!(e.contains("\"outcome\":\"error\""), "{args:?}: {e}");
        assert!(e.contains("not-a-ref"), "{args:?}: {e}");
    }
    // put/rotate are denied-free too — bad ref fails at parse
    let o = run_stdin(home, &["put", "not-a-ref"], b"fake\n");
    assert!(!o.status.success());
    let o = run_stdin(home, &["rotate", "not-a-ref"], b"fake\n");
    assert!(!o.status.success());
    // a denied op on an unparseable ref → denied event with raw ref
    let cfg = config(home, "arca.toml", "[guardrails]\ndeny = [\"get\"]\n");
    let o = run_cfg(home, &cfg, &["get", "not-a-ref"]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("\"outcome\":\"denied\""));
    assert!(stderr(&o).contains("not-a-ref"));
}

#[test]
fn non_json_verbs_print_for_humans() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    init(home);
    // second init is not "generated" — the quiet else arm
    let o = run(home, &["init"]);
    assert!(o.status.success());
    assert!(!stderr(&o).contains("(generated)"), "{}", stderr(&o));

    run_stdin(home, &["put", "arca://billing/stripe"], FAKE.as_bytes());
    let o = run(home, &["list"]);
    assert_eq!(stdout(&o).trim(), "arca://billing/stripe");
    // rm non-json
    let o = run(home, &["rm", "arca://billing/stripe"]);
    assert!(o.status.success());
    assert!(stderr(&o).contains("removed"), "{}", stderr(&o));
    // rm-miss non-json is an error
    let o = run(home, &["rm", "arca://billing/stripe"]);
    assert!(!o.status.success());
    // put + rotate non-json print "(vN)"
    run_stdin(home, &["put", "arca://a/b"], b"fake-a\n");
    let o = run_stdin(home, &["rotate", "arca://a/b"], b"fake-a2\n");
    assert!(o.status.success());
    assert!(stderr(&o).contains("(v2)"), "{}", stderr(&o));
    // list --json shows the rotated_at stamp
    let o = run(home, &["list", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert!(v[0]["rotated_at"].is_string(), "{v}");
    // rotate of a missing ref → NotFound, clean error
    let o = run_stdin(home, &["rotate", "arca://no/here"], b"fake\n");
    assert!(!o.status.success());
}

#[test]
fn verbs_fail_clean_when_the_vault_cant_open() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    // dev routes to a vault of an unknown kind → open errors
    let cfg = config(
        home,
        "arca.toml",
        "[vaults.bogus]\nkind = \"zzz\"\n\n[envs]\ndev = \"bogus\"\n",
    );
    for args in [
        vec!["list"],
        vec!["list", "--json"],
        vec!["get", "arca://x/y"],
        vec!["rm", "arca://x/y"],
        vec!["init"],
    ] {
        let o = run_cfg(home, &cfg, &args);
        assert!(!o.status.success(), "{args:?}");
    }
    let o = run_cfg(home, &cfg, &["deploy", "arca://x/y", "--target", "t"]);
    assert!(!o.status.success());
}

#[test]
fn read_secret_rejects_non_utf8_and_empty_stdin() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    init(home);
    // invalid utf-8 on the pipe → a read error, not a panic
    let o = run_stdin(home, &["put", "arca://x/y"], b"\xff\xfe");
    assert!(!o.status.success());
    // empty stdin → "empty secret"
    let o = run_stdin(home, &["put", "arca://x/y"], b"");
    assert!(!o.status.success());
    let o = run_stdin(home, &["put", "arca://x/y"], b"\n");
    assert!(!o.status.success());
}

#[test]
fn run_and_env_validate_pairs_and_resolutions() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    init(home);
    // -s without '=' → the pair parse error
    let o = run(home, &["run", "-s", "BADPAIR", "--", "true"]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("VAR=arca://ref"), "{}", stderr(&o));
    // -s with an unparseable ref → resolve error
    let o = run(home, &["run", "-s", "V=not-a-ref", "--", "true"]);
    assert!(!o.status.success());
    // a declared-but-unstored ref → the whole run refuses
    let o = run(home, &["run", "-s", "V=arca://not/stored", "--", "true"]);
    assert!(!o.status.success());
    // same for `env`
    let o = run(home, &["env", "-s", "BADPAIR"]);
    assert!(!o.status.success());
    let o = run(home, &["env", "-s", "V=arca://not/stored"]);
    assert!(!o.status.success());
}

#[test]
fn run_without_bash_on_path_is_a_clean_error() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    init(home);
    // bash -c mode with no bash on PATH → spawn fails, arca errors
    let o = arca(home, &["run", "--", "true"])
        .env("PATH", "/nonexistent-bin")
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(
        stderr(&o).contains("\"outcome\":\"error\""),
        "{}",
        stderr(&o)
    );
}

#[test]
fn env_writes_fail_cleanly_on_a_full_device() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    init(home);
    run_stdin(home, &["put", "arca://x/y"], FAKE.as_bytes());
    if !Path::new("/dev/full").exists() {
        return; // no /dev/full on this platform — nothing to test
    }
    let full = std::fs::File::options()
        .write(true)
        .open("/dev/full")
        .unwrap();
    let o = arca(home, &["env", "-s", "V=arca://x/y"])
        .stdout(Stdio::from(full))
        .output()
        .unwrap();
    assert!(!o.status.success(), "writes to /dev/full must fail");
}

#[test]
fn a_deleted_cwd_is_a_clean_error_not_a_panic() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    init(home);
    // real ciphertext in a normal-path vault first
    let vault_file = home.join("real.age");
    let id_file = home.join("xdg-config/arca/identity.txt");
    let cfg_a = config(
        home,
        "a.toml",
        &format!(
            "[vaults.local]\npath = {:?}\nidentity = {:?}\n",
            vault_file, id_file
        ),
    );
    let o = run_stdin(
        home,
        &["--config", cfg_a.to_str().unwrap(), "put", "arca://x/y"],
        FAKE.as_bytes(),
    );
    assert!(o.status.success(), "{}", stderr(&o));
    let ciphertext = std::fs::read(&vault_file).unwrap();
    // now the same identity but the vault path is a fifo — the
    // child's vault read blocks until our writer feeds it
    let fifo = home.join("vault.fifo");
    assert!(Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    let cfg_b = config(
        home,
        "b.toml",
        &format!(
            "[vaults.local]\npath = {:?}\nidentity = {:?}\n",
            fifo, id_file
        ),
    );
    let writer = std::thread::spawn(move || {
        std::fs::write(&fifo, &ciphertext).unwrap();
    });
    let wd = home.join("workdir");
    std::fs::create_dir(&wd).unwrap();
    let child = arca(
        home,
        &[
            "--config",
            cfg_b.to_str().unwrap(),
            "run",
            "-s",
            "V=arca://x/y",
            "--",
            "true",
        ],
    )
    .current_dir(&wd)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    // the child is blocked reading the fifo — delete its cwd now,
    // so `current_dir` fails when the fifo finally feeds it
    std::fs::remove_dir_all(&wd).unwrap();
    let o = child.wait_with_output().unwrap();
    writer.join().unwrap();
    assert!(!o.status.success(), "deleted cwd must error, got ok");
}

#[test]
fn check_prints_findings_for_humans() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let cfg = config(
        home,
        "arca.toml",
        "[secrets]\nMISSING = \"arca://not/here\"\nBAD-NAME = \"arca://x/y\"\n\n\
         [envs]\nprod = \"nowhere\"\n\n\
         [guardrails]\ndefault = \"deny\"\nallow = [\"run\", \"check\"]\ndeny = [\"get\"]\n",
    );
    let o = run_cfg(home, &cfg, &["check"]);
    assert!(!o.status.success());
    let e = stderr(&o);
    assert!(e.contains("config:"), "{e}");
    assert!(e.contains("not readable"), "{e}");
    assert!(e.contains("NOT STORED"), "{e}");
    assert!(e.contains("guardrails:"), "{e}");
    assert!(e.contains("problem:"), "{e}");

    // --json carries the guardrails table — both default arms
    let o = run_cfg(home, &cfg, &["check", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!(v["guardrails"]["default"], "deny");
    let cfg2 = config(
        home,
        "b.toml",
        "[guardrails]\ndefault = \"allow\"\nallow = [\"run\"]\n",
    );
    let o = run_cfg(home, &cfg2, &["check", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!(v["guardrails"]["default"], "allow");
    assert_eq!(v["guardrails"]["allow"][0], "run");
}

#[test]
fn deploy_prints_created_then_updated() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    init(home);
    run_stdin(home, &["put", "arca://x/y"], FAKE.as_bytes());
    let (mock, _seen) = arca_testkit::mocks::cf_secrets_store("a", "s", "t");
    let cfg = config(
        home,
        "arca.toml",
        &format!(
            "[deploy.prod]\naccount_id = \"a\"\nstore_id = \"s\"\nbase_url = \"{}\"\n\n\
             [envs]\ndev = \"local\"\n",
            mock.url()
        ),
    );
    temp_env::with_var("CLOUDFLARE_API_TOKEN", Some("t"), || {
        let o = run_cfg(home, &cfg, &["deploy", "arca://x/y"]);
        assert!(o.status.success(), "{}", stderr(&o));
        assert!(stderr(&o).contains("created"), "{}", stderr(&o));
        let o = run_cfg(home, &cfg, &["deploy", "arca://x/y"]);
        assert!(o.status.success());
        assert!(stderr(&o).contains("updated"), "{}", stderr(&o));
    });
}

#[test]
fn ui_declines_a_non_tty_and_runs_on_a_pty() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    init(home);
    run_stdin(home, &["put", "arca://x/y"], FAKE.as_bytes());
    // piped stdout → the TUI declines up front
    let o = run(home, &["ui"]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("terminal"), "{}", stderr(&o));
    // a pty → the browser opens and 'q' quits clean (pty merges
    // stdout+stderr — the audit event lands in script's stdout)
    let o = run_pty(home, &["ui"], b"q");
    assert!(o.status.success(), "{}", stderr(&o));
    let all = format!("{}{}", stdout(&o), stderr(&o));
    assert!(all.contains("\"op\":\"ui\""), "{all}");
}

#[test]
fn tty_paths_get_a_prompt_and_a_note() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    init(home);
    // put on a pty → the hidden /dev/tty prompt path
    let o = run_pty(home, &["put", "arca://x/y"], b"fake-pty-value\n");
    assert!(o.status.success(), "{}", stderr(&o));
    // get on a pty → the "printed to terminal" note
    let o = run_pty(home, &["get", "arca://x/y"], b"");
    assert!(o.status.success(), "{}", stderr(&o));
    let all = format!("{}{}", stdout(&o), stderr(&o));
    assert!(all.contains("fake-pty-value"), "{all}");
    assert!(all.contains("note: secret value"), "{all}");
}

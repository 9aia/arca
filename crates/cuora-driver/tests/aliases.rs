//! `[aliases]` ref remapping (T-052) — config-level old-path → new-ref
//! rewrites applied during resolution, before vault and sealed-pack
//! lookup. Exercised end-to-end through the driver `run` passthrough:
//! temp HOME, real age identity via `run init --with-file`, a hand-built
//! tamias store for the pack target.
//!
//! Every plaintext here is fake by rule.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::process::{Command, Output};

const FAKE: &str = "fake-alias-value-not-real";
const PACK_FAKE: &str = "fake-pack-alias-value-not-real";

struct Env {
    home: tempfile::TempDir,
    tamias_home: PathBuf,
    keyfile: PathBuf,
}

fn driver(env: &Env) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tamias-driver-cuora"));
    c.env("HOME", env.home.path().join("home"))
        .env("XDG_CONFIG_HOME", env.home.path().join("home/xdg-config"))
        .env("XDG_DATA_HOME", env.home.path().join("home/xdg-data"))
        .env("CUORA_IDENTITY_FILE", &env.keyfile)
        .env("TAMIAS_HOME", &env.tamias_home)
        .env_remove("CUORA_CONFIG")
        .env_remove("CUORA_ENV")
        .env_remove("CUORA_ACTOR");
    c
}

fn run(env: &Env, args: &[&str]) -> Output {
    driver(env).args(args).output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Boot a temp env and write `cuora.toml` under the temp HOME's
/// `~/.config/cuora/` — the default config search path.
fn setup(config: &str) -> Env {
    let home = tempfile::tempdir().unwrap();
    let env = Env {
        tamias_home: home.path().join("tamias-home"),
        keyfile: home.path().join("xdg-config/cuora/identity.txt"),
        home,
    };
    let out = run(&env, &["run", "init", "--with-file"]);
    assert!(out.status.success(), "init failed: {}", stderr(&out));
    let cfg_dir = env.home.path().join("home/.config/cuora");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(cfg_dir.join("cuora.toml"), config).unwrap();
    env
}

/// `run put <ref>` with the fake value on stdin.
fn put(env: &Env, rref: &str, value: &str) {
    let mut c = driver(env);
    c.args(["run", "put", rref])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    {
        use std::io::Write;
        let mut child = c.spawn().unwrap();
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(value.as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success(), "put {rref} failed");
    }
}

/// A sealed payload + store install, same shape as sealed.rs — the
/// alias target for the pack-fallback test.
fn pack_install(env: &Env, name: &str) {
    let work = env.home.path().join(format!("work-{name}"));
    let src = work.join("plain");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("token.txt"), PACK_FAKE).unwrap();
    let out = work.join("payload");
    let o = run(
        env,
        &[
            "seal",
            "--in",
            src.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--keyfile",
            env.keyfile.to_str().unwrap(),
        ],
    );
    assert!(o.status.success(), "seal failed: {}", stderr(&o));
    std::fs::write(
        out.join("tamias.yaml"),
        format!(
            "name: {name}\nversion: \"1.0.0\"\ntype: secrets\n{}",
            stdout(&o)
        ),
    )
    .unwrap();
    let entry = env.tamias_home.join("store").join(format!("{name}@1.0.0"));
    std::fs::create_dir_all(&entry).unwrap();
    for f in ["tamias.yaml", "sealed/token.age", "sealed/recipients"] {
        let to = entry.join(f);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(out.join(f), &to).unwrap();
    }
    std::fs::write(
        entry.join(".tamias-store.json"),
        format!(
            r#"{{"name":"{name}","version":"1.0.0","kind":"secrets","source_ref":"./x","resolved":"sha256:abc","digest":"d","installed_at":0}}"#
        ),
    )
    .unwrap();
}

/// alias → vault hit: the old path resolves to the new ref's record,
/// and the audit event carries both sides (`rref` = resolved,
/// `alias_from` = requested).
#[test]
fn alias_to_vault_hit_audits_both_refs() {
    let env = setup("[aliases]\n\"retired\" = \"cuora://vaults/new-name\"\n");
    put(&env, "cuora://vaults/new-name/key", FAKE);

    let g = run(&env, &["run", "get", "cuora://retired/key"]);
    assert!(g.status.success(), "get: {}", stderr(&g));
    assert_eq!(stdout(&g).trim_end(), FAKE);
    let err = stderr(&g);
    assert!(err.contains("\"rref\":\"vaults/new-name/key\""), "{err}");
    assert!(err.contains("\"alias_from\":\"retired/key\""), "{err}");

    // a direct hit on the new name carries no alias_from
    let g2 = run(&env, &["run", "get", "cuora://vaults/new-name/key"]);
    assert!(g2.status.success());
    assert!(!stderr(&g2).contains("alias_from"), "{}", stderr(&g2));
}

/// alias → sealed pack: the rewritten two-segment path falls through
/// the normal vault-miss → pack-fallback path.
#[test]
fn alias_to_pack_hit() {
    let env = setup("[aliases]\n\"retired\" = \"cuora://secrets-9aia\"\n");
    pack_install(&env, "secrets-9aia");

    let g = run(&env, &["run", "get", "cuora://retired/token"]);
    assert!(g.status.success(), "get: {}", stderr(&g));
    assert_eq!(stdout(&g).trim_end(), PACK_FAKE);
    let err = stderr(&g);
    assert!(err.contains("\"env\":\"sealed\""), "{err}");
    assert!(err.contains("\"rref\":\"secrets-9aia/token\""), "{err}");
    assert!(err.contains("\"alias_from\":\"retired/token\""), "{err}");
}

/// alias miss: neither vault nor pack answers the rewritten path —
/// the error names the *requested* ref.
#[test]
fn alias_miss_reports_requested_ref() {
    let env = setup("[aliases]\n\"retired\" = \"cuora://vaults/new-name\"\n");
    let g = run(&env, &["run", "get", "cuora://retired/ghost"]);
    assert!(!g.status.success());
    assert!(stderr(&g).contains("not stored"), "{}", stderr(&g));
    assert!(
        stderr(&g).contains("cuora://retired/ghost"),
        "{}",
        stderr(&g)
    );
}

/// `[secrets]` injection resolves through the alias too — the
/// declared surface and ad-hoc `-s` pairs share `resolve`.
#[test]
fn alias_applies_to_declared_and_adhoc_injection() {
    let env = setup(
        "[aliases]\n\"retired\" = \"cuora://vaults/new-name\"\n\n\
         [secrets]\nDECLARED = \"cuora://retired/key\"\n",
    );
    put(&env, "cuora://vaults/new-name/key", FAKE);
    put(&env, "cuora://vaults/new-name/adhoc", "fake-adhoc-value");

    let e = run(&env, &["run", "env", "-s", "ADHOC=cuora://retired/adhoc"]);
    assert!(e.status.success(), "env: {}", stderr(&e));
    let out = stdout(&e);
    assert!(out.contains("export DECLARED="), "{out}");
    assert!(out.contains(FAKE), "{out}");
    assert!(out.contains("export ADHOC="), "{out}");
    assert!(out.contains("fake-adhoc-value"), "{out}");
}

/// env pins strip before aliasing: `cuora://prod/retired/key`
/// aliases `retired/key` but still resolves under `prod`.
#[test]
fn alias_respects_env_pin() {
    let env = setup("[aliases]\n\"retired\" = \"cuora://vaults/new-name\"\n");
    // prod routes to the same local vault by default — the pin
    // changes the env key, not the backend.
    put(&env, "cuora://prod/vaults/new-name/key", FAKE);

    let g = run(&env, &["run", "get", "cuora://prod/retired/key"]);
    assert!(g.status.success(), "get: {}", stderr(&g));
    assert_eq!(stdout(&g).trim_end(), FAKE);
    // same request unpinned misses — dev has no such record
    let g2 = run(&env, &["run", "get", "cuora://retired/key"]);
    assert!(!g2.status.success());
}

/// A self-alias or cycle fails config load — every verb errors with
/// the validation message instead of hanging or misresolving.
#[test]
fn alias_cycle_rejected_at_load() {
    let env = setup("[aliases]\n\"a\" = \"cuora://b\"\n\"b\" = \"cuora://a\"\n");
    let g = run(&env, &["run", "get", "cuora://a/x"]);
    assert!(!g.status.success());
    let err = stderr(&g);
    assert!(err.contains("alias"), "{err}");
    assert!(err.contains("cycle"), "{err}");
    // self-alias too
    let env2 = setup("[aliases]\n\"a\" = \"cuora://a\"\n");
    let g2 = run(&env2, &["run", "list"]);
    assert!(!g2.status.success());
    assert!(stderr(&g2).contains("alias"), "{}", stderr(&g2));
}

/// Longest-prefix wins and multi-segment keys work — the namespace
/// dedupe shape (`old-team/*` → `new-team/*`).
#[test]
fn alias_longest_prefix_and_namespace_move() {
    let env = setup(
        "[aliases]\n\"bupha\" = \"cuora://peregrin\"\n\"bupha/token\" = \"cuora://secrets-9aia/token\"\n",
    );
    put(&env, "cuora://peregrin/queue/depth", FAKE);
    pack_install(&env, "secrets-9aia");

    // namespace prefix: bupha/queue/depth → peregrin/queue/depth
    let g = run(&env, &["run", "get", "cuora://bupha/queue/depth"]);
    assert!(g.status.success(), "get: {}", stderr(&g));
    assert_eq!(stdout(&g).trim_end(), FAKE);

    // longer key beats the namespace prefix: the specific ref goes
    // to the sealed pack, not peregrin/token.
    let g2 = run(&env, &["run", "get", "cuora://bupha/token"]);
    assert!(g2.status.success(), "get: {}", stderr(&g2));
    assert_eq!(stdout(&g2).trim_end(), PACK_FAKE);
}

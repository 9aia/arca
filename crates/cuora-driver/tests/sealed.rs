//! Sealed-pack driver surface (tamias ADR-0004 / PL-017 T3) — the
//! `seal`/`open`/`materialize`/`recipients`/`packs` argv verbs plus
//! the `install`/`deploy`/`uninstall` lifecycle no-ops, exercised
//! end-to-end against the real binary: temp HOME, a real age
//! keyfile (via `run init --with-file`), a hand-built tamias store.
//!
//! Every plaintext here is fake by rule.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FAKE: &str = "fake-sealed-value-not-real";

struct Env {
    home: tempfile::TempDir,
    /// `$TAMIAS_HOME` — the store root's parent (`<TAMIAS_HOME>/store`).
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

/// Boot a temp env: home dirs, `run init --with-file` for a real
/// age identity keyfile, and an empty tamias store.
fn setup() -> Env {
    let home = tempfile::tempdir().unwrap();
    let env = Env {
        tamias_home: home.path().join("tamias-home"),
        keyfile: home.path().join("xdg-config/cuora/identity.txt"),
        home,
    };
    // `run init --with-file` bootstraps identity + vault under the
    // temp XDG dirs — the same path `cuora init` takes.
    let out = run(&env, &["run", "init", "--with-file"]);
    assert!(out.status.success(), "init failed: {}", stderr(&out));
    assert!(env.keyfile.is_file(), "keyfile missing");
    env
}

/// A sealed payload dir built by `seal` + a hand-written manifest —
/// the author-side flow, no tamias pack step needed.
fn sealed_payload(env: &Env, name: &str) -> PathBuf {
    let work = env.home.path().join(format!("work-{name}"));
    let src = work.join("plain");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("token.txt"), FAKE).unwrap();
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
    // The printed `sealed:` block merged into tamias.yaml.
    std::fs::write(
        out.join("tamias.yaml"),
        format!(
            "name: {name}\nversion: \"1.0.0\"\ntype: secrets\n{}",
            stdout(&o)
        ),
    )
    .unwrap();
    out
}

/// Install the payload into the fake store the way `tamias install`
/// would leave it: `<TAMIAS_HOME>/store/<name>@<ver>/` payload +
/// `.tamias-store.json`.
fn store_install(env: &Env, payload: &Path, name: &str) -> PathBuf {
    let entry = env.tamias_home.join("store").join(format!("{name}@1.0.0"));
    std::fs::create_dir_all(&entry).unwrap();
    for f in ["tamias.yaml", "sealed/token.age", "sealed/recipients"] {
        let from = payload.join(f);
        let to = entry.join(f);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(&from, &to).unwrap();
    }
    std::fs::write(
        entry.join(".tamias-store.json"),
        format!(
            r#"{{"name":"{name}","version":"1.0.0","kind":"secrets","source_ref":"./x","resolved":"sha256:abc","digest":"d","installed_at":0}}"#
        ),
    )
    .unwrap();
    entry
}

#[test]
fn seal_open_materialize_recipients_packs_roundtrip() {
    let env = setup();
    let payload = sealed_payload(&env, "secrets-test");

    // ciphertext only: .age files carry the age magic and no plaintext
    let age_bytes = std::fs::read(payload.join("sealed/token.age")).unwrap();
    assert!(age_bytes.starts_with(b"age-encryption.org/v1"));
    assert!(!age_bytes.windows(FAKE.len()).any(|w| w == FAKE.as_bytes()));

    // install into the fake store, then resolve by name + cuora:// ref
    store_install(&env, &payload, "secrets-test");

    let packs = run(&env, &["packs"]);
    assert!(packs.status.success(), "packs: {}", stderr(&packs));
    assert!(stdout(&packs).contains("secrets-test@1.0.0"));

    let open = run(
        &env,
        &[
            "open",
            "cuora://secrets-test/token",
            "--keyfile",
            env.keyfile.to_str().unwrap(),
        ],
    );
    assert!(open.status.success(), "open: {}", stderr(&open));
    assert_eq!(stdout(&open).trim_end(), FAKE);
    // the materialization hit the audit stream
    assert!(stderr(&open).contains("\"op\":\"get\""));

    let recipients = run(&env, &["recipients", "secrets-test"]);
    assert!(
        recipients.status.success(),
        "recipients: {}",
        stderr(&recipients)
    );
    assert!(stdout(&recipients).contains("← local keyfile"));

    let mdir = env.home.path().join("materialized");
    let mat = run(
        &env,
        &[
            "materialize",
            "secrets-test",
            "--out",
            mdir.to_str().unwrap(),
            "--keyfile",
            env.keyfile.to_str().unwrap(),
        ],
    );
    assert!(mat.status.success(), "materialize: {}", stderr(&mat));
    assert_eq!(std::fs::read_to_string(mdir.join("token")).unwrap(), FAKE);

    // --out writes 0600
    let outf = env.home.path().join("opened.txt");
    let o2 = run(
        &env,
        &[
            "open",
            "secrets-test",
            "token",
            "--out",
            outf.to_str().unwrap(),
            "--keyfile",
            env.keyfile.to_str().unwrap(),
        ],
    );
    assert!(o2.status.success(), "open --out: {}", stderr(&o2));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&outf).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn open_needs_the_right_key_and_recipients_lists() {
    let env = setup();
    let payload = sealed_payload(&env, "secrets-a");
    store_install(&env, &payload, "secrets-a");

    // a second identity can't open it
    let other_home = tempfile::tempdir().unwrap();
    let other_kf = other_home.path().join("id.txt");
    // mint a second keyfile via the engine under a different HOME
    let mut c = Command::new(env!("CARGO_BIN_EXE_tamias-driver-cuora"));
    c.arg("run")
        .args(["init", "--with-file"])
        .env("HOME", other_home.path().join("home"))
        .env("XDG_CONFIG_HOME", other_home.path().join("home/xdg-config"))
        .env("XDG_DATA_HOME", other_home.path().join("home/xdg-data"))
        .env("CUORA_IDENTITY_FILE", &other_kf)
        .env_remove("CUORA_CONFIG")
        .output()
        .unwrap();
    assert!(other_kf.is_file());
    let bad = run(
        &env,
        &[
            "open",
            "secrets-a",
            "token",
            "--keyfile",
            other_kf.to_str().unwrap(),
        ],
    );
    assert!(!bad.status.success());
    assert!(!stdout(&bad).contains(FAKE));

    // unknown secret / unknown pack fail honestly
    let missing = run(&env, &["open", "secrets-a", "ghost"]);
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("ghost"));
    let nopack = run(&env, &["open", "secrets-ghost", "token"]);
    assert!(!nopack.status.success());
}

#[test]
fn lifecycle_verbs_are_noops_only_for_sealed() {
    let env = setup();
    let payload = sealed_payload(&env, "secrets-life");
    // tamias dispatch shape: <verb> --dir <payload> --name <pkg>
    for verb in ["install", "deploy", "uninstall"] {
        let o = run(
            &env,
            &[
                verb,
                "--dir",
                payload.to_str().unwrap(),
                "--name",
                "secrets-life",
            ],
        );
        assert!(o.status.success(), "{verb}: {}", stderr(&o));
    }
    // a plaintext pack is refused loudly
    let plain = env.home.path().join("plain-payload");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("tamias.yaml"), "name: x\ntype: secrets\n").unwrap();
    let o = run(
        &env,
        &["install", "--dir", plain.to_str().unwrap(), "--name", "x"],
    );
    assert!(!o.status.success());
}

#[test]
fn seal_rejects_bad_input_and_prints_manifest_block() {
    let env = setup();
    let src = env.home.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.txt"), FAKE).unwrap();
    let out = env.home.path().join("out");

    // no recipients → error
    let o = run(
        &env,
        &[
            "seal",
            "--in",
            src.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ],
    );
    assert!(!o.status.success());

    // with a recipient → stdout carries the sealed: block
    let o = run(
        &env,
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
    assert!(o.status.success(), "seal: {}", stderr(&o));
    let block = stdout(&o);
    assert!(block.contains("sealed:"));
    assert!(block.contains("format: age/v1"));
    assert!(block.contains("a: sealed/a.age"));
}

// ------------------------------------------------ engine pack refs
//
// T-051: `cuora://<pack>/<secret>` resolves against installed
// sealed packs everywhere vault refs do — `get`, `run -s`,
// `env`, `check`. Vault-first: a vault hit wins over a pack hit.

/// `run get cuora://<pack>/<secret>` — the escape hatch.
#[test]
fn engine_get_resolves_pack_ref() {
    let env = setup();
    let payload = sealed_payload(&env, "secrets-engine");
    store_install(&env, &payload, "secrets-engine");

    let g = run(&env, &["run", "get", "cuora://secrets-engine/token"]);
    assert!(g.status.success(), "get: {}", stderr(&g));
    assert_eq!(stdout(&g).trim_end(), FAKE);
    // pack opens audit with env "sealed", op "get" — same shape
    // as the driver's `open` verb.
    assert!(stderr(&g).contains("\"env\":\"sealed\""), "{}", stderr(&g));
    assert!(stderr(&g).contains("\"op\":\"get\""), "{}", stderr(&g));
}

/// `run env -s VAR=cuora://<pack>/<secret>` — injection surface.
#[test]
fn engine_env_and_run_inject_pack_refs() {
    let env = setup();
    let payload = sealed_payload(&env, "secrets-inject");
    store_install(&env, &payload, "secrets-inject");

    let e = run(
        &env,
        &["run", "env", "-s", "TOK=cuora://secrets-inject/token"],
    );
    assert!(e.status.success(), "env: {}", stderr(&e));
    let line = stdout(&e);
    assert!(line.contains("export TOK="), "{line}");
    assert!(line.contains(FAKE), "{line}");
    assert!(stderr(&e).contains("\"env\":\"sealed\""), "{}", stderr(&e));

    // `run --` with the var consumed by the child proves the whole
    // inject→exec path, not just resolution. (driver `run` verb →
    // engine argv `run -s … -- sh -c …`.)
    let r = run(
        &env,
        &[
            "run",
            "run",
            "-s",
            "TOK=cuora://secrets-inject/token",
            "--",
            "sh",
            "-c",
            "test \"$TOK\" = 'fake-sealed-value-not-real'",
        ],
    );
    assert!(r.status.success(), "run: {}", stderr(&r));
}

/// Vault-first: a vault record at the same ref path shadows the
/// pack — packs are the migration target, never an override.
#[test]
fn engine_vault_wins_over_pack() {
    let env = setup();
    let payload = sealed_payload(&env, "secrets-shadow");
    store_install(&env, &payload, "secrets-shadow");

    // Store a *different* value in the vault at the same path —
    // `cuora put` reads the value from stdin.
    let mut put = driver(&env);
    put.args(["run", "put", "cuora://secrets-shadow/token"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    {
        use std::io::Write;
        let mut child = put.spawn().unwrap();
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"vault-value-wins")
            .unwrap();
        assert!(child.wait().unwrap().success());
    }

    let g = run(&env, &["run", "get", "cuora://secrets-shadow/token"]);
    assert!(g.status.success(), "get: {}", stderr(&g));
    assert_eq!(stdout(&g).trim_end(), "vault-value-wins");
    // the vault hit audits under the real env, not "sealed"
    assert!(!stderr(&g).contains("\"env\":\"sealed\""), "{}", stderr(&g));
}

/// Honest misses: undeclared pack secret and unknown pack both
/// report not-stored; `check` resolves declared pack refs without
/// decrypting.
#[test]
fn engine_pack_ref_misses_and_check() {
    let env = setup();
    let payload = sealed_payload(&env, "secrets-miss");
    store_install(&env, &payload, "secrets-miss");

    let ghost = run(&env, &["run", "get", "cuora://secrets-miss/ghost"]);
    assert!(!ghost.status.success());
    assert!(stderr(&ghost).contains("not stored"), "{}", stderr(&ghost));

    let nopack = run(&env, &["run", "get", "cuora://secrets-ghost/token"]);
    assert!(!nopack.status.success());
    assert!(
        stderr(&nopack).contains("not stored"),
        "{}",
        stderr(&nopack)
    );

    // `check` with a declared [secrets] pack ref resolves without
    // an identity/decrypt — config search is ~/.config/cuora/.
    let cfg_dir = env.home.path().join("home/.config/cuora");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("cuora.toml"),
        "[secrets]\nTOK = \"cuora://secrets-miss/token\"\nGHOST = \"cuora://secrets-miss/ghost\"\n",
    )
    .unwrap();
    let c = run(&env, &["run", "check"]);
    // check exits 1 on the GHOST finding but must mark TOK resolved
    let out = format!("{}{}", stdout(&c), stderr(&c));
    assert!(out.contains("TOK"), "{out}");
    let tok_line = out.lines().find(|l| l.contains("TOK")).unwrap();
    assert!(tok_line.contains("resolves"), "{tok_line}");
    let ghost_line = out.lines().find(|l| l.contains("GHOST")).unwrap();
    assert!(ghost_line.contains("NOT STORED"), "{ghost_line}");
}

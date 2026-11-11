//! xtask — repo chores. `cargo xtask <verb>` via the alias in
//! `.cargo/config.toml`.
//!
//! - `check`    — `cargo fmt --check` + `cargo clippy -D warnings`
//! - `test`     — `cargo nextest run` when installed, else `cargo test`;
//!   plus the worker smoke test when `bun` is installed
//! - `worker`   — the worker smoke test: `bun test` in
//!   crates/cuora-vault-cf/worker — executes the REAL index.js over
//!   a bun:sqlite D1 shim (the Rust mock can't catch worker bugs)
//! - `coverage` — `cargo llvm-cov` (HTML → target/llvm-cov/html);
//!   extra args pass through, e.g. `--fail-under-lines 100`
//! - `pack`     — stage the tamias pack payload: release-build
//!   `tamias-driver-cuora` into `libraries/cuora/drivers/bin/<triple>/`
//!   (gitignored build artifact) for `tamias install ./libraries/cuora`
//! - `install`  — release-install `tamias-driver-cuora` into
//!   ~/.cargo/bin (the `cuora` command itself is the tamias pack's
//!   generated shim — see `pack`)
//! - `site`     — build the docs site (docs/ worktree, VitePress)

use std::process::{Command, ExitStatus};

fn run(args: &[&str]) -> ExitStatus {
    let empty: &[&str] = &[];
    let (prog, rest) = args.split_first().unwrap_or((&"true", empty));
    Command::new(prog).args(rest).status().unwrap_or_else(|e| {
        eprintln!("xtask: failed to spawn {prog}: {e}");
        std::process::exit(1)
    })
}

/// `run` with a working directory — for commands that must execute
/// inside a child tree (the docs/ worktree).
fn run_in(dir: &str, args: &[&str]) -> ExitStatus {
    let empty: &[&str] = &[];
    let (prog, rest) = args.split_first().unwrap_or((&"true", empty));
    Command::new(prog)
        .args(rest)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("xtask: failed to spawn {prog}: {e}");
            std::process::exit(1)
        })
}

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn have_cargo_sub(sub: &str) -> bool {
    Command::new("cargo")
        .args([sub, "--version"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn check() -> i32 {
    let ok = run(&["cargo", "fmt", "--all", "--", "--check"]).success()
        && run(&[
            "cargo",
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ])
        .success();
    if ok {
        0
    } else {
        1
    }
}

fn test() -> i32 {
    let code = if have_cargo_sub("nextest") {
        run(&["cargo", "nextest", "run", "--workspace"])
    } else {
        run(&["cargo", "test", "--workspace"])
    }
    .code()
    .unwrap_or(1);
    // The worker smoke test rides the suite when bun is around —
    // skipped (with a note) in lean envs so the Rust tests still
    // run everywhere.
    if have("bun") {
        let w = run_in("crates/cuora-vault-cf/worker", &["bun", "test"])
            .code()
            .unwrap_or(1);
        return if code == 0 { w } else { code };
    }
    eprintln!("xtask: bun not installed — skipping the worker smoke test (cargo xtask worker)");
    code
}

/// `bun test` in the worker dir — the smoke test imports the real
/// index.js and drives it over a bun:sqlite D1 shim.
fn worker() -> i32 {
    if !have("bun") {
        eprintln!("bun not installed — needed for the worker smoke test");
        return 1;
    }
    run_in("crates/cuora-vault-cf/worker", &["bun", "test"])
        .code()
        .unwrap_or(1)
}

fn coverage(extra: &[String]) -> i32 {
    if !have_cargo_sub("llvm-cov") {
        eprintln!("cargo-llvm-cov not installed — cargo install cargo-llvm-cov");
        return 1;
    }
    let mut args = vec!["cargo", "llvm-cov"];
    if have_cargo_sub("nextest") {
        args.push("nextest");
        args.extend(["--workspace", "--no-tests=pass"]);
    } else {
        args.push("--workspace");
    }
    if extra.is_empty() {
        args.push("--html");
    }
    let owned: Vec<&str> = extra.iter().map(String::as_str).collect();
    args.extend(owned);
    run(&args).code().unwrap_or(1)
}

/// Captured stdout of a command — for probes like `rustc -vV`.
fn run_out(args: &[&str]) -> Option<String> {
    let (prog, rest) = args.split_first()?;
    let out = Command::new(prog).args(rest).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The host target triple — the `drivers/bin/<triple>/` selector
/// tamias uses for compiled driver payloads.
fn host_triple() -> String {
    run_out(&["rustc", "-vV"])
        .and_then(|v| {
            v.lines()
                .find_map(|l| l.strip_prefix("host: ").map(str::to_string))
        })
        .unwrap_or_else(|| "x86_64-unknown-linux-gnu".to_string())
}

/// `pack` — stage the tamias pack payload (P-023): release-build
/// `tamias-driver-cuora` and drop it under
/// `libraries/cuora/drivers/bin/<host-triple>/` — the bundled-driver
/// dir `tamias install` registers into `<tamias-home>/drivers/`.
/// Gitignored on purpose: the binary is a build artifact; the
/// committed pack is just `tamias.yaml` + this staging verb.
fn pack() -> i32 {
    if run(&["cargo", "build", "--release", "-p", "cuora-driver"]).code() != Some(0) {
        return 1;
    }
    let triple = host_triple();
    let dest_dir = std::path::PathBuf::from("libraries/cuora/drivers/bin").join(&triple);
    if let Err(e) = std::fs::create_dir_all(&dest_dir) {
        eprintln!("xtask: create {}: {e}", dest_dir.display());
        return 1;
    }
    let src = std::path::PathBuf::from("target/release/tamias-driver-cuora");
    let dest = dest_dir.join("tamias-driver-cuora");
    if let Err(e) = std::fs::copy(&src, &dest) {
        eprintln!("xtask: copy {} → {}: {e}", src.display(), dest.display());
        return 1;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755));
    }
    println!("pack staged: {}", dest.display());
    println!();
    println!("install the cuora command (generated bins[] shim):");
    println!("  tamias install --allow-drivers ./libraries/cuora");
    println!("  # or a pinned tarball: tamias bundle pack libraries/cuora");
    0
}

fn install() -> i32 {
    // There is no `cuora` cargo bin anymore — the command ships as a
    // tamias pack (`cargo xtask pack` + `tamias install
    // ./libraries/cuora`). This installs the driver binary itself so
    // `tamias-driver-cuora` resolves on PATH for pavo/driver
    // discovery even without the pack.
    let code = run(&[
        "cargo",
        "install",
        "--path",
        "crates/cuora-driver",
        "--force",
    ])
    .code()
    .unwrap_or(1);
    if code == 0 {
        eprintln!("xtask: tamias-driver-cuora installed — the `cuora` command comes from `cargo xtask pack` + `tamias install ./libraries/cuora`");
    }
    code
}

fn site() -> i32 {
    if !std::path::Path::new("docs/package.json").exists() {
        eprintln!("no docs/ worktree — `git worktree add docs docs` first");
        return 1;
    }
    // run inside docs/ — works for bun and npm alike (`bun --cwd`
    // isn't a `bun run` flag, so flag plumbing gets fragile)
    let pkg = if have("bun") { "bun" } else { "npm" };
    let _ = run_in("docs", &[pkg, "install"]).success();
    run_in("docs", &[pkg, "run", "docs:build"])
        .code()
        .unwrap_or(1)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_default();
    let rest: Vec<String> = args.collect();
    let code = match cmd.as_str() {
        "check" => check(),
        "test" => test(),
        "worker" => worker(),
        "coverage" => coverage(&rest),
        "pack" => pack(),
        "install" => install(),
        "site" => site(),
        other => {
            eprintln!(
                "usage: cargo xtask <check|test|worker|coverage|pack|install|site>\n  unknown verb: {other:?}"
            );
            2
        }
    };
    std::process::exit(code);
}

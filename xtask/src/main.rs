//! xtask — repo chores. `cargo xtask <verb>` via the alias in
//! `.cargo/config.toml`.
//!
//! - `check`    — `cargo fmt --check` + `cargo clippy -D warnings`
//! - `test`     — `cargo nextest run` when installed, else `cargo test`
//! - `coverage` — `cargo llvm-cov` (HTML → target/llvm-cov/html);
//!   extra args pass through, e.g. `--fail-under-lines 100`
//! - `install`  — release-install `arca` into ~/.cargo/bin
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
    if have_cargo_sub("nextest") {
        run(&["cargo", "nextest", "run", "--workspace"])
    } else {
        run(&["cargo", "test", "--workspace"])
    }
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

fn install() -> i32 {
    run(&["cargo", "install", "--path", "crates/arca-cli", "--force"])
        .code()
        .unwrap_or(1)
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
        "coverage" => coverage(&rest),
        "install" => install(),
        "site" => site(),
        other => {
            eprintln!(
                "usage: cargo xtask <check|test|coverage|install|site>\n  unknown verb: {other:?}"
            );
            2
        }
    };
    std::process::exit(code);
}

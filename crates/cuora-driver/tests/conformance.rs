//! Driver conformance gate — runs pavo's wire-contract suite
//! ([`pavo_testkit::conformance::check_driver`]) against this crate's
//! own `tamias-driver-cuora serve` binary.
//!
//! The driver is read-only over vault *metadata* — it never renders
//! secret values, so the conformance run can't leak anything. The
//! spawned driver inherits this test's cwd (`crates/cuora-driver`,
//! inside the cuora repo).

use std::path::Path;

#[test]
fn driver_passes_pavo_conformance() {
    // Isolate the spawned driver from the developer's real cuora
    // config — it inherits this test's env, and a real
    // `~/.config/cuora/cuora.toml` can route views at a remote cf
    // vault whose multi-second ops blow the suite's 10s per-request
    // timeout (late responses then misalign every later check).
    // An empty HOME gives the default local vault: fast, offline.
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("HOME", home.path());
    std::env::set_var("XDG_CONFIG_HOME", home.path().join(".config"));
    std::env::remove_var("CUORA_CONFIG");
    std::env::remove_var("CUORA_ENV");
    std::env::remove_var("CUORA_ACTOR");
    let violations = pavo_testkit::conformance::check_driver(Path::new(env!(
        "CARGO_BIN_EXE_tamias-driver-cuora"
    )));
    assert!(
        violations.is_empty(),
        "pavo conformance violations:\n{}",
        violations
            .iter()
            .map(|v| format!("  - {v}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

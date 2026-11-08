//! `cuora-vault-age` against the shared vault conformance suite —
//! each test opens a fresh vault in a fresh tempdir.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use cuora_vault_age::AgeVault;

cuora_testkit::vault_conformance!(|| {
    let dir = std::env::temp_dir().join(format!(
        "cuora-age-conform-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let vault: PathBuf = dir.join("vault.age");
    let identity = age::x25519::Identity::generate();
    AgeVault::open(&vault, identity, &[]).unwrap()
});

//! `arca-vault-age` against the shared vault conformance suite —
//! each test opens a fresh vault in a fresh tempdir.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use arca_vault_age::AgeVault;

arca_testkit::vault_conformance!(|| {
    let dir = std::env::temp_dir().join(format!(
        "arca-age-conform-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let vault: PathBuf = dir.join("vault.age");
    let identity: PathBuf = dir.join("identity.txt");
    AgeVault::ensure_identity(&identity).unwrap();
    AgeVault::open(&vault, &identity, &[]).unwrap()
});

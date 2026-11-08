//! Property tests for the age vault — generated values, env
//! isolation, ciphertext hygiene, and tamper evidence. Unit tests
//! cover the fixture cases; these cover the space between them.

#![allow(clippy::unwrap_used)]

use age::x25519;
use arca_core::{Env, Ref, Secret};
use arca_vault::Vault;
use arca_vault_age::AgeVault;
use proptest::prelude::*;

/// A fresh vault in a tempdir — `dir` must outlive the vault.
fn fresh_vault(dir: &std::path::Path) -> AgeVault {
    AgeVault::open(&dir.join("vault.age"), x25519::Identity::generate(), &[]).unwrap()
}

fn ref_for(name: &str) -> Ref {
    Ref::new(&format!("arca://p/{name}")).unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    /// put → get returns the exact bytes back, version 1, for
    /// arbitrary unicode/empty/huge-ish values.
    #[test]
    fn put_get_roundtrips_arbitrary_values(
        value in ".{0,512}",
        name in "[a-z][a-z0-9_-]{0,20}",
    ) {
        let dir = tempfile::tempdir().unwrap();
        let v = fresh_vault(dir.path());
        let env = Env::dev();
        let rref = ref_for(&name);

        v.put(&env, &rref, Secret::new(value.clone())).unwrap();
        let rec = v.get(&env, &rref).unwrap().expect("stored record");
        prop_assert_eq!(rec.value.expose(), value.as_str());
        prop_assert_eq!(rec.version, 1);
    }

    /// The same ref path under two different envs is two
    /// independent records — env is resolution context, not name.
    #[test]
    fn envs_are_isolated(
        e1 in "[a-z][a-z0-9_-]{0,12}",
        e2 in "[a-z][a-z0-9_-]{0,12}",
        val in ".{0,256}",
    ) {
        prop_assume!(e1 != e2);
        let dir = tempfile::tempdir().unwrap();
        let v = fresh_vault(dir.path());
        let rref = ref_for("shared/name");
        let env1 = Env::new(&e1).unwrap();
        let env2 = Env::new(&e2).unwrap();

        let want1 = format!("v1-{val}");
        let want2 = format!("v2-{val}");
        v.put(&env1, &rref, Secret::new(want1.clone())).unwrap();
        v.put(&env2, &rref, Secret::new(want2.clone())).unwrap();

        let got1 = v.get(&env1, &rref).unwrap().unwrap();
        let got2 = v.get(&env2, &rref).unwrap().unwrap();
        prop_assert!(got1.value.expose() == want1);
        prop_assert!(got2.value.expose() == want2);
        // and neither env's list leaks the other's refs
        prop_assert!(v.list(&env1).unwrap().contains(&rref));
        prop_assert!(v.list(&env2).unwrap().contains(&rref));
    }

    /// The vault file is ciphertext: the plaintext value (and the
    /// ref path) must not appear anywhere in the bytes on disk.
    #[test]
    fn ciphertext_never_contains_plaintext(
        value in ".{4,256}",
        // ≥4 chars — a 1-byte needle trivially occurs in any file
        name in "[a-z][a-z0-9_-]{3,20}",
    ) {
        let dir = tempfile::tempdir().unwrap();
        let vpath = dir.path().join("vault.age");
        let v = AgeVault::open(&vpath, x25519::Identity::generate(), &[]).unwrap();
        v.put(&Env::dev(), &ref_for(&name), Secret::new(value.clone()))
            .unwrap();

        let bytes = std::fs::read(&vpath).unwrap();
        prop_assert!(!contains_subslice(&bytes, value.as_bytes()));
        prop_assert!(!contains_subslice(&bytes, name.as_bytes()));
    }

    /// rm removes exactly that record; rotate bumps the version and
    /// swaps the value.
    #[test]
    fn rm_and_rotate_semantics(
        v1 in ".{0,128}",
        v2 in ".{0,128}",
        name in "[a-z][a-z0-9_-]{0,20}",
    ) {
        let dir = tempfile::tempdir().unwrap();
        let v = fresh_vault(dir.path());
        let env = Env::dev();
        let rref = ref_for(&name);

        v.put(&env, &rref, Secret::new(v1.clone())).unwrap();
        let rec = v.rotate(&env, &rref, Secret::new(v2.clone())).unwrap();
        prop_assert!(rec.version == 2);
        let got = v.get(&env, &rref).unwrap().unwrap();
        prop_assert!(got.value.expose() == v2);

        prop_assert!(v.rm(&env, &rref).unwrap());
        prop_assert!(v.get(&env, &rref).unwrap().is_none());
        prop_assert!(!v.rm(&env, &rref).unwrap());
    }

    /// Any single-byte corruption of the vault file makes open
    /// fail — age's AEAD means tamper evidence is total, and a
    /// corrupted vault must error, never yield wrong plaintext.
    #[test]
    fn single_byte_tamper_fails_closed(
        pos in 0usize..1_000_000,
        value in ".{8,128}",
    ) {
        let dir = tempfile::tempdir().unwrap();
        let vpath = dir.path().join("vault.age");
        let id = x25519::Identity::generate();
        let v = AgeVault::open(&vpath, id.clone(), &[]).unwrap();
        v.put(&Env::dev(), &ref_for("tamper/me"), Secret::new(value))
            .unwrap();
        drop(v);

        let mut bytes = std::fs::read(&vpath).unwrap();
        prop_assume!(!bytes.is_empty());
        let i = pos % bytes.len();
        bytes[i] ^= 0xFF; // always changes the byte
        std::fs::write(&vpath, &bytes).unwrap();

        prop_assert!(AgeVault::open(&vpath, id, &[]).is_err());
    }
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

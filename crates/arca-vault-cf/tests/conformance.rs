//! `arca-vault-cf` against the shared vault conformance suite —
//! every test goes through real HTTP to the in-memory wire mock
//! (the same semantics `worker/index.js` implements against D1).

#![allow(clippy::unwrap_used)]

use age::x25519;
use arca_core::{Env, Ref, Secret};
use arca_testkit::httpmock::HttpMock;
use arca_testkit::mocks;
use arca_vault::{SecretRecord, Vault, VaultError};
use arca_vault_cf::CfVault;

/// The token every request must bear — a fake, obviously.
const TOKEN: &str = "test-token-not-real";

/// A `CfVault` plus the mock server that keeps it reachable — the
/// mock must live as long as the vault under test.
struct VaultOnMock {
    mock: HttpMock,
    inner: CfVault,
}

fn fresh_vault() -> VaultOnMock {
    let mock = mocks::vault_wire(TOKEN);
    let identity = x25519::Identity::generate();
    let inner = CfVault::with_identity(mock.url(), TOKEN, identity, &[]).unwrap();
    VaultOnMock { mock, inner }
}

impl Vault for VaultOnMock {
    fn get(&self, env: &Env, rref: &Ref) -> Result<Option<SecretRecord>, VaultError> {
        self.inner.get(env, rref)
    }
    fn put(&self, env: &Env, rref: &Ref, value: Secret) -> Result<SecretRecord, VaultError> {
        self.inner.put(env, rref, value)
    }
    fn rm(&self, env: &Env, rref: &Ref) -> Result<bool, VaultError> {
        self.inner.rm(env, rref)
    }
    fn list(&self, env: &Env) -> Result<Vec<Ref>, VaultError> {
        self.inner.list(env)
    }
    fn rotate(&self, env: &Env, rref: &Ref, value: Secret) -> Result<SecretRecord, VaultError> {
        self.inner.rotate(env, rref, value)
    }
}

arca_testkit::vault_conformance!(fresh_vault);

#[test]
fn bad_token_is_an_error_not_a_panic() {
    let mock = mocks::vault_wire(TOKEN);
    let identity = x25519::Identity::generate();
    let v = CfVault::with_identity(mock.url(), "wrong-token", identity, &[]).unwrap();
    let err = v
        .get(&Env::dev(), &Ref::new("arca://x/y").unwrap())
        .unwrap_err();
    assert!(err.to_string().contains("unauthorized"), "{err}");
}

#[test]
fn unreachable_host_is_io_error_not_panic() {
    let identity = x25519::Identity::generate();
    // A port nothing listens on — connection refused is a clean
    // VaultError, never a panic or a hang.
    let v = CfVault::with_identity("http://127.0.0.1:1", "t", identity, &[]).unwrap();
    match v.list(&Env::dev()) {
        Err(VaultError::Io(_)) => {}
        other => panic!("expected VaultError::Io, got {other:?}"),
    }
}

#[test]
fn ciphertext_only_on_the_wire() {
    let vault = fresh_vault();
    vault
        .inner
        .put(
            &Env::dev(),
            &Ref::new("arca://billing/stripe").unwrap(),
            Secret::new(arca_testkit::FAKE),
        )
        .unwrap();
    // Fetch the stored row as the server serves it — the body must
    // carry ciphertext only, never the plaintext.
    let raw = ureq::get(format!("{}/v1/dev/billing/stripe", vault.mock.url()))
        .header("Authorization", &format!("Bearer {TOKEN}"))
        .call()
        .unwrap()
        .body_mut()
        .read_to_string()
        .unwrap();
    assert!(!raw.contains(arca_testkit::FAKE), "wire carried plaintext");
}

//! `arca-deploy-cf` against a mock of the CF API v4 Secrets Store
//! surface — request construction, upsert semantics, and the
//! envelope error path. Values are fake by rule.

#![allow(clippy::unwrap_used)]

use arca_core::{Env, Ref, Secret};
use arca_deploy::Deploy;
use arca_deploy_cf::CfSecretsStore;
use arca_testkit::mocks;

const FAKE: &str = "fake-value-not-a-real-secret";
const TOKEN: &str = "fake-cf-token";

fn target(url: &str) -> CfSecretsStore {
    CfSecretsStore::with_token("prod", "acc", "sto", TOKEN, Some(url))
}

#[test]
fn push_creates_then_updates() {
    let (mock, seen) = mocks::cf_secrets_store("acc", "sto", TOKEN);
    let t = target(mock.url());
    let env = Env::new("prod").unwrap();
    let rref = Ref::new("arca://billing/stripe").unwrap();

    let report = t.push(&env, &rref, &Secret::new(FAKE)).unwrap();
    assert_eq!(report.name, "prod_billing_stripe");
    assert!(report.created);

    // create used POST with the array body carrying the value
    let post = seen
        .lock()
        .unwrap()
        .iter()
        .find(|r| r.method == "POST")
        .unwrap()
        .text();
    assert!(post.contains(FAKE), "POST body: {post}");
    assert!(post.contains("prod_billing_stripe"), "{post}");

    // second push: search finds it → PATCH on the id path
    let report = t.push(&env, &rref, &Secret::new("fake-v2")).unwrap();
    assert!(!report.created);
    let patch = seen
        .lock()
        .unwrap()
        .iter()
        .find(|r| r.method == "PATCH")
        .unwrap()
        .clone();
    assert!(patch.path.contains("/secrets/id0001"), "{:?}", patch.path);
    assert!(patch.text().contains("fake-v2"));
}

#[test]
fn remove_true_then_false() {
    let (mock, _seen) = mocks::cf_secrets_store("acc", "sto", TOKEN);
    let t = target(mock.url());
    let env = Env::new("prod").unwrap();
    let rref = Ref::new("arca://x/y").unwrap();
    t.push(&env, &rref, &Secret::new(FAKE)).unwrap();
    assert!(t.remove(&env, &rref).unwrap());
    assert!(!t.remove(&env, &rref).unwrap());
}

#[test]
fn bad_token_maps_to_unauthorized() {
    let (mock, _seen) = mocks::cf_secrets_store("acc", "sto", TOKEN);
    let t = CfSecretsStore::with_token("prod", "acc", "sto", "wrong", Some(mock.url()));
    let err = t
        .push(
            &Env::dev(),
            &Ref::new("arca://x/y").unwrap(),
            &Secret::new(FAKE),
        )
        .unwrap_err();
    assert!(
        matches!(err, arca_deploy::DeployError::Unauthorized(_)),
        "{err}"
    );
}

#[test]
fn sink_names_are_stable() {
    let env = Env::new("prod").unwrap();
    let rref = Ref::new("arca://billing/stripe-key").unwrap();
    assert_eq!(
        CfSecretsStore::sink_name(&env, &rref),
        "prod_billing_stripe_key"
    );
}

//! Protocol fakes on top of [`HttpMock`] — in-memory reference
//! implementations of the two remote surfaces arca talks to:
//!
//! - [`vault_wire`] — the `arca-vault-cf/1` wire protocol, the same
//!   semantics `arca-vault-cf/worker/index.js` implements against
//!   D1. Stores base64 ciphertext rows only — never plaintext.
//! - [`cf_secrets_store`] — the CF API v4 Secrets Store envelope
//!   (`{success, errors, result}`) that `arca-deploy-cf` pushes to.
//!   A real Secrets Store *is* a plaintext sink, so this fake
//!   records what it receives (fake values only, by rule).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::json;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::httpmock::{HttpMock, Request, Response};

/// One stored vault row (ciphertext is opaque to the store).
#[derive(Clone)]
struct VaultRow {
    ciphertext: String,
    version: u64,
    created_at: String,
    rotated_at: Option<String>,
}

fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

fn row_json(s: &VaultRow) -> String {
    json!({
        "version": s.version,
        "created_at": s.created_at,
        "rotated_at": s.rotated_at,
    })
    .to_string()
}

/// The `arca-vault-cf/1` wire protocol, in memory. Bearer `token`
/// required on every request; per-env keyspace; server-side
/// version/created_at/rotated_at bookkeeping (the client's rotate
/// is a `PUT` with `"rotate": true`, 404 on a missing ref).
pub fn vault_wire(token: &'static str) -> HttpMock {
    let store: Arc<Mutex<BTreeMap<(String, String), VaultRow>>> =
        Arc::new(Mutex::new(BTreeMap::new()));
    HttpMock::new(move |req: &Request| {
        if req.header("authorization") != Some(format!("Bearer {token}").as_str()) {
            return Response::json(401, r#"{"error":"unauthorized"}"#);
        }
        let path = req.path.split('?').next().unwrap_or("");
        let rest = match path.strip_prefix("/v1/") {
            Some(r) => r,
            None => return Response::json(404, r#"{"error":"no such route"}"#),
        };
        // /v1/{env} lists; /v1/{env}/{ref…} addresses one record.
        let (env, rref) = match rest.split_once('/') {
            Some((env, rref)) => (env.to_string(), Some(rref.to_string())),
            None => (rest.to_string(), None),
        };
        if env.is_empty() {
            return Response::json(404, r#"{"error":"no such route"}"#);
        }
        let mut store = store.lock().unwrap();
        match (req.method.as_str(), rref) {
            ("GET", None) => {
                let refs: Vec<String> = store
                    .keys()
                    .filter(|(e, _)| e == &env)
                    .map(|(_, r)| r.clone())
                    .collect();
                Response::json(200, json!({ "refs": refs }).to_string())
            }
            ("GET", Some(rref)) => match store.get(&(env, rref)) {
                Some(s) => Response::json(
                    200,
                    json!({
                        "ciphertext": s.ciphertext,
                        "version": s.version,
                        "created_at": s.created_at,
                        "rotated_at": s.rotated_at,
                    })
                    .to_string(),
                ),
                None => Response::json(404, r#"{"error":"not found"}"#),
            },
            ("PUT", Some(rref)) => {
                let body: serde_json::Value =
                    serde_json::from_str(&req.text()).unwrap_or(json!({}));
                let ciphertext = match body["ciphertext"].as_str() {
                    Some(c) if !c.is_empty() => c.to_string(),
                    _ => return Response::json(400, r#"{"error":"missing ciphertext"}"#),
                };
                let rotate = body["rotate"].as_bool().unwrap_or(false);
                let key = (env, rref);
                match (store.get(&key), rotate) {
                    (None, true) => Response::json(404, r#"{"error":"not found"}"#),
                    (None, false) => {
                        let s = VaultRow {
                            ciphertext,
                            version: 1,
                            created_at: now(),
                            rotated_at: None,
                        };
                        let out = row_json(&s);
                        store.insert(key, s);
                        Response::json(200, out)
                    }
                    (Some(old), rot) => {
                        let s = VaultRow {
                            ciphertext,
                            version: old.version + 1,
                            created_at: old.created_at.clone(),
                            rotated_at: if rot {
                                Some(now())
                            } else {
                                old.rotated_at.clone()
                            },
                        };
                        let out = row_json(&s);
                        store.insert(key, s);
                        Response::json(200, out)
                    }
                }
            }
            ("DELETE", Some(rref)) => {
                let removed = store.remove(&(env, rref)).is_some();
                Response::json(200, json!({ "removed": removed }).to_string())
            }
            _ => Response::json(405, r#"{"error":"method not allowed"}"#),
        }
    })
}

/// A fake CF API v4 Secrets Store. Routes under
/// `/accounts/{account}/secrets_store/stores/{store}/secrets`:
/// `GET ?search=` lists (name-substring), `POST` creates from an
/// array body, `PATCH /{id}` updates, `DELETE /{id}` removes —
/// all inside the `{success, errors, result}` envelope.
///
/// Returns the running mock plus the request log for assertions.
pub fn cf_secrets_store(
    account: &'static str,
    store_id: &'static str,
    token: &'static str,
) -> (HttpMock, Arc<Mutex<Vec<Request>>>) {
    let seen: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));
    let store: Arc<Mutex<BTreeMap<String, serde_json::Value>>> =
        Arc::new(Mutex::new(BTreeMap::new()));
    let next_id = Arc::new(Mutex::new(0u64));
    let mock = {
        let seen = seen.clone();
        HttpMock::new(move |req: &Request| {
            seen.lock().unwrap().push(req.clone());
            if req.header("authorization") != Some(format!("Bearer {token}").as_str()) {
                return Response::json(
                    403,
                    r#"{"success":false,"errors":[{"code":1000,"message":"bad token"}],"result":null}"#,
                );
            }
            let ok = |result: serde_json::Value| {
                Response::json(
                    200,
                    json!({"success":true,"errors":[],"result":result}).to_string(),
                )
            };
            let base = format!("/accounts/{account}/secrets_store/stores/{store_id}/secrets");
            let (path, query) = match req.path.split_once('?') {
                Some((p, q)) => (p, q),
                None => (req.path.as_str(), ""),
            };
            let suffix = path.strip_prefix(&base);
            let mut store = store.lock().unwrap();
            match (req.method.as_str(), suffix) {
                ("GET", Some("")) => {
                    let want = query
                        .split('&')
                        .find_map(|kv| kv.strip_prefix("search="))
                        .unwrap_or("");
                    let result: Vec<serde_json::Value> = store
                        .values()
                        .filter(|s| {
                            want.is_empty() || s["name"].as_str().unwrap_or("").contains(want)
                        })
                        .map(|s| json!({"id": s["id"], "name": s["name"], "status": "active"}))
                        .collect();
                    ok(json!(result))
                }
                ("POST", Some("")) => {
                    let body: serde_json::Value =
                        serde_json::from_str(&req.text()).unwrap_or(json!([]));
                    let mut results = Vec::new();
                    for item in body.as_array().cloned().unwrap_or_default() {
                        let name = item["name"].as_str().unwrap_or("").to_string();
                        let mut id = next_id.lock().unwrap();
                        *id += 1;
                        let id = format!("id{:04}", *id);
                        store.insert(
                            id.clone(),
                            json!({"id": id, "name": name, "value": item["value"]}),
                        );
                        results.push(json!({"id": id, "name": name}));
                    }
                    ok(json!(results))
                }
                ("PATCH", Some(suf)) | ("DELETE", Some(suf)) => {
                    let id = suf.trim_start_matches('/');
                    match store.get(&id.to_string()) {
                        None => Response::json(
                            404,
                            r#"{"success":false,"errors":[{"code":1007,"message":"not found"}],"result":null}"#,
                        ),
                        Some(_) if req.method == "DELETE" => {
                            store.remove(&id.to_string());
                            ok(json!({"id": id}))
                        }
                        Some(old) => {
                            let body: serde_json::Value =
                                serde_json::from_str(&req.text()).unwrap_or_default();
                            let mut new = old.clone();
                            new["value"] = body["value"].clone();
                            store.insert(id.to_string(), new.clone());
                            ok(json!({"id": id, "name": new["name"]}))
                        }
                    }
                }
                _ => Response::json(404, r#"{"success":false,"errors":[],"result":null}"#),
            }
        })
    };
    (mock, seen)
}

//! arca-vault-cf — the Cloudflare Worker + D1 vault impl.
//!
//! A [`Vault`] over HTTPS: a self-hosted Worker (source in
//! `worker/`) fronts a D1 table of opaque rows —
//! `(env, ref) → {ciphertext, version, created_at, rotated_at}`.
//! **The Worker never sees plaintext.** Each value is age-encrypted
//! client-side to the configured recipients *before* it leaves the
//! process; the store holds and returns base64 ciphertext only.
//! This is the same crypto boundary as `arca-vault-age` (ADR-0001)
//! — a remote store is just a different persistence medium — but
//! with *per-secret* ciphertext so rows map 1:1 to D1.
//!
//! ## Wire protocol (`arca-vault-cf/1`)
//!
//! Bearer auth on every request: `Authorization: Bearer <token>`,
//! the token read from the env var named by `token_env` in config —
//! never inline.
//!
//! | Verb | Path | Body → Response |
//! | --- | --- | --- |
//! | `GET` | `/v1/{env}` | `{"refs": ["a/b", …]}` sorted |
//! | `GET` | `/v1/{env}/{ref}` | `{"ciphertext": "<b64>", "version": n, "created_at": rfc3339, "rotated_at": rfc3339\|null}` |
//! | `PUT` | `/v1/{env}/{ref}` | `{"ciphertext": "<b64>", "rotate": bool}` → record metadata; `rotate:true` on a missing ref → 404 |
//! | `DELETE` | `/v1/{env}/{ref}` | `{"removed": bool}` |
//!
//! `{ref}` is the bare ref path (`billing/stripe`) — both sides
//! split on `/`. Errors are `{"error": "…"}` with a non-2xx status.
//! Bookkeeping is **server-side**: the Worker bumps `version`,
//! preserves `created_at`, and stamps `rotated_at` on rotate, so
//! concurrent writers can't race client-computed versions.
//!
//! HTTP uses `ureq` (blocking, no async runtime) per AGENTS.md.

use std::io::{Read, Write};
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use age::x25519;
use arca_core::{Env, Ref, Secret};
use arca_vault::{SecretRecord, Vault, VaultError};
use base64::Engine;
use serde::Deserialize;
use time::OffsetDateTime;
use ureq::Agent;

/// One row of the wire protocol — `ciphertext` present only on the
/// single-record GET.
#[derive(Debug, Deserialize)]
struct Row {
    #[serde(default)]
    ciphertext: Option<String>,
    version: u64,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    rotated_at: Option<OffsetDateTime>,
}

#[derive(Debug, Deserialize)]
struct ListBody {
    refs: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RmBody {
    removed: bool,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    error: Option<String>,
}

/// A [`Vault`] backed by the Worker + D1 service.
pub struct CfVault {
    agent: Agent,
    base: String,
    auth: String,
    identity: x25519::Identity,
    recipients: Vec<x25519::Recipient>,
}

impl CfVault {
    /// Build a vault from config values: `url` is the Worker base
    /// (no trailing slash needed), `token_env` names the env var
    /// holding the bearer token, `identity_path` the age keyfile
    /// used to decrypt, `extra_recipients` extra `age1…` keys
    /// encrypted to alongside the identity's public key.
    ///
    /// Construction does **no** network I/O — the first verb does.
    pub fn new(
        url: &str,
        token_env: &str,
        identity_path: &Path,
        extra_recipients: &[String],
    ) -> Result<Self, VaultError> {
        let token = std::env::var(token_env).map_err(|_| {
            VaultError::Config(format!(
                "env var {token_env} is unset — cf vault tokens come via env, never config"
            ))
        })?;
        if token.is_empty() {
            return Err(VaultError::Config(format!(
                "env var {token_env} is empty — cf vault tokens come via env, never config"
            )));
        }
        let identity = read_identity(identity_path)?;
        Self::with_identity(url, &token, identity, extra_recipients)
    }

    /// Build from an already-parsed identity + token — the test and
    /// embedding path; `new` is the config path.
    pub fn with_identity(
        url: &str,
        token: &str,
        identity: x25519::Identity,
        extra_recipients: &[String],
    ) -> Result<Self, VaultError> {
        let mut recipients = vec![identity.to_public()];
        for r in extra_recipients {
            recipients.push(
                x25519::Recipient::from_str(r)
                    .map_err(|e| VaultError::Config(format!("invalid recipient {r:?}: {e}")))?,
            );
        }
        let agent: Agent = Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .build()
            .into();
        Ok(CfVault {
            agent,
            base: url.trim_end_matches('/').to_string(),
            auth: format!("Bearer {token}"),
            identity,
            recipients,
        })
    }

    /// `{base}/v1/{env}/{ref}` — ref segments are URL-safe by the
    /// `Ref` grammar (`[A-Za-z0-9._-]`), no escaping needed.
    fn item_url(&self, env: &Env, rref: &Ref) -> String {
        format!("{}/v1/{}/{}", self.base, env.as_str(), rref.path())
    }

    /// One HTTP round-trip. `http_status_as_error(false)` so error
    /// bodies are readable; transport failure is `VaultError::Io`.
    /// (ureq is typestate: GET/DELETE and body-carrying verbs are
    /// different builder types, so each arm is self-contained.)
    fn request(
        &self,
        method: &str,
        url: &str,
        body: Option<String>,
    ) -> Result<(u16, String), VaultError> {
        let io = |e: ureq::Error| VaultError::Io(format!("{method} {url}: {e}"));
        let has_body = body.is_some();
        let mut res = match (method, body) {
            ("GET", None) => self
                .agent
                .get(url)
                .header("Authorization", &self.auth)
                .config()
                .http_status_as_error(false)
                .build()
                .call()
                .map_err(io),
            ("DELETE", None) => self
                .agent
                .delete(url)
                .header("Authorization", &self.auth)
                .config()
                .http_status_as_error(false)
                .build()
                .call()
                .map_err(io),
            ("PUT", Some(b)) => self
                .agent
                .put(url)
                .header("Authorization", &self.auth)
                .config()
                .http_status_as_error(false)
                .build()
                .content_type("application/json")
                .send(b)
                .map_err(io),
            _ => {
                return Err(VaultError::Backend(format!(
                    "bad request shape {method} body={has_body}"
                )))
            }
        }?;
        let status = res.status().as_u16();
        let text = res
            .body_mut()
            .read_to_string()
            .map_err(|e| VaultError::Io(format!("{method} {url}: read body: {e}")))?;
        Ok((status, text))
    }

    /// Extract `{"error": "…"}` from a non-2xx body.
    fn error_msg(status: u16, body: &str) -> VaultError {
        if status == 401 || status == 403 {
            return VaultError::Backend(format!(
                "HTTP {status} — unauthorized; check the token env var"
            ));
        }
        let msg = serde_json::from_str::<ErrorBody>(body)
            .ok()
            .and_then(|e| e.error)
            .unwrap_or_else(|| body.chars().take(120).collect());
        VaultError::Backend(format!("HTTP {status}: {msg}"))
    }

    fn parse_row(body: &str) -> Result<Row, VaultError> {
        serde_json::from_str(body)
            .map_err(|e| VaultError::Backend(format!("worker returned bad row json: {e}")))
    }
}

impl Vault for CfVault {
    fn get(&self, env: &Env, rref: &Ref) -> Result<Option<SecretRecord>, VaultError> {
        let (status, body) = self.request("GET", &self.item_url(env, rref), None)?;
        match status {
            200 => {
                let row = Self::parse_row(&body)?;
                let b64 = row
                    .ciphertext
                    .ok_or_else(|| VaultError::Backend("worker row missing ciphertext".into()))?;
                let blob = base64::engine::general_purpose::STANDARD
                    .decode(&b64)
                    .map_err(|e| VaultError::Backend(format!("bad b64 ciphertext: {e}")))?;
                let plain = open(&self.identity, &blob)?;
                Ok(Some(SecretRecord {
                    value: Secret::new(plain),
                    version: row.version,
                    created_at: row.created_at,
                    rotated_at: row.rotated_at,
                }))
            }
            404 => Ok(None),
            other => Err(Self::error_msg(other, &body)),
        }
    }

    fn put(&self, env: &Env, rref: &Ref, value: Secret) -> Result<SecretRecord, VaultError> {
        self.write(env, rref, value, false)
    }

    fn rm(&self, env: &Env, rref: &Ref) -> Result<bool, VaultError> {
        let (status, body) = self.request("DELETE", &self.item_url(env, rref), None)?;
        match status {
            200 => {
                let out: RmBody = serde_json::from_str(&body).map_err(|e| {
                    VaultError::Backend(format!("worker returned bad rm json: {e}"))
                })?;
                Ok(out.removed)
            }
            other => Err(Self::error_msg(other, &body)),
        }
    }

    fn list(&self, env: &Env) -> Result<Vec<Ref>, VaultError> {
        let url = format!("{}/v1/{}", self.base, env.as_str());
        let (status, body) = self.request("GET", &url, None)?;
        match status {
            200 => {
                let out: ListBody = serde_json::from_str(&body).map_err(|e| {
                    VaultError::Backend(format!("worker returned bad list json: {e}"))
                })?;
                let mut refs = Vec::new();
                for path in out.refs {
                    refs.push(
                        Ref::from_path(&path)
                            .map_err(|e| VaultError::Backend(format!("stored key {e}")))?,
                    );
                }
                refs.sort();
                Ok(refs)
            }
            other => Err(Self::error_msg(other, &body)),
        }
    }

    fn rotate(&self, env: &Env, rref: &Ref, value: Secret) -> Result<SecretRecord, VaultError> {
        self.write(env, rref, value, true)
    }
}

impl CfVault {
    /// PUT shared by `put` (`rotate:false`) and `rotate`
    /// (`rotate:true`); the server does the version bookkeeping.
    fn write(
        &self,
        env: &Env,
        rref: &Ref,
        value: Secret,
        rotate: bool,
    ) -> Result<SecretRecord, VaultError> {
        let blob = seal(&self.recipients, value.expose());
        let b64 = base64::engine::general_purpose::STANDARD.encode(blob);
        let body = serde_json::json!({ "ciphertext": b64, "rotate": rotate }).to_string();
        let (status, text) = self.request("PUT", &self.item_url(env, rref), Some(body))?;
        match status {
            200 => {
                let row = Self::parse_row(&text)?;
                Ok(SecretRecord {
                    value,
                    version: row.version,
                    created_at: row.created_at,
                    rotated_at: row.rotated_at,
                })
            }
            404 => Err(VaultError::NotFound {
                rref: rref.path(),
                env: env.as_str().to_string(),
            }),
            other => Err(Self::error_msg(other, &text)),
        }
    }
}

/// Age-encrypt `plaintext` to `recipients` — per-secret ciphertext,
/// unlike `arca-vault-age`'s whole-doc encryption (deliberate small
/// duplication rather than a shared crypto crate). In-memory
/// encryption cannot realistically fail — callers always pass ≥1
/// recipient and a `Vec` sink never errors — so the `expect`s
/// document invariants rather than guard reachable paths.
#[allow(clippy::unwrap_used)]
fn seal(recipients: &[x25519::Recipient], plaintext: &str) -> Vec<u8> {
    let encryptor =
        age::Encryptor::with_recipients(recipients.iter().map(|r| r as &dyn age::Recipient))
            .expect("seal is only called with ≥1 recipient");
    let mut cipher = Vec::new();
    {
        let mut w = encryptor.wrap_output(&mut cipher).expect("in-memory wrap");
        w.write_all(plaintext.as_bytes()).expect("in-memory write");
        w.finish().expect("in-memory finish");
    }
    cipher
}

/// Decrypt one per-secret ciphertext blob with `identity`.
fn open(identity: &x25519::Identity, cipher: &[u8]) -> Result<String, VaultError> {
    let decryptor = age::Decryptor::new_buffered(cipher)
        .map_err(|e| VaultError::Crypto(format!("read ciphertext: {e}")))?;
    if decryptor.is_scrypt() {
        return Err(VaultError::Crypto(
            "passphrase-encrypted ciphertext; recipient key required".into(),
        ));
    }
    let mut reader = decryptor
        .decrypt(std::iter::once(identity as &dyn age::Identity))
        .map_err(|e| VaultError::Crypto(format!("decrypt: {e}")))?;
    let mut buf = String::new();
    reader.read_to_string(&mut buf)?;
    Ok(buf)
}

/// Read + validate an age identity keyfile: first non-empty,
/// non-`#`-comment line must be an `AGE-SECRET-KEY-…` string (same
/// format as `arca-vault-age` — deliberately duplicated, ~15 lines).
fn read_identity(path: &Path) -> Result<x25519::Identity, VaultError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| VaultError::Config(format!("read identity {}: {e}", path.display())))?;
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .ok_or_else(|| VaultError::Config(format!("identity {} holds no key", path.display())))?;
    x25519::Identity::from_str(line)
        .map_err(|e| VaultError::Crypto(format!("identity {}: {e}", path.display())))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip_hides_value() {
        let id = x25519::Identity::generate();
        let blob = seal(&[id.to_public()], "fake-roundtrip");
        assert!(!String::from_utf8_lossy(&blob).contains("fake-roundtrip"));
        assert_eq!(open(&id, &blob).unwrap(), "fake-roundtrip");
        // a different identity can't open it
        let other = x25519::Identity::generate();
        assert!(open(&other, &blob).is_err());
    }

    #[test]
    fn open_rejects_scrypt_and_tampered_blobs() {
        let id = x25519::Identity::generate();
        // passphrase-encrypted → we hold no passphrase
        let scrypt = age::Encryptor::with_user_passphrase(age::secrecy::SecretString::from(
            "fake-pw".to_string(),
        ));
        let mut blob = Vec::new();
        {
            let mut w = scrypt.wrap_output(&mut blob).unwrap();
            w.write_all(b"x").unwrap();
            w.finish().unwrap();
        }
        let e = open(&id, &blob).unwrap_err();
        assert!(e.to_string().contains("passphrase"), "{e}");
        // not an age stream at all
        assert!(open(&id, b"garbage").is_err());
        // valid envelope but corrupted/truncated body → read fails
        let good = seal(&[id.to_public()], "fake-body");
        let cut = &good[..good.len() / 2];
        assert!(open(&id, cut).is_err());
    }

    #[test]
    fn request_rejects_unknown_method_body_shapes() {
        // `request` is the internal HTTP seam — a shape no caller
        // produces must be a clean error, never a panic
        let id = x25519::Identity::generate();
        let v = CfVault::with_identity("http://127.0.0.1:1", "t", id, &[]).unwrap();
        assert!(v.request("PATCH", "http://x", None).is_err());
        assert!(v.request("GET", "http://x", Some("{}".into())).is_err());
        assert!(v.request("PUT", "http://x", None).is_err());
    }

    #[test]
    fn new_requires_token_env() {
        let dir = tempfile::tempdir().unwrap();
        let id_path = dir.path().join("identity.txt");
        let id = x25519::Identity::generate();
        use age::secrecy::ExposeSecret;
        std::fs::write(&id_path, format!("{}\n", id.to_string().expose_secret())).unwrap();
        temp_env::with_var_unset("ARCA_CF_TOKEN_TEST", || {
            assert!(CfVault::new("http://x", "ARCA_CF_TOKEN_TEST", &id_path, &[]).is_err());
        });
        // set but empty → still an error
        temp_env::with_var("ARCA_CF_TOKEN_TEST", Some(""), || {
            let e = CfVault::new("http://x", "ARCA_CF_TOKEN_TEST", &id_path, &[])
                .err()
                .unwrap();
            assert!(e.to_string().contains("empty"), "{e}");
        });
    }

    #[test]
    fn new_and_with_identity_validate_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let id_path = dir.path().join("identity.txt");
        let id = x25519::Identity::generate();
        use age::secrecy::ExposeSecret;
        std::fs::write(&id_path, format!("{}\n", id.to_string().expose_secret())).unwrap();
        // a broken identity file fails before any env-var concern
        let junk = dir.path().join("junk.txt");
        std::fs::write(&junk, "not-a-key\n").unwrap();
        temp_env::with_var("ARCA_CF_TOKEN_TEST", Some("fake"), || {
            assert!(CfVault::new("http://x", "ARCA_CF_TOKEN_TEST", &junk, &[]).is_err());
        });
        // invalid extra recipient strings are config errors
        let e = CfVault::with_identity("http://x", "t", id.clone(), &["bad!!".into()])
            .err()
            .unwrap();
        assert!(e.to_string().contains("invalid recipient"), "{e}");
    }

    #[test]
    fn error_msg_prefers_auth_hint_then_body() {
        // 401/403 get the token hint regardless of body
        for status in [401, 403] {
            let e = CfVault::error_msg(status, "{}");
            assert!(e.to_string().contains("unauthorized"), "{e}");
        }
        // a json error body is surfaced
        let e = CfVault::error_msg(500, r#"{"error":"d1 is down"}"#);
        assert!(e.to_string().contains("d1 is down"), "{e}");
        // non-json bodies degrade to a truncated dump
        let e = CfVault::error_msg(502, "not json at all");
        assert!(e.to_string().contains("not json at all"), "{e}");
    }

    #[test]
    fn read_identity_rejects_missing_and_garbage_files() {
        let dir = tempfile::tempdir().unwrap();
        // missing file → config error
        assert!(read_identity(&dir.path().join("nope.txt")).is_err());
        // comments only → "holds no key"
        let p = dir.path().join("comments.txt");
        std::fs::write(&p, "# nothing\n\n").unwrap();
        assert!(read_identity(&p).is_err());
        // a non-key line → crypto error
        let p = dir.path().join("bad.txt");
        std::fs::write(&p, "AGE-SECRET-KEY-NOTREAL\n").unwrap();
        assert!(read_identity(&p).is_err());
    }
}

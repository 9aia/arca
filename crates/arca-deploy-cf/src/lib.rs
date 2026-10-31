//! arca-deploy-cf — push secrets to a Cloudflare **account Secrets
//! Store** via the CF API v4 (`arca-deploy` impl).
//!
//! The Secrets Store is the canonical write-only sink: values go in
//! and can never be fetched back — Workers bind to them directly.
//! `arca deploy` pushes the *plaintext* value (decrypted at the ops
//! edge): the age-ciphertext boundary covers arca's own vaults; an
//! external store that must serve values to runtimes is outside it.
//!
//! ## API surface used
//!
//! `GET`/`POST`/`PATCH`/`DELETE` under
//! `{base}/accounts/{account_id}/secrets_store/stores/{store_id}/secrets`
//! — CF envelope `{success, errors, result}` on every response.
//! Upsert = `GET ?search={name}` → `PATCH /{id}` when an exact-name
//! row exists, else `POST` a create body.
//!
//! The API token arrives via the env var named by `token_env`
//! (default `CLOUDFLARE_API_TOKEN`) — never inline in config.
//! Blocking `ureq`, no async runtime (AGENTS.md).

use std::time::Duration;

use arca_core::{Env, Ref, Secret};
use arca_deploy::{Deploy, DeployError, PushReport};
use ureq::Agent;

/// The default CF API base.
pub const DEFAULT_BASE: &str = "https://api.cloudflare.com/client/v4";

/// The default token env var when config doesn't name one.
pub const DEFAULT_TOKEN_ENV: &str = "CLOUDFLARE_API_TOKEN";

/// A [`Deploy`] target backed by a CF account Secrets Store.
pub struct CfSecretsStore {
    name: String,
    agent: Agent,
    base: String,
    auth: String,
    account_id: String,
    store_id: String,
}

impl CfSecretsStore {
    /// Build a target from config values. `token_env` names the env
    /// var holding a `Secrets Store Write` API token; `base_url`
    /// overrides the API base (tests, gateways).
    ///
    /// Construction does **no** network I/O — `push`/`remove` do.
    pub fn new(
        name: &str,
        account_id: &str,
        store_id: &str,
        token_env: &str,
        base_url: Option<&str>,
    ) -> Result<Self, DeployError> {
        let token = std::env::var(token_env).map_err(|_| {
            DeployError::Config(format!(
                "env var {token_env} is unset — deploy tokens come via env, never config"
            ))
        })?;
        if token.is_empty() {
            return Err(DeployError::Config(format!(
                "env var {token_env} is empty — deploy tokens come via env, never config"
            )));
        }
        Ok(Self::with_token(
            name, account_id, store_id, &token, base_url,
        ))
    }

    /// Build with an explicit token — the test/embedding path.
    pub fn with_token(
        name: &str,
        account_id: &str,
        store_id: &str,
        token: &str,
        base_url: Option<&str>,
    ) -> Self {
        let agent: Agent = Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .build()
            .into();
        CfSecretsStore {
            name: name.to_string(),
            agent,
            base: base_url
                .unwrap_or(DEFAULT_BASE)
                .trim_end_matches('/')
                .to_string(),
            auth: format!("Bearer {token}"),
            account_id: account_id.to_string(),
            store_id: store_id.to_string(),
        }
    }

    /// The sink-side name for `(env, rref)`:
    /// `{env}_{path}`, non-`[A-Za-z0-9_]` chars → `_`. Stable and
    /// collision-light; e.g. `(prod, billing/stripe)` →
    /// `prod_billing_stripe`.
    pub fn sink_name(env: &Env, rref: &Ref) -> String {
        let mut n = format!("{}_{}", env.as_str(), rref.path());
        n.make_ascii_lowercase();
        n.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }

    fn secrets_url(&self) -> String {
        format!(
            "{}/accounts/{}/secrets_store/stores/{}/secrets",
            self.base, self.account_id, self.store_id
        )
    }

    /// One JSON round-trip; returns `(status, body)`. (ureq is
    /// typestate: GET/DELETE and body-carrying verbs are different
    /// builder types, so each arm is self-contained.)
    fn request(
        &self,
        method: &str,
        url: &str,
        query: Option<(&str, &str)>,
        body: Option<serde_json::Value>,
    ) -> Result<(u16, String), DeployError> {
        let io = |e: ureq::Error| DeployError::Io(format!("{method} {url}: {e}"));
        let has_body = body.is_some();
        let mut res = match (method, body) {
            ("GET", None) => {
                let rb = self.agent.get(url).header("Authorization", &self.auth);
                let rb = match query {
                    Some((k, v)) => rb.query(k, v),
                    None => rb,
                };
                rb.config()
                    .http_status_as_error(false)
                    .build()
                    .call()
                    .map_err(io)
            }
            ("DELETE", None) => self
                .agent
                .delete(url)
                .header("Authorization", &self.auth)
                .config()
                .http_status_as_error(false)
                .build()
                .call()
                .map_err(io),
            ("POST", Some(b)) => self
                .agent
                .post(url)
                .header("Authorization", &self.auth)
                .config()
                .http_status_as_error(false)
                .build()
                .content_type("application/json")
                .send(b.to_string())
                .map_err(io),
            ("PATCH", Some(b)) => self
                .agent
                .patch(url)
                .header("Authorization", &self.auth)
                .config()
                .http_status_as_error(false)
                .build()
                .content_type("application/json")
                .send(b.to_string())
                .map_err(io),
            _ => {
                return Err(DeployError::Backend(format!(
                    "bad request shape {method} body={has_body}"
                )))
            }
        }?;
        let status = res.status().as_u16();
        let text = res
            .body_mut()
            .read_to_string()
            .map_err(|e| DeployError::Io(format!("{method} {url}: read body: {e}")))?;
        Ok((status, text))
    }

    /// Parse the CF envelope; on failure surfaces the errors array.
    fn envelope(status: u16, body: &str) -> Result<serde_json::Value, DeployError> {
        if status == 401 || status == 403 {
            return Err(DeployError::Unauthorized(format!(
                "HTTP {status} — check the token env var and Secrets Store permissions"
            )));
        }
        let v: serde_json::Value = serde_json::from_str(body).map_err(|e| {
            DeployError::Backend(format!("CF returned non-json (HTTP {status}): {e}"))
        })?;
        if v["success"].as_bool() != Some(true) {
            let errs: Vec<String> = v["errors"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|e| e["message"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            return Err(DeployError::Backend(format!(
                "CF API HTTP {status}: {}",
                if errs.is_empty() {
                    body.chars().take(200).collect()
                } else {
                    errs.join("; ")
                }
            )));
        }
        Ok(v)
    }

    /// The store-internal id of the secret named `name`, if any.
    fn find_id(&self, name: &str) -> Result<Option<String>, DeployError> {
        let url = self.secrets_url();
        let (status, body) = self.request("GET", &url, Some(("search", name)), None)?;
        let v = Self::envelope(status, &body)?;
        let found = v["result"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|s| s["name"].as_str() == Some(name))
            .and_then(|s| s["id"].as_str().map(str::to_string));
        Ok(found)
    }
}

impl Deploy for CfSecretsStore {
    fn name(&self) -> &str {
        &self.name
    }

    fn push(&self, env: &Env, rref: &Ref, value: &Secret) -> Result<PushReport, DeployError> {
        let name = Self::sink_name(env, rref);
        match self.find_id(&name)? {
            Some(id) => {
                let url = format!("{}/{}", self.secrets_url(), id);
                let body = serde_json::json!({
                    "name": name,
                    "value": value.expose(),
                });
                let (status, text) = self.request("PATCH", &url, None, Some(body))?;
                Self::envelope(status, &text)?;
                Ok(PushReport {
                    name,
                    created: false,
                })
            }
            None => {
                let body = serde_json::json!([{
                    "name": name,
                    "value": value.expose(),
                    "comment": "pushed by arca deploy",
                    "scopes": ["workers"],
                }]);
                let (status, text) = self.request("POST", &self.secrets_url(), None, Some(body))?;
                Self::envelope(status, &text)?;
                Ok(PushReport {
                    name,
                    created: true,
                })
            }
        }
    }

    fn remove(&self, env: &Env, rref: &Ref) -> Result<bool, DeployError> {
        let name = Self::sink_name(env, rref);
        match self.find_id(&name)? {
            Some(id) => {
                let url = format!("{}/{}", self.secrets_url(), id);
                let (status, text) = self.request("DELETE", &url, None, None)?;
                Self::envelope(status, &text)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

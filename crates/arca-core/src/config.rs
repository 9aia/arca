//! The `arca.toml`-shaped config model every `arca-config` source
//! loads. Five tables:
//!
//! ```toml
//! [vaults.local]                 # named vault backends ("local" is
//! kind = "age"                   #   the built-in age default)
//!
//! [envs]                         # env → vault routing; unlisted
//! dev = { vault = "local" }      #   envs route to "local"
//!
//! [secrets]                      # the declared surface run may
//! STRIPE_KEY = "arca://billing/stripe"   #   inject: VAR → ref
//!
//! [deploy.prod]                  # write-only push targets (sinks)
//! kind = "cf-secrets-store"
//!
//! [guardrails]                   # advisory op table — accident
//! default = "allow"              #   prevention, not policy
//! allow = ["run", "list"]
//! deny = ["put"]
//! ```
//!
//! Unknown keys are rejected so a typo fails loud instead of
//! silently doing nothing.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Env, Op, Ref};

/// The name of the built-in local (age) vault.
pub const LOCAL_VAULT: &str = "local";

/// The `arca.toml` config value.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Config {
    /// Named vault backends. `local` always exists implicitly (age,
    /// default paths); an entry only overrides it or adds more.
    #[serde(default)]
    pub vaults: BTreeMap<String, VaultConfig>,
    /// Env → vault routing. Envs not listed route to `local`;
    /// `dev`/`staging`/`prod` always exist.
    #[serde(default)]
    pub envs: BTreeMap<Env, EnvRoute>,
    /// The declared secret surface: env-var name → `arca://` ref.
    /// Bounds what `run`/`env` may inject — least privilege, no
    /// ambient vault dump.
    #[serde(default)]
    pub secrets: BTreeMap<String, Ref>,
    /// Named deploy targets — write-only sinks (`arca deploy`
    /// pushes a stored value in; nothing ever reads one back).
    #[serde(default)]
    pub deploy: BTreeMap<String, DeployConfig>,
    /// Advisory op guardrails — accident prevention for standalone
    /// use, honestly *not* a policy boundary (a locally claimed
    /// actor is spoofable by construction).
    #[serde(default)]
    pub guardrails: Option<Guardrails>,
}

impl Config {
    /// The vault name an env routes to — `[envs] <env>` if present,
    /// else `local`.
    pub fn vault_for(&self, env: &Env) -> &str {
        self.envs.get(env).map(|r| r.vault()).unwrap_or(LOCAL_VAULT)
    }

    /// Does `name` denote a known env? Well-known envs plus every
    /// `[envs]` key — what makes `arca://<name>/…` an env pin.
    pub fn is_known_env(&self, name: &str) -> bool {
        Env::WELL_KNOWN.contains(&name) || self.envs.keys().any(|e| e.as_str() == name)
    }

    /// The [`VaultConfig`] for a vault name, or `None` when the name
    /// isn't configured (`local` included — it has implicit
    /// defaults).
    pub fn vault_config(&self, name: &str) -> Option<&VaultConfig> {
        self.vaults.get(name)
    }

    /// The [`DeployConfig`] for a deploy target name.
    pub fn deploy_config(&self, name: &str) -> Option<&DeployConfig> {
        self.deploy.get(name)
    }
}

/// One `[vaults.<name>]` entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct VaultConfig {
    /// Backend kind — `"age"` (built-in, local file) or `"cf"`
    /// (self-hosted Cloudflare Worker + D1, `arca-vault-cf`).
    #[serde(default = "default_kind")]
    pub kind: String,
    /// Vault file path override (age impl; `~` is *not* expanded —
    /// write the path in full).
    pub path: Option<String>,
    /// Age identity file override (`ARCA_IDENTITY_FILE` wins over
    /// this; see `arca-ops`). `cf` vaults decrypt with the same
    /// keyfile — all vault impls store age ciphertext (ADR-0001).
    pub identity: Option<String>,
    /// Extra age recipients to encrypt to, beyond the identity's
    /// public key (`age1…` strings).
    #[serde(default)]
    pub recipients: Vec<String>,
    /// Worker base URL (`cf` impl), e.g.
    /// `https://arca-vault.you.workers.dev`.
    pub url: Option<String>,
    /// Name of the env var holding the bearer token (`cf` impl) —
    /// the token *name* lives in config, the token *value* only in
    /// the process env.
    pub token_env: Option<String>,
}

fn default_kind() -> String {
    "age".into()
}

/// One `[deploy.<name>]` entry — a write-only push target.
///
/// Deploy targets are **sinks**, not sources: `arca deploy` pushes a
/// vault-stored value in and nothing is ever read back (CF Secrets
/// Store values are write-only by design). Credentials always arrive
/// via the env var named by `token_env`, never inline.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DeployConfig {
    /// Target kind — `"cf-secrets-store"` (Cloudflare account
    /// Secrets Store, `arca-deploy-cf`).
    #[serde(default = "default_deploy_kind")]
    pub kind: String,
    /// Cloudflare account id (`cf-secrets-store`).
    pub account_id: Option<String>,
    /// Secrets Store id (`cf-secrets-store`).
    pub store_id: Option<String>,
    /// Name of the env var holding the API token — never the token
    /// itself (`cf-secrets-store` defaults to
    /// `CLOUDFLARE_API_TOKEN`).
    pub token_env: Option<String>,
    /// API base URL override — for tests, gateways, and
    /// self-hosted proxies. Defaults to
    /// `https://api.cloudflare.com/client/v4`.
    pub base_url: Option<String>,
}

fn default_deploy_kind() -> String {
    "cf-secrets-store".into()
}

/// How an env routes to its vault — `dev = "local"` shorthand or
/// `dev = { vault = "local" }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum EnvRoute {
    /// Bare vault name.
    Name(String),
    /// Table form — room for later per-env options.
    Table(EnvRouteTable),
}

/// The `{ vault = "…" }` table form of [`EnvRoute`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EnvRouteTable {
    /// Vault name in `[vaults]` (or the implicit `local`).
    pub vault: String,
}

impl EnvRoute {
    /// The vault name either form routes to.
    pub fn vault(&self) -> &str {
        match self {
            EnvRoute::Name(v) => v,
            EnvRoute::Table(t) => &t.vault,
        }
    }
}

impl From<&str> for EnvRoute {
    fn from(vault: &str) -> Self {
        EnvRoute::Name(vault.to_string())
    }
}

impl From<String> for EnvRoute {
    fn from(vault: String) -> Self {
        EnvRoute::Name(vault)
    }
}

/// What an unlisted op falls back to in [`Guardrails`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum GuardDefault {
    /// Unlisted ops pass.
    #[default]
    Allow,
    /// Unlisted ops are refused.
    Deny,
}

/// Advisory op table — `[guardrails]` in `arca.toml`.
///
/// Guardrails, not policy: they catch accidents ("agents get
/// `run`/`list`, never `get`/`put`") for standalone use. Real
/// enforcement is peregrin's job — `ARCA_ACTOR` is a self-claimed
/// string, spoofable by construction.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Guardrails {
    /// Fallback for ops in neither list.
    #[serde(default)]
    pub default: GuardDefault,
    /// Ops always allowed.
    #[serde(default)]
    pub allow: Vec<Op>,
    /// Ops always refused — deny wins over allow.
    #[serde(default)]
    pub deny: Vec<Op>,
}

impl Guardrails {
    /// Advisory check: deny list wins, then allow list, then
    /// `default`.
    pub fn allows(&self, op: Op) -> bool {
        if self.deny.contains(&op) {
            return false;
        }
        if self.allow.contains(&op) {
            return true;
        }
        matches!(self.default, GuardDefault::Allow)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_shape() {
        let text = r#"
            [vaults.local]
            kind = "age"
            recipients = ["age1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqygh9"]

            [envs]
            dev = { vault = "local" }
            prod = "local"

            [secrets]
            STRIPE_KEY = "arca://billing/stripe"

            [vaults.cloud]
            kind = "cf"
            url = "https://arca-vault.example.workers.dev"
            token_env = "ARCA_CF_TOKEN"

            [deploy.prod]
            kind = "cf-secrets-store"
            account_id = "0000000000000000000000000000dead"
            store_id = "0000000000000000000000000000beef"

            [guardrails]
            default = "deny"
            allow = ["run", "list"]
        "#;
        let cfg: Config = toml::from_str(text).unwrap();
        assert_eq!(cfg.vaults["local"].kind, "age");
        assert_eq!(cfg.vault_for(&Env::dev()), "local");
        assert_eq!(cfg.vault_for(&Env::new("ci").unwrap()), "local");
        assert!(cfg.is_known_env("prod"));
        assert!(cfg.is_known_env("staging")); // well-known, unconfigured
        assert!(!cfg.is_known_env("ci"));
        assert_eq!(
            cfg.secrets["STRIPE_KEY"].to_string(),
            "arca://billing/stripe"
        );
        assert_eq!(cfg.vaults["cloud"].kind, "cf");
        assert_eq!(
            cfg.vaults["cloud"].token_env.as_deref(),
            Some("ARCA_CF_TOKEN")
        );
        let d = &cfg.deploy["prod"];
        assert_eq!(d.kind, "cf-secrets-store");
        assert!(d.account_id.is_some() && d.token_env.is_none());
        let g = cfg.guardrails.unwrap();
        assert_eq!(g.default, GuardDefault::Deny);
        assert!(g.allows(Op::Run));
        assert!(g.allows(Op::List));
        assert!(!g.allows(Op::Get));
    }

    #[test]
    fn empty_is_valid_and_routes_local() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.vault_for(&Env::dev()), "local");
        assert!(cfg.is_known_env("prod"));
        assert!(cfg.guardrails.is_none());
    }

    #[test]
    fn unknown_keys_fail_loud() {
        assert!(toml::from_str::<Config>("project_dir = \"project\"").is_err());
        assert!(toml::from_str::<Config>("[envs]\nPROD = \"local\"").is_err());
        assert!(toml::from_str::<Config>("[secrets]\nX = \"not-a-ref\"").is_err());
    }

    #[test]
    fn guardrails_deny_wins() {
        let g = Guardrails {
            default: GuardDefault::Allow,
            allow: vec![Op::Get],
            deny: vec![Op::Get],
        };
        assert!(!g.allows(Op::Get));
        assert!(g.allows(Op::Put));
    }
}

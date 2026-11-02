//! arca-ops — the I/O shell every front-end calls.
//!
//! `arca-core` is pure domain; this crate is everything that touches
//! the outside world: config loading + env/vault resolution
//! (`--env`/`ARCA_ENV` → `[envs]` routing → a concrete
//! [`arca_vault::Vault`]), the env-pin override on refs, the verb
//! operations the CLI calls, and the audit ndjson emission on
//! stderr. `arca` (CLI) is a thin clap shell over this; `arca-tui`
//! calls the same functions.

pub mod audit;
mod error;

pub use audit::{AuditEvent, Outcome};
pub use error::OpsError;

use std::collections::BTreeMap;
use std::path::PathBuf;

use arca_config::ConfigSource;
use arca_config_toml::TomlSource;
use arca_core::{is_valid_env_var, Actor, Config, Env, Op, Ref, Secret};
use arca_deploy::Deploy;
use arca_deploy_cf::CfSecretsStore;
use arca_vault::{SecretRecord, Vault};
use arca_vault_age::AgeVault;
use arca_vault_cf::CfVault;

/// Knobs for [`Ops::load`] — the global CLI flags, resolved.
#[derive(Debug, Default)]
pub struct LoadOpts {
    /// `--config` path (explicit → errors propagate).
    pub config_path: Option<String>,
    /// `--env` name (else `ARCA_ENV`, else `dev`).
    pub env: Option<String>,
    /// `--actor` attribution (else `ARCA_ACTOR`, `$USER`, `anon`).
    pub actor: Option<String>,
}

/// What `init` reports back.
#[derive(Debug)]
pub struct InitInfo {
    /// The vault file that backs the selected env.
    pub vault_path: PathBuf,
    /// The age identity keyfile.
    pub identity_path: PathBuf,
    /// `true` when a fresh identity was generated.
    pub identity_created: bool,
    /// The identity's public recipient (`age1…`).
    pub recipient: String,
}

/// What `check` reports about one vault — the backend is
/// decryptable/readable, or why not.
#[derive(Debug)]
pub struct VaultCheck {
    /// Vault name from `[vaults]` (or the implicit `local`).
    pub name: String,
    /// Backend kind (`age`; anything else is a problem).
    pub kind: String,
    /// Vault file path for file-backed impls (`None` when the kind
    /// is unknown or unset).
    pub path: Option<PathBuf>,
    /// The vault opened (identity present, ciphertext decrypts).
    pub ok: bool,
    /// Why it didn't (`None` when `ok`).
    pub error: Option<String>,
}

/// What `deploy` reports back.
#[derive(Debug)]
pub struct DeployReport {
    /// The `[deploy.<name>]` target that received the value.
    pub target: String,
    /// The env the ref resolved under.
    pub env: Env,
    /// The resolved ref (env pin stripped).
    pub rref: Ref,
    /// The sink-side name the value was written under.
    pub secret_name: String,
    /// `true` when the sink created a new entry (else updated).
    pub created: bool,
}

/// What `check` reports about one declared `[secrets]` entry.
#[derive(Debug)]
pub struct SecretCheck {
    /// The env-var name.
    pub var: String,
    /// The declared `arca://` ref, env pin stripped if present.
    pub rref: String,
    /// The env it resolves under (pin-aware).
    pub env: String,
    /// A record exists at that `(env, ref)`.
    pub resolves: bool,
}

/// What `check` reports back.
#[derive(Debug)]
pub struct CheckReport {
    /// Where config was loaded from (`None` = defaults).
    pub config_path: Option<PathBuf>,
    /// The selected env and the vault it routes to.
    pub env: Env,
    /// Vault name `env` routes to.
    pub vault: String,
    /// Every configured or routed vault, with readability status.
    pub vaults: Vec<VaultCheck>,
    /// Every declared `[secrets]` entry, with resolution status.
    pub secrets: Vec<SecretCheck>,
    /// The configured guardrails table, if any.
    pub guardrails: Option<arca_core::Guardrails>,
    /// Findings — non-empty means "problems to fix" (exit 1).
    pub problems: Vec<String>,
}

/// Loaded config + selected env + actor — the handle verbs run on.
pub struct Ops {
    config: Config,
    config_path: Option<PathBuf>,
    env: Env,
    actor: Actor,
}

impl Ops {
    /// Load config, resolve env + actor.
    ///
    /// Config search: explicit path → `ARCA_CONFIG` → `./arca.toml`
    /// → `~/.config/arca/arca.toml`. No file on the default search
    /// means defaults (local vault, `dev`) — arca works without a
    /// config; an explicit `--config`/`ARCA_CONFIG` that fails is an
    /// error, as is a file that fails to parse.
    pub fn load(opts: LoadOpts) -> Result<Self, OpsError> {
        let src = TomlSource {
            path: opts.config_path.clone(),
        };
        let located = src.locate();
        let config = match &located {
            Some(_) => src.load().map_err(OpsError::Config)?,
            None if opts.config_path.is_some() => {
                return Err(OpsError::Config(
                    // `locate()` found nothing — a load can't succeed.
                    src.load().err().unwrap_or("unreadable".into()),
                ));
            }
            None => Config::default(),
        };
        let env = resolve_env(opts.env.as_deref())?;
        let actor = resolve_actor(opts.actor.as_deref())?;
        Ok(Ops {
            config,
            config_path: located,
            env,
            actor,
        })
    }

    /// The loaded config.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Where config was loaded from, if anywhere.
    pub fn config_path(&self) -> Option<&PathBuf> {
        self.config_path.as_ref()
    }

    /// The selected env context.
    pub fn env(&self) -> &Env {
        &self.env
    }

    /// The acting identity (attribution).
    pub fn actor(&self) -> &Actor {
        &self.actor
    }

    /// Apply the env-pin rule: `arca://prod/x` resolves under `prod`
    /// regardless of context; anything else uses the selected env.
    /// Returns `(env, ref-with-pin-stripped)`.
    pub fn resolve(&self, rref: &Ref) -> (Env, Ref) {
        match rref.env_pin(|s| self.config.is_known_env(s)) {
            Some((env, rest)) => (env, rest),
            None => (self.env.clone(), rref.clone()),
        }
    }

    /// Open the vault an env routes to. `local` (the implicit
    /// default) is always the age backend.
    pub fn open_vault(&self, env: &Env) -> Result<Box<dyn Vault>, OpsError> {
        self.open_vault_named(self.config.vault_for(env))
    }

    /// Open a vault by `[vaults]` name (`local` resolves to its
    /// implicit defaults when unconfigured).
    pub fn open_vault_named(&self, name: &str) -> Result<Box<dyn Vault>, OpsError> {
        let cfg = self.config.vault_config(name);
        let kind = cfg.map(|c| c.kind.as_str()).unwrap_or("age");
        match kind {
            "age" => {
                let vault_path = cfg
                    .and_then(|c| c.path.as_ref())
                    .map(PathBuf::from)
                    .unwrap_or_else(AgeVault::default_vault_path);
                let identity_path = cfg
                    .and_then(|c| c.identity.as_ref())
                    .map(PathBuf::from)
                    .unwrap_or_else(AgeVault::default_identity_path);
                let recipients = cfg.map(|c| c.recipients.as_slice()).unwrap_or(&[]);
                let v = AgeVault::open(&vault_path, &identity_path, recipients)?;
                Ok(Box::new(v))
            }
            "cf" => {
                // `kind` was read off `cfg` — "cf" here means the
                // vault is configured, so this expect documents the
                // invariant rather than guarding a reachable path.
                #[allow(clippy::unwrap_used)]
                let c = cfg.expect("kind cf implies a configured vault");
                let url = c.url.as_deref().ok_or_else(|| {
                    OpsError::Config(format!("[vaults.{name}] kind cf needs a `url`"))
                })?;
                let token_env = c.token_env.as_deref().unwrap_or("ARCA_CF_TOKEN");
                let identity_path = c
                    .identity
                    .as_ref()
                    .map(PathBuf::from)
                    .unwrap_or_else(AgeVault::default_identity_path);
                let v = CfVault::new(url, token_env, &identity_path, &c.recipients)?;
                Ok(Box::new(v))
            }
            other => Err(OpsError::Config(format!(
                "vault {name:?} has unknown kind {other:?} (have: age, cf)"
            ))),
        }
    }

    /// Open a `[deploy.<name>]` sink — write-only push targets,
    /// never sources.
    pub fn open_deploy_named(&self, name: &str) -> Result<Box<dyn Deploy>, OpsError> {
        let cfg = self
            .config
            .deploy_config(name)
            .ok_or_else(|| OpsError::Config(format!("no [deploy.{name}] table in config")))?;
        match cfg.kind.as_str() {
            "cf-secrets-store" => {
                let need = |field: &str, v: &Option<String>| {
                    v.clone()
                        .ok_or_else(|| OpsError::Config(format!("[deploy.{name}] needs `{field}`")))
                };
                let token_env = cfg
                    .token_env
                    .as_deref()
                    .unwrap_or(arca_deploy_cf::DEFAULT_TOKEN_ENV);
                let d = CfSecretsStore::new(
                    name,
                    &need("account_id", &cfg.account_id)?,
                    &need("store_id", &cfg.store_id)?,
                    token_env,
                    cfg.base_url.as_deref(),
                )?;
                Ok(Box::new(d))
            }
            other => Err(OpsError::Config(format!(
                "deploy target {name:?} has unknown kind {other:?} (have: cf-secrets-store)"
            ))),
        }
    }

    /// The `(vault file, identity file)` pair an env's vault would
    /// use — for `init`/`check` reporting without opening anything.
    pub fn vault_paths(&self, env: &Env) -> Result<(PathBuf, PathBuf, Vec<String>), OpsError> {
        let name = self.config.vault_for(env);
        let cfg = self.config.vault_config(name);
        let kind = cfg.map(|c| c.kind.as_str()).unwrap_or("age");
        match kind {
            "age" => {}
            "cf" => {
                return Err(OpsError::Config(format!(
                    "vault {name:?} is a remote cf vault — `arca init` only \
                     bootstraps local age vaults"
                )));
            }
            other => {
                return Err(OpsError::Config(format!(
                    "vault {name:?} has unknown kind {other:?} (have: age, cf)"
                )));
            }
        }
        Ok((
            cfg.and_then(|c| c.path.as_ref())
                .map(PathBuf::from)
                .unwrap_or_else(AgeVault::default_vault_path),
            cfg.and_then(|c| c.identity.as_ref())
                .map(PathBuf::from)
                .unwrap_or_else(AgeVault::default_identity_path),
            cfg.map(|c| c.recipients.clone()).unwrap_or_default(),
        ))
    }

    /// `arca init` — ensure the identity exists (generate 0600
    /// keyfile if missing), then verify the vault opens.
    pub fn init(&self) -> Result<InitInfo, OpsError> {
        let (vault_path, identity_path, recipients) = self.vault_paths(&self.env)?;
        let created = AgeVault::ensure_identity(&identity_path)?;
        let vault = AgeVault::open(&vault_path, &identity_path, &recipients)?;
        Ok(InitInfo {
            vault_path,
            identity_path,
            identity_created: created,
            recipient: vault.public_recipient(),
        })
    }

    /// Store a secret (env-pin aware); emits an audit event.
    pub fn put(&self, rref: &Ref, value: Secret) -> Result<SecretRecord, OpsError> {
        let (env, r) = self.resolve(rref);
        let rec = self.open_vault(&env)?.put(&env, &r, value)?;
        self.audit(Op::Put, &r, &env);
        Ok(rec)
    }

    /// Read a secret (env-pin aware); `None` when absent. Emits a
    /// materialization audit event only when a value comes out.
    pub fn get(&self, rref: &Ref) -> Result<Option<SecretRecord>, OpsError> {
        let (env, r) = self.resolve(rref);
        let rec = self.open_vault(&env)?.get(&env, &r)?;
        if rec.is_some() {
            self.audit(Op::Get, &r, &env);
        }
        Ok(rec)
    }

    /// Remove a secret; `true` when something was removed.
    pub fn rm(&self, rref: &Ref) -> Result<bool, OpsError> {
        let (env, r) = self.resolve(rref);
        let gone = self.open_vault(&env)?.rm(&env, &r)?;
        if gone {
            self.audit(Op::Rm, &r, &env);
        }
        Ok(gone)
    }

    /// Rotate a secret to a new value (env-pin aware).
    pub fn rotate(&self, rref: &Ref, value: Secret) -> Result<SecretRecord, OpsError> {
        let (env, r) = self.resolve(rref);
        let rec = self.open_vault(&env)?.rotate(&env, &r, value)?;
        self.audit(Op::Rotate, &r, &env);
        Ok(rec)
    }

    /// Push a stored value to a deploy target — the vault is the
    /// source, the `[deploy.NAME]` table the sink. The plaintext
    /// value crosses here (and only here): the sink is an external
    /// store that serves runtimes directly, outside arca's
    /// ciphertext boundary. Emits an `op = "deploy"` audit event.
    ///
    /// `target` selects the sink; absent, a lone configured target
    /// is used — ambiguity and absence are both errors.
    pub fn deploy(&self, rref: &Ref, target: Option<&str>) -> Result<DeployReport, OpsError> {
        let name = match target {
            Some(t) => t.to_string(),
            None => match self.config.deploy.len() {
                1 => self
                    .config
                    .deploy
                    .keys()
                    .next()
                    .cloned()
                    .unwrap_or_default(),
                0 => return Err(OpsError::Config("no [deploy.*] targets configured".into())),
                _ => {
                    let names: Vec<&str> = self.config.deploy.keys().map(String::as_str).collect();
                    return Err(OpsError::Config(format!(
                        "multiple deploy targets — pass --target ({})",
                        names.join(", ")
                    )));
                }
            },
        };
        let (env, r) = self.resolve(rref);
        let rec = self
            .open_vault(&env)?
            .get(&env, &r)?
            .ok_or_else(|| OpsError::NotStored {
                var: "(deploy)".into(),
                rref: rref.to_string(),
                env: env.as_str().to_string(),
            })?;
        let sink = self.open_deploy_named(&name)?;
        let report = sink.push(&env, &r, &rec.value)?;
        self.audit(Op::Deploy, &r, &env);
        Ok(DeployReport {
            target: name,
            env,
            rref: r,
            secret_name: report.name,
            created: report.created,
        })
    }

    /// Read a secret in an explicit env — the audited read for
    /// callers that already know the env (the TUI's reveal, which
    /// browses envs other than the selected one). Same event
    /// [`get`](Self::get) emits: one materialization per value out.
    pub fn get_in(&self, env: &Env, rref: &Ref) -> Result<Option<SecretRecord>, OpsError> {
        let rec = self.open_vault(env)?.get(env, rref)?;
        if rec.is_some() {
            self.audit(Op::Get, rref, env);
        }
        Ok(rec)
    }

    /// Refs stored in the selected env (or an explicit one).
    pub fn list(&self, env: Option<&Env>) -> Result<(Env, Vec<Ref>), OpsError> {
        let env = env.cloned().unwrap_or_else(|| self.env.clone());
        let refs = self.open_vault(&env)?.list(&env)?;
        Ok((env, refs))
    }

    /// Refs plus their records (metadata) in the selected env —
    /// backs `list --json`. Values come along inside
    /// [`SecretRecord`]s but are never printed.
    pub fn list_records(
        &self,
        env: Option<&Env>,
    ) -> Result<(Env, Vec<(Ref, SecretRecord)>), OpsError> {
        let env = env.cloned().unwrap_or_else(|| self.env.clone());
        let vault = self.open_vault(&env)?;
        let refs = vault.list(&env)?;
        let mut out = Vec::new();
        for r in refs {
            if let Some(rec) = vault.get(&env, &r)? {
                out.push((r, rec));
            }
        }
        Ok((env, out))
    }

    /// Resolve every declared + ad-hoc `VAR = arca://ref` pair into
    /// a `VAR → Secret` map for injection. Emits one
    /// materialization event per resolved ref. Missing refs and bad
    /// var names fail the whole operation — partial injection is
    /// worse than none.
    pub fn resolve_for_injection(
        &self,
        op: Op,
        extra: &[(String, String)],
    ) -> Result<BTreeMap<String, Secret>, OpsError> {
        // declared surface first, then ad-hoc overrides/additions
        let mut wanted: BTreeMap<String, Ref> = self.config.secrets.clone();
        for (var, raw) in extra {
            if !is_valid_env_var(var) {
                return Err(OpsError::Msg(format!("invalid env var name {var:?}")));
            }
            wanted.insert(var.clone(), raw.parse::<Ref>()?);
        }
        let mut vaults: BTreeMap<String, Box<dyn Vault>> = BTreeMap::new();
        let mut out = BTreeMap::new();
        for (var, rref) in wanted {
            if !is_valid_env_var(&var) {
                return Err(OpsError::Msg(format!(
                    "[secrets] key {var:?} is not a valid env var name"
                )));
            }
            let (env, r) = self.resolve(&rref);
            let vault = match vaults.entry(env.as_str().to_string()) {
                std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::btree_map::Entry::Vacant(e) => e.insert(self.open_vault(&env)?),
            };
            let rec = vault.get(&env, &r)?.ok_or_else(|| OpsError::NotStored {
                var: var.clone(),
                rref: rref.to_string(),
                env: env.as_str().to_string(),
            })?;
            self.audit(op, &r, &env);
            out.insert(var, rec.value);
        }
        Ok(out)
    }

    /// `arca check` — validate the config, the declared surface,
    /// and vault readability. Opens vaults read-only (no identity
    /// generation, no writes); declared secrets that don't resolve
    /// and undecryptable vaults are findings.
    pub fn check(&self) -> CheckReport {
        let mut problems = Vec::new();
        for var in self.config.secrets.keys() {
            if !is_valid_env_var(var) {
                problems.push(format!("[secrets] key {var:?} is not a valid env var name"));
            }
        }
        for (env, route) in &self.config.envs {
            let name = route.vault();
            if name != arca_core::config::LOCAL_VAULT && !self.config.vaults.contains_key(name) {
                problems.push(format!(
                    "[envs] {env} routes to unconfigured vault {name:?}"
                ));
            }
        }
        for (name, v) in &self.config.vaults {
            if !matches!(v.kind.as_str(), "age" | "cf") {
                problems.push(format!("[vaults.{name}] kind {:?} — have: age, cf", v.kind));
            }
            if v.kind == "cf" && v.url.is_none() {
                problems.push(format!("[vaults.{name}] kind cf needs a `url`"));
            }
        }
        for (name, d) in &self.config.deploy {
            if d.kind != "cf-secrets-store" {
                problems.push(format!(
                    "[deploy.{name}] kind {:?} — have: cf-secrets-store",
                    d.kind
                ));
            }
            if d.account_id.is_none() || d.store_id.is_none() {
                problems.push(format!("[deploy.{name}] needs `account_id` and `store_id`"));
            }
        }

        // Every vault worth reporting on: configured names plus
        // every route's target (the implicit `local` included).
        let mut vault_names: Vec<String> = self.config.vaults.keys().cloned().collect();
        for route in self.config.envs.values() {
            let name = route.vault().to_string();
            if !vault_names.contains(&name) {
                vault_names.push(name);
            }
        }
        let selected_vault = self.config.vault_for(&self.env).to_string();
        if !vault_names.contains(&selected_vault) {
            vault_names.push(selected_vault.clone());
        }
        vault_names.sort();
        vault_names.dedup();

        let mut vaults = Vec::new();
        let mut opened: BTreeMap<String, Box<dyn Vault>> = BTreeMap::new();
        let mut failures: BTreeMap<String, String> = BTreeMap::new();
        for name in &vault_names {
            let cfg = self.config.vault_config(name);
            let kind = cfg.map(|c| c.kind.clone()).unwrap_or_else(|| "age".into());
            let path = cfg.and_then(|c| c.path.as_ref()).map(PathBuf::from);
            match self.open_vault_named(name).and_then(|v| {
                // Probe: construction is cheap (no I/O); a `list`
                // proves a remote vault is actually reachable.
                match v.list(&self.env) {
                    Ok(_) => Ok(v),
                    Err(e) => Err(e.into()),
                }
            }) {
                Ok(v) => {
                    opened.insert(name.clone(), v);
                    vaults.push(VaultCheck {
                        name: name.clone(),
                        kind,
                        path,
                        ok: true,
                        error: None,
                    });
                }
                Err(e) => {
                    problems.push(format!("vault {name:?} is not readable: {e}"));
                    failures.insert(name.clone(), e.to_string());
                    vaults.push(VaultCheck {
                        name: name.clone(),
                        kind,
                        path,
                        ok: false,
                        error: Some(e.to_string()),
                    });
                }
            }
        }

        // Declared surface: does each VAR's ref resolve in its env?
        let mut secrets = Vec::new();
        for (var, rref) in &self.config.secrets {
            let (env, r) = self.resolve(rref);
            let vault_name = self.config.vault_for(&env);
            let resolves = match opened.get(vault_name) {
                Some(v) => v.get(&env, &r).map(|rec| rec.is_some()).unwrap_or(false),
                None => {
                    // The vault probe above already tried this name
                    // and it failed — a second open would fail
                    // identically, so report the recorded error.
                    let err = failures
                        .get(vault_name)
                        .map(String::as_str)
                        .unwrap_or("unknown error");
                    problems.push(format!(
                        "{var} → {rref}: env {env} vault {vault_name:?} not readable: {err}"
                    ));
                    false
                }
            };
            if !resolves && is_valid_env_var(var) {
                problems.push(format!("{var} → {rref} is not stored in env {env}"));
            }
            secrets.push(SecretCheck {
                var: var.clone(),
                rref: r.to_string(),
                env: env.as_str().to_string(),
                resolves,
            });
        }

        CheckReport {
            config_path: self.config_path.clone(),
            env: self.env.clone(),
            vault: selected_vault,
            vaults,
            secrets,
            guardrails: self.config.guardrails.clone(),
            problems,
        }
    }

    /// Emit an op-scoped audit event — denials, failures, and
    /// ref-less verb completions the front-end knows about.
    /// `rref` is the raw requested ref when the verb takes one.
    pub fn emit(&self, op: Op, rref: Option<&str>, outcome: Outcome) {
        audit::emit(&AuditEvent::op(
            self.actor.as_str(),
            op,
            rref,
            self.env.as_str(),
            outcome,
        ));
    }

    fn audit(&self, op: Op, rref: &Ref, env: &Env) {
        audit::emit(&AuditEvent::materialized(
            self.actor.as_str(),
            op,
            &rref.path(),
            env.as_str(),
        ));
    }
}

/// `--env` → `ARCA_ENV` → `dev`.
fn resolve_env(flag: Option<&str>) -> Result<Env, OpsError> {
    let name = flag
        .map(str::to_string)
        .or_else(|| std::env::var("ARCA_ENV").ok())
        .unwrap_or_else(|| "dev".into());
    Ok(Env::new(&name)?)
}

/// `--actor`/`ARCA_ACTOR` (must validate) → `$USER`/`$LOGNAME`
/// (best-effort) → `anon`.
fn resolve_actor(flag: Option<&str>) -> Result<Actor, OpsError> {
    let claimed = flag
        .map(str::to_string)
        .or_else(|| std::env::var("ARCA_ACTOR").ok());
    if let Some(a) = claimed {
        return Ok(Actor::new(&a)?);
    }
    for var in ["USER", "LOGNAME"] {
        if let Ok(u) = std::env::var(var) {
            if let Ok(a) = Actor::new(&u) {
                return Ok(a);
            }
        }
    }
    Actor::new("anon").map_err(OpsError::Actor)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn env_pin_overrides_context() {
        let ops = Ops {
            config: Config::default(),
            config_path: None,
            env: Env::dev(),
            actor: Actor::new("devin").unwrap(),
        };
        let (env, r) = ops.resolve(&Ref::new("arca://prod/billing/stripe").unwrap());
        assert_eq!(env.as_str(), "prod");
        assert_eq!(r.path(), "billing/stripe");
        let (env, r) = ops.resolve(&Ref::new("arca://billing/stripe").unwrap());
        assert_eq!(env.as_str(), "dev");
        assert_eq!(r.path(), "billing/stripe");
        // custom envs count when configured
        let mut config = Config::default();
        config.envs.insert(Env::new("ci").unwrap(), "local".into());
        let ops = Ops { config, ..ops };
        let (env, _) = ops.resolve(&Ref::new("arca://ci/token").unwrap());
        assert_eq!(env.as_str(), "ci");
    }

    #[test]
    fn check_flags_bad_var_names_and_routes() {
        let mut config = Config::default();
        config
            .secrets
            .insert("NOT-A-VAR".into(), Ref::new("arca://x/y").unwrap());
        config
            .envs
            .insert(Env::new("ci").unwrap(), "nowhere".into());
        let ops = Ops {
            config,
            config_path: None,
            env: Env::dev(),
            actor: Actor::new("devin").unwrap(),
        };
        // Isolate HOME so default vault paths miss deterministically.
        let home = tempfile::tempdir().unwrap();
        temp_env::with_vars(
            [
                ("HOME", Some(home.path().to_str().unwrap())),
                ("XDG_CONFIG_HOME", Some(home.path().to_str().unwrap())),
                ("XDG_DATA_HOME", Some(home.path().to_str().unwrap())),
                ("ARCA_IDENTITY_FILE", None::<&str>),
            ],
            || {
                let report = ops.check();
                let has = |needle: &str| report.problems.iter().any(|p| p.contains(needle));
                assert!(has("not a valid env var"), "{:?}", report.problems);
                assert!(has("unconfigured vault"), "{:?}", report.problems);
                // vaults can't open without an identity → reported
                assert!(report.vaults.iter().all(|v| !v.ok));
                assert!(has("not readable"), "{:?}", report.problems);
            },
        );
    }

    #[test]
    fn check_reports_resolving_secrets() {
        let home = tempfile::tempdir().unwrap();
        let id_path = home.path().join("identity.txt");
        let vault_path = home.path().join("vault.age");
        AgeVault::ensure_identity(&id_path).unwrap();
        let vault = AgeVault::open(&vault_path, &id_path, &[]).unwrap();
        Vault::put(
            &vault,
            &Env::dev(),
            &Ref::new("arca://billing/stripe").unwrap(),
            Secret::new("fake-check-value"),
        )
        .unwrap();

        let mut config = Config::default();
        config.vaults.insert(
            "local".into(),
            arca_core::VaultConfig {
                kind: "age".into(),
                path: Some(vault_path.to_string_lossy().into_owned()),
                identity: Some(id_path.to_string_lossy().into_owned()),
                recipients: vec![],
                url: None,
                token_env: None,
            },
        );
        config.secrets.insert(
            "STRIPE_KEY".into(),
            Ref::new("arca://billing/stripe").unwrap(),
        );
        config
            .secrets
            .insert("MISSING".into(), Ref::new("arca://not/here").unwrap());
        let ops = Ops {
            config,
            config_path: None,
            env: Env::dev(),
            actor: Actor::new("devin").unwrap(),
        };
        let report = ops.check();
        assert!(report.vaults.iter().all(|v| v.ok), "{:?}", report.vaults);
        let stripe = report.secrets.iter().find(|s| s.var == "STRIPE_KEY");
        assert_eq!(stripe.map(|s| s.resolves), Some(true));
        let missing = report.secrets.iter().find(|s| s.var == "MISSING");
        assert_eq!(missing.map(|s| s.resolves), Some(false));
        assert!(
            report
                .problems
                .iter()
                .any(|p| p.contains("MISSING") && p.contains("not stored")),
            "{:?}",
            report.problems
        );
    }

    fn mk_ops(config: Config) -> Ops {
        Ops {
            config,
            config_path: None,
            env: Env::dev(),
            actor: Actor::new("devin").unwrap(),
        }
    }

    fn vault_cfg(kind: &str, url: Option<&str>) -> arca_core::VaultConfig {
        arca_core::VaultConfig {
            kind: kind.into(),
            path: None,
            identity: None,
            recipients: vec![],
            url: url.map(str::to_string),
            token_env: None,
        }
    }

    fn deploy_cfg(
        kind: &str,
        account: Option<&str>,
        store: Option<&str>,
    ) -> arca_core::DeployConfig {
        arca_core::DeployConfig {
            kind: kind.into(),
            account_id: account.map(str::to_string),
            store_id: store.map(str::to_string),
            token_env: None,
            base_url: None,
        }
    }

    /// Isolate every path an age vault could pick up by default.
    fn isolate_home(home: &tempfile::TempDir, f: impl FnOnce()) {
        temp_env::with_vars(
            [
                ("HOME", Some(home.path().to_str().unwrap())),
                (
                    "XDG_CONFIG_HOME",
                    Some(home.path().join("c").to_str().unwrap()),
                ),
                (
                    "XDG_DATA_HOME",
                    Some(home.path().join("d").to_str().unwrap()),
                ),
                ("ARCA_IDENTITY_FILE", None::<&str>),
                ("ARCA_CONFIG", None::<&str>),
            ],
            f,
        );
    }

    /// An Ops whose `local` vault is a real age vault in a tempdir,
    /// with `refs` pre-stored under `env`.
    fn ops_with_vault(env: &Env, refs: &[&str]) -> (tempfile::TempDir, Ops) {
        let home = tempfile::tempdir().unwrap();
        let id_path = home.path().join("identity.txt");
        let vault_path = home.path().join("vault.age");
        AgeVault::ensure_identity(&id_path).unwrap();
        let vault = AgeVault::open(&vault_path, &id_path, &[]).unwrap();
        for r in refs {
            Vault::put(
                &vault,
                env,
                &Ref::new(r).unwrap(),
                Secret::new("fake-stored-value"),
            )
            .unwrap();
        }
        let mut config = Config::default();
        let mut v = vault_cfg("age", None);
        v.path = Some(vault_path.to_string_lossy().into_owned());
        v.identity = Some(id_path.to_string_lossy().into_owned());
        config.vaults.insert("local".into(), v);
        (home, mk_ops(config))
    }

    /// An Ops whose `dev` env routes to a cf vault on `url` —
    /// `token_var` is left to the caller to set (or not).
    fn ops_on_cf(url: &str, token_var: &str, id_path: &std::path::Path) -> Ops {
        let mut config = Config::default();
        let mut v = vault_cfg("cf", Some(url));
        v.token_env = Some(token_var.into());
        v.identity = Some(id_path.to_string_lossy().into_owned());
        config.vaults.insert("cloud".into(), v);
        config.envs.insert(Env::dev(), "cloud".into());
        mk_ops(config)
    }

    fn write_identity(dir: &std::path::Path) -> PathBuf {
        let id_path = dir.join("identity.txt");
        AgeVault::ensure_identity(&id_path).unwrap();
        id_path
    }

    #[test]
    fn load_reports_explicit_config_and_flag_errors() {
        // explicit --config that doesn't exist → config error
        let e = Ops::load(LoadOpts {
            config_path: Some("/nonexistent/arca.toml".into()),
            ..LoadOpts::default()
        })
        .err()
        .unwrap();
        assert!(format!("{e:?}").starts_with("Config"), "{e}");
        // explicit --config that exists but won't parse
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("arca.toml");
        std::fs::write(&bad, "[[[ not toml").unwrap();
        let e = Ops::load(LoadOpts {
            config_path: Some(bad.to_string_lossy().into_owned()),
            ..LoadOpts::default()
        })
        .err()
        .unwrap();
        assert!(format!("{e:?}").starts_with("Config"), "{e}");
        // bad --env and bad --actor surface as typed errors
        let e = Ops::load(LoadOpts {
            env: Some("PROD".into()),
            ..LoadOpts::default()
        })
        .err()
        .unwrap();
        assert!(format!("{e:?}").starts_with("Env"), "{e}");
        let e = Ops::load(LoadOpts {
            actor: Some("bad actor!".into()),
            ..LoadOpts::default()
        })
        .err()
        .unwrap();
        assert!(format!("{e:?}").starts_with("Actor"), "{e}");
    }

    #[test]
    fn env_and_actor_resolution_variants() {
        let home = tempfile::tempdir().unwrap();
        isolate_home(&home, || {
            temp_env::with_vars(
                [("ARCA_ENV", Some("ci")), ("ARCA_ACTOR", Some("bot-1"))],
                || {
                    let ops = Ops::load(LoadOpts::default()).unwrap();
                    assert_eq!(ops.env().as_str(), "ci");
                    assert_eq!(ops.actor().as_str(), "bot-1");
                    assert!(ops.config_path().is_none());
                },
            );
            // an unreadable ARCA_ENV is a typed error
            temp_env::with_vars(
                [("ARCA_ENV", Some("BAD!!")), ("ARCA_ACTOR", Some("bot"))],
                || {
                    let e = Ops::load(LoadOpts::default()).err().unwrap();
                    assert!(format!("{e:?}").starts_with("Env"), "{e}");
                },
            );
            // USER present and valid → claimed as actor
            temp_env::with_vars(
                [
                    ("ARCA_ACTOR", None::<&str>),
                    ("USER", Some("luis-x")),
                    ("LOGNAME", None::<&str>),
                ],
                || {
                    let ops = Ops::load(LoadOpts::default()).unwrap();
                    assert_eq!(ops.actor().as_str(), "luis-x");
                },
            );
            // USER invalid + LOGNAME unset → the anon fallback
            temp_env::with_vars(
                [
                    ("ARCA_ACTOR", None::<&str>),
                    ("USER", Some("bad name")),
                    ("LOGNAME", None::<&str>),
                ],
                || {
                    let ops = Ops::load(LoadOpts::default()).unwrap();
                    assert_eq!(ops.actor().as_str(), "anon");
                },
            );
        });
    }

    #[test]
    fn open_vault_named_rejects_bad_kinds_and_missing_cf_url() {
        // cf without a url → a named config error
        let mut config = Config::default();
        config.vaults.insert("cloud".into(), vault_cfg("cf", None));
        let ops = mk_ops(config.clone());
        let e = ops.open_vault_named("cloud").err().unwrap().to_string();
        assert!(e.contains("needs a `url`"), "{e}");
        // unknown kind → named error listing what exists
        config.vaults.insert("bogus".into(), vault_cfg("zzz", None));
        let ops = mk_ops(config);
        let e = ops.open_vault_named("bogus").err().unwrap().to_string();
        assert!(e.contains("unknown kind") && e.contains("zzz"), "{e}");
    }

    #[test]
    fn open_deploy_named_covers_table_errors() {
        // no table at all
        let ops = mk_ops(Config::default());
        let e = ops.open_deploy_named("prod").err().unwrap().to_string();
        assert!(e.contains("no [deploy.prod] table"), "{e}");
        // missing account_id/store_id fields
        let mut config = Config::default();
        config
            .deploy
            .insert("prod".into(), deploy_cfg("cf-secrets-store", None, None));
        let ops = mk_ops(config.clone());
        let e = ops.open_deploy_named("prod").err().unwrap().to_string();
        assert!(e.contains("account_id"), "{e}");
        // account_id present, store_id missing → the second `need`
        let mut config2 = Config::default();
        config2.deploy.insert(
            "prod".into(),
            deploy_cfg("cf-secrets-store", Some("acc"), None),
        );
        let ops = mk_ops(config2);
        let e = ops.open_deploy_named("prod").err().unwrap().to_string();
        assert!(e.contains("store_id"), "{e}");
        // unknown kind
        config
            .deploy
            .insert("odd".into(), deploy_cfg("gcs", None, None));
        let ops = mk_ops(config);
        let e = ops.open_deploy_named("odd").err().unwrap().to_string();
        assert!(e.contains("unknown kind") && e.contains("gcs"), "{e}");
        // complete shape but the token env var is unset
        temp_env::with_var_unset("ARCA_TEST_DEPLOY_TOKEN", || {
            let mut config = Config::default();
            let mut d = deploy_cfg("cf-secrets-store", Some("acc"), Some("sto"));
            d.token_env = Some("ARCA_TEST_DEPLOY_TOKEN".into());
            config.deploy.insert("prod".into(), d);
            let ops = mk_ops(config);
            let e = ops.open_deploy_named("prod").err().unwrap().to_string();
            assert!(e.contains("ARCA_TEST_DEPLOY_TOKEN"), "{e}");
        });
    }

    #[test]
    fn vault_paths_reports_configured_paths_and_kind_errors() {
        // a configured age vault reports its configured paths
        let mut config = Config::default();
        let mut v = vault_cfg("age", None);
        v.path = Some("/tmp/x/vault.age".into());
        v.identity = Some("/tmp/x/id.txt".into());
        v.recipients =
            vec!["age1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqygh9".to_string()];
        config.vaults.insert("local".into(), v);
        let ops = mk_ops(config);
        let (vp, ip, rec) = ops.vault_paths(&Env::dev()).unwrap();
        assert_eq!(vp, PathBuf::from("/tmp/x/vault.age"));
        assert_eq!(ip, PathBuf::from("/tmp/x/id.txt"));
        assert_eq!(rec.len(), 1);
        // cf and unknown kinds can't init locally
        let mut config = Config::default();
        config
            .vaults
            .insert("cloud".into(), vault_cfg("cf", Some("http://x")));
        config.envs.insert(Env::new("ci").unwrap(), "cloud".into());
        config.vaults.insert("bogus".into(), vault_cfg("zzz", None));
        config.envs.insert(Env::new("st").unwrap(), "bogus".into());
        let ops = mk_ops(config);
        let e = ops
            .vault_paths(&Env::new("ci").unwrap())
            .unwrap_err()
            .to_string();
        assert!(e.contains("remote cf"), "{e}");
        let e = ops
            .vault_paths(&Env::new("st").unwrap())
            .unwrap_err()
            .to_string();
        assert!(e.contains("unknown kind"), "{e}");
    }

    #[test]
    fn init_happy_and_error_paths() {
        // happy: isolated HOME → fresh identity generated, vault opens
        let home = tempfile::tempdir().unwrap();
        isolate_home(&home, || {
            let ops = mk_ops(Config::default());
            let info = ops.init().unwrap();
            assert!(info.identity_created);
            assert!(info.recipient.starts_with("age1"));
            assert!(info.identity_path.exists());
            assert!(info.vault_path.ends_with("vault.age"));
        });
        // cf-routed env → init refuses (remote vaults can't init locally)
        let mut config = Config::default();
        config
            .vaults
            .insert("cloud".into(), vault_cfg("cf", Some("http://x")));
        config.envs.insert(Env::dev(), "cloud".into());
        let ops = mk_ops(config);
        assert!(ops.init().unwrap_err().to_string().contains("cf"));
        // identity path whose parent can't be created → ensure fails
        let home = tempfile::tempdir().unwrap();
        let blocker = home.path().join("blocker");
        std::fs::write(&blocker, "x").unwrap();
        let mut config = Config::default();
        let mut v = vault_cfg("age", None);
        v.path = Some(home.path().join("v1.age").to_string_lossy().into_owned());
        v.identity = Some(blocker.join("id.txt").to_string_lossy().into_owned());
        config.vaults.insert("local".into(), v);
        let ops = mk_ops(config);
        assert!(ops.init().is_err());
        // garbage identity present → ensure skips it, open fails
        let bad_id = home.path().join("bad-id.txt");
        std::fs::write(&bad_id, "not-a-key\n").unwrap();
        let mut config = Config::default();
        let mut v = vault_cfg("age", None);
        v.path = Some(home.path().join("v2.age").to_string_lossy().into_owned());
        v.identity = Some(bad_id.to_string_lossy().into_owned());
        config.vaults.insert("local".into(), v);
        let ops = mk_ops(config);
        assert!(ops.init().is_err());
    }

    #[test]
    fn verbs_fail_when_the_vault_cant_open() {
        // env dev routes to a vault of unknown kind → every verb's
        // open fails with a config error, not a panic
        let mut config = Config::default();
        config.vaults.insert("bogus".into(), vault_cfg("zzz", None));
        config.envs.insert(Env::dev(), "bogus".into());
        let ops = mk_ops(config);
        let r = Ref::new("arca://x/y").unwrap();
        assert!(ops.put(&r, Secret::new("fake")).is_err());
        assert!(ops.get(&r).is_err());
        assert!(ops.rm(&r).is_err());
        assert!(ops.rotate(&r, Secret::new("fake")).is_err());
        assert!(ops.get_in(&Env::dev(), &r).is_err());
        assert!(ops.list(None).is_err());
        assert!(ops.list_records(None).is_err());
        assert!(ops.deploy(&r, Some("t")).is_err());
        // an explicit env other than the selected one works the same
        assert!(ops.list(Some(&Env::dev())).is_err());
    }

    #[test]
    fn deploy_target_selection_errors() {
        let r = Ref::new("arca://x/y").unwrap();
        // zero targets configured → can't pick
        let ops = mk_ops(Config::default());
        let e = ops.deploy(&r, None).unwrap_err().to_string();
        assert!(e.contains("no [deploy.*] targets"), "{e}");
        // several targets and no --target → ambiguous
        let mut config = Config::default();
        config.deploy.insert(
            "a".into(),
            deploy_cfg("cf-secrets-store", Some("x"), Some("y")),
        );
        config.deploy.insert(
            "b".into(),
            deploy_cfg("cf-secrets-store", Some("x"), Some("y")),
        );
        let ops = mk_ops(config);
        let e = ops.deploy(&r, None).unwrap_err().to_string();
        assert!(e.contains("multiple deploy targets"), "{e}");
        // a --target name that has no table → open_deploy_named errs
        let (_h, ops) = ops_with_vault(&Env::dev(), &["arca://x/y"]);
        let e = ops.deploy(&r, Some("ghost")).unwrap_err().to_string();
        assert!(e.contains("no [deploy.ghost] table"), "{e}");
    }

    #[test]
    fn get_in_reads_across_envs() {
        let (_h, ops) = ops_with_vault(&Env::new("prod").unwrap(), &["arca://x/y"]);
        let got = ops
            .get_in(&Env::new("prod").unwrap(), &Ref::new("arca://x/y").unwrap())
            .unwrap();
        assert_eq!(got.unwrap().value.expose(), "fake-stored-value");
        let miss = ops
            .get_in(
                &Env::new("prod").unwrap(),
                &Ref::new("arca://no/thing").unwrap(),
            )
            .unwrap();
        assert!(miss.is_none());
    }

    #[test]
    fn injection_validates_pairs_and_var_names() {
        let ops = mk_ops(Config::default());
        // bad ad-hoc var name
        let e = ops
            .resolve_for_injection(Op::Run, &[("NOT-A-VAR".into(), "arca://x/y".into())])
            .unwrap_err()
            .to_string();
        assert!(e.contains("invalid env var"), "{e}");
        // bad ad-hoc ref
        assert!(ops
            .resolve_for_injection(Op::Run, &[("GOOD".into(), "not-a-ref".into())])
            .is_err());
        // a declared [secrets] key that isn't a var name
        let mut config = Config::default();
        config
            .secrets
            .insert("NOT-A-VAR".into(), Ref::new("arca://x/y").unwrap());
        let ops = mk_ops(config);
        let e = ops
            .resolve_for_injection(Op::Run, &[])
            .unwrap_err()
            .to_string();
        assert!(e.contains("not a valid env var"), "{e}");
        // a ref pinned to an env whose vault can't open
        let mut config = Config::default();
        config
            .envs
            .insert(Env::new("ci").unwrap(), "nowhere".into());
        config
            .secrets
            .insert("V".into(), Ref::new("arca://ci/x").unwrap());
        let ops = mk_ops(config);
        let home = tempfile::tempdir().unwrap();
        isolate_home(&home, || {
            assert!(ops.resolve_for_injection(Op::Run, &[]).is_err());
        });
    }

    #[test]
    fn check_flags_kind_and_deploy_problems() {
        let mut config = Config::default();
        config.vaults.insert("bad".into(), vault_cfg("zzz", None));
        config.vaults.insert("nolink".into(), vault_cfg("cf", None));
        config
            .deploy
            .insert("weird".into(), deploy_cfg("bogus-kind", None, None));
        config
            .deploy
            .insert("half".into(), deploy_cfg("cf-secrets-store", None, None));
        // account set, store missing → the `||` right arm runs too
        config.deploy.insert(
            "half2".into(),
            deploy_cfg("cf-secrets-store", Some("a"), None),
        );
        let ops = mk_ops(config);
        let home = tempfile::tempdir().unwrap();
        isolate_home(&home, || {
            let report = ops.check();
            let has = |n: &str| report.problems.iter().any(|p| p.contains(n));
            assert!(has("\"zzz\""), "{:?}", report.problems);
            assert!(has("needs a `url`"), "{:?}", report.problems);
            assert!(has("bogus-kind"), "{:?}", report.problems);
            assert!(has("account_id"), "{:?}", report.problems);
            // none of the configured vaults can open either
            assert!(report.vaults.iter().all(|v| !v.ok));
        });
    }

    #[test]
    fn check_reports_open_ok_but_list_fails_and_secret_findings() {
        // A cf vault: construction is cheap (no I/O) — it opens,
        // then `list` proves it unreachable → "not readable" on the
        // vault AND on each secret that routes through it.
        let home = tempfile::tempdir().unwrap();
        let id_path = write_identity(home.path());
        let mut config = Config::default();
        let mut v = vault_cfg("cf", Some("http://127.0.0.1:1"));
        v.identity = Some(id_path.to_string_lossy().into_owned());
        config.vaults.insert("cloud".into(), v);
        config.envs.insert(Env::dev(), "cloud".into());
        config
            .secrets
            .insert("V".into(), Ref::new("arca://x/y").unwrap());
        let ops = mk_ops(config);
        temp_env::with_vars(
            [
                ("ARCA_CF_TOKEN", Some("fake-cf-token")),
                ("HOME", Some(home.path().to_str().unwrap())),
                (
                    "XDG_CONFIG_HOME",
                    Some(home.path().join("c").to_str().unwrap()),
                ),
                (
                    "XDG_DATA_HOME",
                    Some(home.path().join("d").to_str().unwrap()),
                ),
                ("ARCA_IDENTITY_FILE", None::<&str>),
            ],
            || {
                let report = ops.check();
                let has = |n: &str| report.problems.iter().any(|p| p.contains(n));
                assert!(has("not readable"), "{:?}", report.problems);
                assert!(has("V → arca://x/y"), "{:?}", report.problems);
                let cloud = report.vaults.iter().find(|v| v.name == "cloud").unwrap();
                assert!(!cloud.ok);
                assert!(cloud.error.is_some());
            },
        );
    }

    #[test]
    fn cf_backend_errors_propagate_through_verbs() {
        use arca_testkit::httpmock::{HttpMock, Response};
        // List says the ref exists; single-get says otherwise or
        // just fails — the verbs surface backend errors, no panic.
        let mock = HttpMock::new(|req| {
            if req.path == "/v1/dev" {
                Response::json(200, r#"{"refs":["gone/one","x/y"]}"#)
            } else if req.path.ends_with("/gone/one") {
                Response::json(404, r#"{"error":"not found"}"#)
            } else {
                Response::json(500, r#"{"error":"boom"}"#)
            }
        });
        let home = tempfile::tempdir().unwrap();
        let id_path = write_identity(home.path());
        let ops = ops_on_cf(mock.url(), "ARCA_TEST_OPS_TOKEN", &id_path);
        temp_env::with_var("ARCA_TEST_OPS_TOKEN", Some("fake-token"), || {
            let r = Ref::new("arca://x/y").unwrap();
            // backend 500s propagate as vault errors on every verb
            assert!(ops.get(&r).is_err());
            assert!(ops.put(&r, Secret::new("fake")).is_err());
            assert!(ops.rm(&r).is_err());
            assert!(ops.rotate(&r, Secret::new("fake")).is_err());
            // list_records: gone/one → 404 → skipped silently;
            // x/y → 500 → the error propagates
            assert!(ops.list_records(None).is_err());
            // injection hits the same failing get
            let mut config = Config::default();
            let mut v = vault_cfg("cf", Some(mock.url()));
            v.token_env = Some("ARCA_TEST_OPS_TOKEN".into());
            v.identity = Some(id_path.to_string_lossy().into_owned());
            config.vaults.insert("cloud".into(), v);
            config.envs.insert(Env::dev(), "cloud".into());
            config.secrets.insert("V".into(), r.clone());
            let ops = mk_ops(config);
            assert!(ops.resolve_for_injection(Op::Run, &[]).is_err());
        });
    }

    #[test]
    fn list_and_deploy_surface_backend_failures() {
        use arca_testkit::httpmock::{HttpMock, Response};
        let home = tempfile::tempdir().unwrap();
        let id_path = write_identity(home.path());
        let r = Ref::new("arca://x/y").unwrap();
        // every backend call 500s → list() and list_records() both
        // surface the vault's list error
        let mock = HttpMock::new(|_| Response::json(500, r#"{"error":"boom"}"#));
        let ops = ops_on_cf(mock.url(), "ARCA_TEST_OPS_TOKEN", &id_path);
        temp_env::with_var("ARCA_TEST_OPS_TOKEN", Some("fake-token"), || {
            assert!(ops.list(None).is_err());
            assert!(ops.list_records(None).is_err());
            // deploy: vault get fails before any sink work
            let mut config = Config::default();
            let mut v = vault_cfg("cf", Some(mock.url()));
            v.token_env = Some("ARCA_TEST_OPS_TOKEN".into());
            v.identity = Some(id_path.to_string_lossy().into_owned());
            config.vaults.insert("cloud".into(), v);
            config.envs.insert(Env::dev(), "cloud".into());
            let mut d = deploy_cfg("cf-secrets-store", Some("a"), Some("s"));
            d.token_env = Some("ARCA_TEST_OPS_TOKEN".into());
            d.base_url = Some(mock.url().to_string());
            config.deploy.insert("prod".into(), d);
            let ops = mk_ops(config);
            assert!(ops.deploy(&r, Some("prod")).is_err());
        });
        // vault get succeeds (real age vault) but the sink push 500s
        let (_h, mut ops) = ops_with_vault(&Env::dev(), &["arca://x/y"]);
        let mut d = deploy_cfg("cf-secrets-store", Some("a"), Some("s"));
        d.token_env = Some("ARCA_TEST_OPS_TOKEN".into());
        d.base_url = Some(mock.url().to_string());
        ops.config.deploy.insert("prod".into(), d);
        temp_env::with_var("ARCA_TEST_OPS_TOKEN", Some("fake-token"), || {
            assert!(ops.deploy(&r, Some("prod")).is_err());
        });
    }

    #[test]
    fn emit_writes_an_op_scoped_event() {
        let ops = mk_ops(Config::default());
        // stderr is the wire — this is a "does not panic" probe;
        // shape assertions live in audit.rs's tests
        ops.emit(Op::Get, Some("x/y"), Outcome::Denied);
        ops.emit(Op::List, None, Outcome::Error);
    }
}

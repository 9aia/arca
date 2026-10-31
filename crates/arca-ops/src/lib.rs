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
                    src.load().err().unwrap_or_else(|| "unreadable".into()),
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
                let c = cfg.ok_or_else(|| {
                    OpsError::Config(format!("vault {name:?} is kind cf but unconfigured"))
                })?;
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
        let (env, refs) = self.list(env)?;
        let vault = self.open_vault(&env)?;
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
            if !vaults.contains_key(env.as_str()) {
                vaults.insert(env.as_str().to_string(), self.open_vault(&env)?);
            }
            let vault = vaults
                .get(env.as_str())
                .ok_or_else(|| OpsError::Msg("vault open failed".into()))?;
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
                    // A route/env the loop above didn't cover — try
                    // once, and report its failure inline.
                    match self.open_vault(&env) {
                        Ok(v) => {
                            let got = v.get(&env, &r).map(|rec| rec.is_some()).unwrap_or(false);
                            opened.insert(vault_name.to_string(), v);
                            got
                        }
                        Err(e) => {
                            problems.push(format!(
                                "{var} → {rref}: env {env} vault {vault_name:?} not readable: {e}"
                            ));
                            false
                        }
                    }
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
    Ok(Actor::new("anon")?)
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
}

//! data — one `cuora-ops` handle per wire op.
//!
//! The driver is a *reader*: every `view`/`query` builds an
//! [`Ops`] exactly like the CLI does (`--config`/`--env`/`--actor`
//! become the `config`/`env`/`actor` request params, then the same
//! `CUORA_CONFIG` → `./cuora.toml` → `~/.config/cuora/cuora.toml`
//! search `cuora` verbs use) and answers from vault *metadata* —
//! refs, versions, timestamps, vault kinds and routes.
//!
//! **Secret values never leave this process.** `list_records`
//! decrypts records to reach their bookkeeping fields; the
//! `SecretRecord::value` is dropped the moment the row is built.
//! The `secret` view reads a record only to report `stored` +
//! bookkeeping — it never calls `Ops::get_in`, so no materialization
//! audit event fires and no value can render. There is no reveal
//! verb: `cuora get`/`cuora run` stay the only value edges.
//!
//! Degradation: a config/env/actor that fails [`Ops::load`], or a
//! vault that won't open, becomes an `{"error": …}` payload inside
//! an `ok:true` response — hosts render the failure inline, the
//! same contract `cuora-tui` used for its "vault not readable"
//! line.

use std::collections::BTreeSet;

use cuora_core::{config::CacheScope, Env, Ref};
use cuora_ops::{CheckReport, LoadOpts, Ops};
use serde_json::{json, Map, Value};

/// Everything a wire op needs, loaded once per request.
pub struct Ctx {
    /// `Ops::load` result — `Err` carries the load failure string.
    ops: Result<Ops, String>,
    /// `ops.check()` memoized — the check view's three tables share
    /// one probe round (remote vaults aren't free).
    check: Option<CheckReport>,
}

impl Ctx {
    /// Build the handle from request params. `config`/`env`/`actor`
    /// map to the CLI's `--config`/`--env`/`--actor`; everything
    /// else falls through to the usual env-var/default resolution.
    pub fn open(params: &Map<String, Value>) -> Self {
        let get = |k: &str| params.get(k).and_then(Value::as_str).map(str::to_string);
        let ops = Ops::load(LoadOpts {
            config_path: get("config"),
            env: get("env"),
            actor: get("actor"),
        })
        .map_err(|e| e.to_string());
        Ctx { ops, check: None }
    }

    /// The loaded ops, or the load failure.
    pub fn ops(&self) -> Result<&Ops, String> {
        self.ops.as_ref().map_err(|e| e.clone())
    }

    /// `cuora check`'s report, computed once per request.
    pub fn check_report(&mut self) -> Result<&CheckReport, String> {
        if self.check.is_none() {
            let report = self.ops()?.check();
            self.check = Some(report);
        }
        match self.check.as_ref() {
            Some(r) => Ok(r),
            None => Err("check report missing".to_string()),
        }
    }

    /// Every env worth a tab: well-known first, then `[envs]`
    /// customs sorted, then the selected env if unlisted — the same
    /// order `cuora-tui` built its tabs.
    pub fn envs(&self) -> Vec<Env> {
        let mut names: Vec<Env> = Env::WELL_KNOWN
            .iter()
            .filter_map(|n| Env::new(n).ok())
            .collect();
        if let Ok(ops) = self.ops() {
            let mut customs: Vec<Env> = ops
                .config()
                .envs
                .keys()
                .filter(|e| !e.is_well_known())
                .cloned()
                .collect();
            customs.sort();
            names.extend(customs);
            if !names.iter().any(|e| e == ops.env()) {
                names.push(ops.env().clone());
            }
        }
        names
    }

    /// Vault names the config knows: `[vaults]` keys ∪ every env
    /// route's target ∪ the implicit `local`.
    pub fn vault_names(&self) -> Vec<String> {
        let mut names = BTreeSet::new();
        names.insert(cuora_core::config::LOCAL_VAULT.to_string());
        if let Ok(ops) = self.ops() {
            names.extend(ops.config().vaults.keys().cloned());
            for route in ops.config().envs.values() {
                names.insert(route.vault().to_string());
            }
            names.insert(ops.config().vault_for(ops.env()).to_string());
        }
        names.into_iter().collect()
    }

    /// Envs routing to `vault` (for the vaults table's envs column).
    pub fn envs_for(&self, vault: &str) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(ops) = self.ops() {
            for env in self.envs() {
                if ops.config().vault_for(&env) == vault {
                    out.push(env.as_str().to_string());
                }
            }
        }
        out
    }
}

/// `{"error": …}` — the degradation marker every renderer knows.
pub fn error_payload(msg: impl Into<String>) -> Value {
    json!({"error": msg.into()})
}

/// The data payload for one query name. Unknown names and failed
/// loads degrade to `{"error": …}` — never a wire error.
pub fn query_data(ctx: &mut Ctx, name: &str, params: &Map<String, Value>) -> Value {
    match name {
        "summary" => summary(ctx),
        "vaults" => vaults_rows(ctx),
        "envs" => envs_rows(ctx),
        "declared" => declared_rows(ctx),
        "refs" => refs_rows(ctx, params),
        "tree" => tree_payload(ctx),
        "secret" => secret_payload(ctx, params),
        "check_info" => check_info(ctx),
        "check_vaults" => check_vaults_rows(ctx),
        "check_secrets" => check_secrets_rows(ctx),
        "check_problems" => check_problems_rows(ctx),
        other => error_payload(format!("unknown query '{other}'")),
    }
}

// ---------------------------------------------------------------- rows

/// `path`/`url` for a vault config, or an honest "default" marker.
fn location(cfg: Option<&cuora_core::VaultConfig>) -> String {
    match cfg {
        Some(c) => c
            .path
            .clone()
            .or_else(|| c.url.clone())
            .unwrap_or_else(|| "implicit defaults".to_string()),
        None => "implicit defaults".to_string(),
    }
}

/// `cache` scope rendered for a table cell.
fn cache_desc(cfg: Option<&cuora_core::VaultConfig>) -> String {
    match cfg.and_then(|c| c.cache.as_ref()) {
        None => "off".to_string(),
        Some(CacheScope::All(true)) => "all envs".to_string(),
        Some(CacheScope::All(false)) => "off".to_string(),
        Some(CacheScope::One(e)) => e.as_str().to_string(),
        Some(CacheScope::Envs(es)) => es.iter().map(|e| e.as_str()).collect::<Vec<_>>().join(", "),
    }
}

/// RFC 3339 like the TUI's detail pane; "—" when absent.
fn ts(t: Option<time::OffsetDateTime>) -> String {
    match t {
        Some(t) => t
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| t.to_string()),
        None => "—".to_string(),
    }
}

/// `summary` — the statusbar/dashboard kv: where config came from,
/// the env context, and surface counts. Pure config — no vault I/O,
/// safe to poll.
fn summary(ctx: &Ctx) -> Value {
    let ops = match ctx.ops() {
        Ok(o) => o,
        Err(e) => return error_payload(e),
    };
    let cfg = ops.config();
    let mut out = Map::new();
    out.insert(
        "config".into(),
        json!(ops
            .config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "defaults (no cuora.toml found)".into())),
    );
    out.insert("env".into(), json!(ops.env().as_str()));
    out.insert("actor".into(), json!(ops.actor().as_str()));
    out.insert("vault".into(), json!(cfg.vault_for(ops.env())));
    out.insert("vaults".into(), json!(ctx.vault_names().len()));
    out.insert("envs".into(), json!(ctx.envs().len()));
    out.insert("declared_secrets".into(), json!(cfg.secrets.len()));
    out.insert(
        "identity_backend".into(),
        json!(cfg
            .identity
            .as_ref()
            .map(|i| format!("{:?}", i.backend).to_lowercase())
            .unwrap_or_else(|| "keyring".into())),
    );
    Value::Object(out)
}

/// `vaults` — every known vault: kind, location, cache scope, the
/// envs routed to it, and whether it's a configured `[vaults]`
/// entry or an implicit default. Config-only — no opens, no probes
/// (the `check` view owns live readability).
fn vaults_rows(ctx: &Ctx) -> Value {
    let ops = match ctx.ops() {
        Ok(o) => o,
        Err(e) => return error_payload(e),
    };
    let rows: Vec<Value> = ctx
        .vault_names()
        .into_iter()
        .map(|name| {
            let cfg = ops.config().vault_config(&name);
            let configured = cfg.is_some() || name == cuora_core::config::LOCAL_VAULT;
            json!({
                "name": name,
                "kind": cfg.map(|c| c.kind.clone()).unwrap_or_else(|| "age".into()),
                "location": location(cfg),
                "cache": cache_desc(cfg),
                "envs": ctx.envs_for(&name).join(", "),
                "configured": if configured { "yes" } else { "routed only" },
            })
        })
        .collect();
    Value::Array(rows)
}

/// `envs` — the env → vault routing table (the TUI's tab strip).
fn envs_rows(ctx: &Ctx) -> Value {
    let ops = match ctx.ops() {
        Ok(o) => o,
        Err(e) => return error_payload(e),
    };
    let rows: Vec<Value> = ctx
        .envs()
        .into_iter()
        .map(|env| {
            let vault = ops.config().vault_for(&env).to_string();
            let cfg = ops.config().vault_config(&vault);
            json!({
                "env": env.as_str(),
                "well_known": if env.is_well_known() { "yes" } else { "" },
                "vault": vault,
                "kind": cfg.map(|c| c.kind.clone()).unwrap_or_else(|| "age".into()),
                "location": location(cfg),
            })
        })
        .collect();
    Value::Array(rows)
}

/// `declared` — the `[secrets]` injection surface: env-var name →
/// ref, resolved env + vault. Names and refs only.
fn declared_rows(ctx: &Ctx) -> Value {
    let ops = match ctx.ops() {
        Ok(o) => o,
        Err(e) => return error_payload(e),
    };
    let rows: Vec<Value> = ops
        .config()
        .secrets
        .iter()
        .map(|(var, rref)| {
            let (env, stripped) = ops.resolve(rref);
            json!({
                "var": var,
                "ref": rref.to_string(),
                "resolved_ref": stripped.to_string(),
                "env": env.as_str(),
                "vault": ops.config().vault_for(&env),
            })
        })
        .collect();
    Value::Array(rows)
}

/// `refs` — the TUI's core list: refs + bookkeeping for one env
/// (`--param env=` picks it; default is the selected env). Values
/// are dropped inside `list_records` — rows carry metadata only.
fn refs_rows(ctx: &Ctx, _params: &Map<String, Value>) -> Value {
    let ops = match ctx.ops() {
        Ok(o) => o,
        Err(e) => return error_payload(e),
    };
    let env = ops.env().clone();
    match ops.list_records(Some(&env)) {
        Ok((_, records)) => {
            let vault = ops.config().vault_for(&env).to_string();
            Value::Array(
                records
                    .into_iter()
                    .map(|(rref, rec)| {
                        // `rec.value` is a redacting Secret and is
                        // never read — bookkeeping only.
                        json!({
                            "ref": rref.to_string(),
                            "version": rec.version,
                            "created": ts(Some(rec.created_at)),
                            "rotated": ts(rec.rotated_at),
                            "vault": vault,
                        })
                    })
                    .collect(),
            )
        }
        Err(e) => error_payload(format!("env {}: {e}", env.as_str())),
    }
}

/// `tree` — env → refs hierarchy, the TUI's env tabs as a tree.
/// Uses `list` (paths only, no per-record decrypt) so the browse
/// stays cheap; per-env failures become the node's detail text.
fn tree_payload(ctx: &Ctx) -> Value {
    let mut items = Vec::new();
    for env in ctx.envs() {
        match ctx.ops().and_then(|ops| {
            ops.list(Some(&env))
                .map(|(_, refs)| refs)
                .map_err(|e| e.to_string())
        }) {
            Ok(refs) => {
                let children: Vec<pavo_protocol::TreeItem> = refs
                    .iter()
                    .map(|r| {
                        let mut item = pavo_driver_kit::tree_item(r.path());
                        item.id = Some(r.to_string());
                        item
                    })
                    .collect();
                let mut env_item = pavo_driver_kit::tree_item(env.as_str());
                env_item.id = Some(env.as_str().to_string());
                env_item.detail = Some(format!("{} refs", children.len()));
                env_item.children = children;
                items.push(env_item);
            }
            Err(e) => {
                let mut item = pavo_driver_kit::tree_item(env.as_str());
                item.id = Some(env.as_str().to_string());
                item.detail = Some(format!("unreadable: {e}"));
                items.push(item);
            }
        }
    }
    json!({"items": items})
}

/// `secret` — one ref's metadata pane (the TUI detail pane minus
/// the reveal). `ref` param is required (`cuora://…` or legacy
/// `arca://…`); env pins resolve through `Ops::resolve` like the
/// CLI. Reads the record for bookkeeping only — the value is never
/// exposed and no materialization audit fires.
fn secret_payload(ctx: &Ctx, params: &Map<String, Value>) -> Value {
    let raw = match params.get("ref").and_then(Value::as_str) {
        Some(r) => r,
        None => {
            return error_payload(
                "missing required param 'ref' (e.g. --param ref=cuora://billing/stripe)",
            )
        }
    };
    let rref = match Ref::new(raw) {
        Ok(r) => r,
        Err(e) => return error_payload(format!("bad ref '{raw}': {e}")),
    };
    let ops = match ctx.ops() {
        Ok(o) => o,
        Err(e) => return error_payload(e),
    };
    let (env, stripped) = ops.resolve(&rref);
    let vault_name = ops.config().vault_for(&env).to_string();
    let mut out = Map::new();
    out.insert("ref".into(), json!(rref.to_string()));
    out.insert("env".into(), json!(env.as_str()));
    out.insert("vault".into(), json!(vault_name));
    let vault = match ops.open_vault(&env) {
        Ok(v) => v,
        Err(e) => return error_payload(format!("env {}: {e}", env.as_str())),
    };
    match vault.get(&env, &stripped) {
        Ok(Some(rec)) => {
            out.insert("stored".into(), json!("yes"));
            out.insert("version".into(), json!(rec.version));
            out.insert("created".into(), json!(ts(Some(rec.created_at))));
            out.insert("rotated".into(), json!(ts(rec.rotated_at)));
        }
        Ok(None) => {
            out.insert("stored".into(), json!("no"));
        }
        Err(e) => return error_payload(format!("env {}: {e}", env.as_str())),
    }
    Value::Object(out)
}

// ---------------------------------------------------------------- check

/// `check_info` — the kv header for the check view.
fn check_info(ctx: &mut Ctx) -> Value {
    let report = match ctx.check_report() {
        Ok(r) => r,
        Err(e) => return error_payload(e),
    };
    let ok = report.vaults.iter().filter(|v| v.ok).count();
    let resolved = report.secrets.iter().filter(|s| s.resolves).count();
    json!({
        "config": report.config_path.as_ref().map(|p| p.display().to_string())
            .unwrap_or_else(|| "defaults".into()),
        "env": report.env.as_str(),
        "vault": report.vault,
        "vaults_ok": format!("{ok}/{}", report.vaults.len()),
        "secrets_resolved": format!("{resolved}/{}", report.secrets.len()),
        "problems": report.problems.len(),
    })
}

/// `check_vaults` — one row per probed vault (`cuora check`'s
/// readability table).
fn check_vaults_rows(ctx: &mut Ctx) -> Value {
    let report = match ctx.check_report() {
        Ok(r) => r,
        Err(e) => return error_payload(e),
    };
    Value::Array(
        report
            .vaults
            .iter()
            .map(|v| {
                json!({
                    "name": v.name,
                    "kind": v.kind,
                    "path": v.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
                    "ok": if v.ok { "yes" } else { "no" },
                    "error": v.error.clone().unwrap_or_default(),
                })
            })
            .collect(),
    )
}

/// `check_secrets` — the declared `[secrets]` surface with
/// resolution status.
fn check_secrets_rows(ctx: &mut Ctx) -> Value {
    let report = match ctx.check_report() {
        Ok(r) => r,
        Err(e) => return error_payload(e),
    };
    Value::Array(
        report
            .secrets
            .iter()
            .map(|s| {
                json!({
                    "var": s.var,
                    "ref": s.rref,
                    "env": s.env,
                    "resolves": if s.resolves { "yes" } else { "no" },
                })
            })
            .collect(),
    )
}

/// `check_problems` — the findings list, one row each.
fn check_problems_rows(ctx: &mut Ctx) -> Value {
    let report = match ctx.check_report() {
        Ok(r) => r,
        Err(e) => return error_payload(e),
    };
    Value::Array(
        report
            .problems
            .iter()
            .map(|p| json!({"problem": p}))
            .collect(),
    )
}

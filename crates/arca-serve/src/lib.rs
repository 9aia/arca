//! arca-serve — `arca serve`: the ndjson request/response surface
//! orchestrators (peregrin) embed (P-005).
//!
//! One JSON request per **stdin** line, one JSON response per
//! **stdout** line; the audit wire stays on stderr, unchanged. A
//! thin front over `arca-ops` like `arca-tui` — every vault access
//! goes through [`Ops`], so env-pin resolution, `[envs]` routing,
//! and the audit events are the same ones the CLI emits.
//!
//! Protocol `arca-serve/1`:
//!
//! ```json
//! {"id": 1, "op": "get", "ref": "arca://billing/stripe",
//!  "env": "prod", "actor": "agent:peregrin", "attestation": "…"}
//! ```
//!
//! → `{"id": 1, "ok": true, "result": {…}}` or
//! `{"id": 1, "ok": false, "error": {"code": "…", "message": "…"}}`.
//! `id` is echoed verbatim — any JSON value, absent → `null`.
//!
//! Ops: `get`/`put`/`rm`/`rotate` (the vault-touching verbs — each
//! consults the configured `[serve.pdp]` first), `list`, `check`,
//! `manifest` (this protocol's self-description), and `shutdown`
//! (clean exit). `run`/`env`/`deploy`/`init`/`ui` stay single-shot
//! CLI verbs — serve covers the port peregrin needs in-process.
//!
//! Policy: requests carry *advisory* actor claims — `actor` is the
//! attribution string audit records, `attestation` an opaque blob
//! passed to the PDP verbatim. arca verifies neither; signing is
//! the PDP's side (P-005 is the boundary contract, not the
//! enforcement). A configured PDP that denies — or that can't be
//! reached under the default fail-closed posture — turns into a
//! `denied`/`pdp_unreachable` error, a `denied` audit event, and
//! no vault access.
//!
//! Values cross this wire deliberately: `get` returns them,
//! `put`/`rotate` take them — the parent owns the channel exactly
//! like it owns `arca get`'s stdout.

mod pdp;

use std::io::{self, BufRead, Write};

use arca_core::{Actor, Env, Op, Ref, Secret};
use arca_ops::{Ops, OpsError, Outcome, PolicyVerdict};
use arca_vault::{SecretRecord, VaultError};
use serde::Deserialize;

pub use pdp::{Decision, ExecPdp, PdpRequest};

/// Serve the ndjson loop on `reader`/`writer` against the loaded
/// [`Ops`] — the `arca serve` verb's whole job. Runs until EOF or
/// a `shutdown` request. A malformed line is a `bad_request`
/// response, never a crash and never the end of the loop; only a
/// real stdout/reader I/O failure propagates.
pub fn run(ops: &Ops, reader: &mut impl BufRead, writer: &mut impl Write) -> io::Result<()> {
    let pdp = ops
        .config()
        .serve
        .as_ref()
        .and_then(|s| s.pdp.as_ref())
        .map(ExecPdp::new);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            // EOF — the parent closed the wire; clean exit.
            Ok(0) => return Ok(()),
            Ok(_) => {}
            // Not UTF-8 — a bad line like any other; keep serving.
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                write_reply(
                    writer,
                    &err_reply(
                        serde_json::Value::Null,
                        "bad_request",
                        "line is not valid UTF-8",
                    ),
                )?;
                continue;
            }
            Err(e) => return Err(e),
        }
        if line.trim().is_empty() {
            continue;
        }
        let reply = dispatch(ops, pdp.as_ref(), &line);
        let done = reply.shutdown;
        write_reply(writer, &reply)?;
        if done {
            return Ok(());
        }
    }
}

/// One incoming line, validated loosely — field-level errors are
/// `bad_request`s at dispatch, not parse failures here.
#[derive(Debug, Deserialize)]
struct Request {
    /// Echo handle — any JSON value, echoed verbatim.
    #[serde(default)]
    id: serde_json::Value,
    /// The verb name.
    op: String,
    /// `arca://` ref (`get`/`put`/`rm`/`rotate`).
    #[serde(default, rename = "ref")]
    rref: Option<String>,
    /// Per-request env override (else the loaded context; an
    /// `arca://<env>/…` pin still wins).
    #[serde(default)]
    env: Option<String>,
    /// The new value (`put`/`rotate`) — on the wire, not in args.
    #[serde(default)]
    value: Option<String>,
    /// Claimed actor id — advisory attribution, recorded on audit.
    #[serde(default)]
    actor: Option<String>,
    /// Opaque identity claim for the PDP — passthrough, never
    /// interpreted.
    #[serde(default)]
    attestation: Option<String>,
}

/// One outgoing line.
struct Reply {
    id: serde_json::Value,
    ok: bool,
    result: Option<serde_json::Value>,
    /// The PDP verdict, when one was consulted — the requester
    /// sees the same `policy:` the audit event records.
    policy: Option<PolicyVerdict>,
    code: Option<&'static str>,
    message: Option<String>,
    /// Set on `shutdown` — respond, then break the loop.
    shutdown: bool,
}

fn ok_reply(id: serde_json::Value, result: serde_json::Value) -> Reply {
    Reply {
        id,
        ok: true,
        result: Some(result),
        policy: None,
        code: None,
        message: None,
        shutdown: false,
    }
}

fn err_reply(id: serde_json::Value, code: &'static str, message: impl Into<String>) -> Reply {
    Reply {
        id,
        ok: false,
        result: None,
        policy: None,
        code: Some(code),
        message: Some(message.into()),
        shutdown: false,
    }
}

fn write_reply(writer: &mut impl Write, reply: &Reply) -> io::Result<()> {
    let mut v = serde_json::json!({
        "id": reply.id,
        "ok": reply.ok,
    });
    if let Some(r) = &reply.result {
        v["result"] = r.clone();
    }
    if let Some(p) = &reply.policy {
        v["policy"] = serde_json::to_value(p).map_err(io::Error::other)?;
    }
    if let (Some(code), Some(message)) = (reply.code, &reply.message) {
        v["error"] = serde_json::json!({"code": code, "message": message});
    }
    // A serde_json::Value always serializes — the expect documents
    // the invariant rather than guarding a reachable panic.
    #[allow(clippy::unwrap_used)]
    let line = serde_json::to_string(&v).expect("reply serializes");
    writeln!(writer, "{line}")
}

/// Route one request line — everything that can fail per-request
/// folds into the reply; nothing here panics.
fn dispatch(ops: &Ops, pdp: Option<&ExecPdp>, line: &str) -> Reply {
    let req: Request = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            // Try to salvage just the id so the caller can
            // correlate; a totally broken line gets null.
            let id = serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|v| v.get("id").cloned())
                .unwrap_or(serde_json::Value::Null);
            return err_reply(id, "bad_request", format!("malformed request: {e}"));
        }
    };
    let id = req.id.clone();
    // Per-request claims, validated up front — bad values are
    // bad_request, not silent fallback.
    let actor = match req.actor.as_deref().map(Actor::new) {
        Some(Ok(a)) => Some(a),
        Some(Err(e)) => return err_reply(id, "bad_request", e.to_string()),
        None => None,
    };
    let env = match req.env.as_deref().map(Env::new) {
        Some(Ok(e)) => Some(e),
        Some(Err(e)) => return err_reply(id, "bad_request", e.to_string()),
        None => None,
    };

    match req.op.as_str() {
        "manifest" => ok_reply(id, manifest()),
        "shutdown" => {
            let mut r = ok_reply(id, serde_json::json!({"shutdown": true}));
            r.shutdown = true;
            let scoped = ops.scoped(actor, env, None);
            scoped.emit(Op::Serve, None, Outcome::Ok);
            r
        }
        "list" => refless(ops, &id, actor, env, Op::List, |s| {
            let (env, _) = s.list(None)?;
            let (_, records) = s.list_records(None)?;
            let arr: Vec<serde_json::Value> = records
                .iter()
                .map(|(r, rec)| record_json(&env, r, rec))
                .collect();
            s.emit(Op::List, None, Outcome::Ok);
            Ok(serde_json::json!({"env": env.as_str(), "refs": arr}))
        }),
        "check" => refless(ops, &id, actor, env, Op::Check, |s| {
            let report = s.check();
            s.emit(Op::Check, None, Outcome::Ok);
            Ok(check_json(&report))
        }),
        "get" | "put" | "rm" | "rotate" => {
            vault_op(ops, pdp, &id, actor, env, req.attestation.as_deref(), &req)
        }
        other => {
            let msg = match other.parse::<Op>() {
                // A real arca verb that serve deliberately doesn't
                // cover — name the CLI path rather than "unknown".
                Ok(op) => format!("op '{op}' is not served — use the single-shot CLI"),
                Err(_) => format!("unknown op {other:?}"),
            };
            err_reply(id, "unknown_op", msg)
        }
    }
}

/// The ops that don't touch a ref — guardrails still apply, the
/// PDP does not (nothing is materialized).
fn refless(
    ops: &Ops,
    id: &serde_json::Value,
    actor: Option<Actor>,
    env: Option<Env>,
    op: Op,
    f: impl FnOnce(&Ops) -> Result<serde_json::Value, OpsError>,
) -> Reply {
    let scoped = ops.scoped(actor, env, None);
    if let Some(reply) = guardrail_reply(&scoped, id, op, None) {
        return reply;
    }
    match f(&scoped) {
        Ok(result) => ok_reply(id.clone(), result),
        Err(e) => {
            scoped.emit(op, None, Outcome::Error);
            err_reply(id.clone(), "op_error", e.to_string())
        }
    }
}

/// Advisory guardrails — same check the CLI applies. A denial is
/// a `denied` error plus the usual denied audit event.
fn guardrail_reply(
    scoped: &Ops,
    id: &serde_json::Value,
    op: Op,
    rref: Option<&str>,
) -> Option<Reply> {
    let g = scoped.config().guardrails.as_ref()?;
    if g.allows(op) {
        return None;
    }
    let norm = rref.and_then(|s| s.parse::<Ref>().ok().map(|r| scoped.resolve(&r).1.path()));
    scoped.emit(op, norm.as_deref().or(rref), Outcome::Denied);
    Some(err_reply(
        id.clone(),
        "denied",
        format!("op '{op}' denied by guardrails (advisory)"),
    ))
}

/// The vault-touching ops — parse, guardrails, resolve, PDP,
/// execute. The PDP consult happens *before* the vault is opened;
/// its verdict attaches to the scoped handle so every audit event
/// the op emits carries the policy context.
fn vault_op(
    ops: &Ops,
    pdp: Option<&ExecPdp>,
    id: &serde_json::Value,
    actor: Option<Actor>,
    env: Option<Env>,
    attestation: Option<&str>,
    req: &Request,
) -> Reply {
    let mut scoped = ops.scoped(actor, env, None);
    let raw = match req.rref.as_deref() {
        Some(r) => r,
        None => {
            return err_reply(
                id.clone(),
                "bad_request",
                format!("op '{}' needs a ref", req.op),
            )
        }
    };
    let parsed = match raw.parse::<Ref>() {
        Ok(r) => r,
        Err(e) => return err_reply(id.clone(), "bad_request", e.to_string()),
    };
    let op = match req.op.parse::<Op>() {
        Ok(o) => o,
        // The dispatch table only routes get/put/rm/rotate here —
        // the parse is total by construction.
        Err(e) => return err_reply(id.clone(), "bad_request", e.to_string()),
    };
    if let Some(reply) = guardrail_reply(&scoped, id, op, Some(raw)) {
        return reply;
    }
    let (resolved_env, resolved_ref) = scoped.resolve(&parsed);

    // The policy consult — vault-touching ops only, before any
    // vault open. The verdict attaches to the scoped handle so
    // every audit event the op emits carries the policy context,
    // and echoes back on the reply's `policy` field.
    let mut verdict: Option<PolicyVerdict> = None;
    if let Some(pdp) = pdp {
        match pdp.decide(&PdpRequest {
            actor: scoped.actor().as_str(),
            attestation,
            op: op.as_str(),
            env: resolved_env.as_str(),
            rref: &resolved_ref.path(),
        }) {
            Decision::Allow {
                reason,
                obligations,
            } => {
                verdict = Some(PolicyVerdict {
                    allow: true,
                    reason,
                    obligations,
                });
            }
            Decision::Deny { reason } => {
                verdict = Some(PolicyVerdict {
                    allow: false,
                    reason: reason.clone(),
                    obligations: None,
                });
                scoped.set_policy(verdict.clone());
                scoped.emit(op, Some(&resolved_ref.path()), Outcome::Denied);
                let mut reply = err_reply(
                    id.clone(),
                    "denied",
                    reason
                        .map(|r| format!("denied by policy: {r}"))
                        .unwrap_or_else(|| "denied by policy".to_string()),
                );
                reply.policy = verdict;
                return reply;
            }
            Decision::Unreachable(why) => {
                if !pdp.allow_on_error() {
                    // Fail closed — an unreachable policy engine
                    // must not silently mean "allowed".
                    verdict = Some(PolicyVerdict {
                        allow: false,
                        reason: Some(why.clone()),
                        obligations: None,
                    });
                    scoped.set_policy(verdict.clone());
                    scoped.emit(op, Some(&resolved_ref.path()), Outcome::Denied);
                    let mut reply = err_reply(
                        id.clone(),
                        "pdp_unreachable",
                        format!("policy decision point unreachable: {why}"),
                    );
                    reply.policy = verdict;
                    return reply;
                }
                // allow_on_error — the op runs and the audit event
                // records the degraded verdict.
                verdict = Some(PolicyVerdict {
                    allow: true,
                    reason: Some(format!(
                        "pdp unreachable — allowed by allow_on_error: {why}"
                    )),
                    obligations: None,
                });
            }
        }
    }
    scoped.set_policy(verdict.clone());

    match execute(&scoped, op, &parsed, &resolved_env, &resolved_ref, req) {
        Ok(v) => {
            let mut reply = ok_reply(id.clone(), v);
            reply.policy = verdict;
            reply
        }
        Err((code, msg)) => {
            scoped.emit(op, Some(&resolved_ref.path()), Outcome::Error);
            let mut reply = err_reply(id.clone(), code, msg);
            reply.policy = verdict;
            reply
        }
    }
}

/// Dispatch the four vault ops on the *unstripped* parsed ref —
/// `scoped.resolve` already decided env for the PDP context, but
/// the ops calls resolve once themselves, so the pin semantics
/// stay single-application (a stripped ref could re-pin wrongly).
/// `env`/`r` are only for response shaping.
fn execute(
    scoped: &Ops,
    op: Op,
    parsed: &Ref,
    env: &Env,
    r: &Ref,
    req: &Request,
) -> Result<serde_json::Value, (&'static str, String)> {
    match op {
        Op::Get => match scoped.get(parsed) {
            Ok(Some(rec)) => {
                let mut v = record_json(env, r, &rec);
                v["value"] = rec.value.expose().into();
                Ok(v)
            }
            Ok(None) => Err(("not_found", format!("{parsed} is not stored in env {env}"))),
            Err(e) => Err(ops_err(&e)),
        },
        Op::Put => {
            let secret = req_secret(req)?;
            match scoped.put(parsed, secret) {
                Ok(rec) => Ok(record_json(env, r, &rec)),
                Err(e) => Err(ops_err(&e)),
            }
        }
        Op::Rm => match scoped.rm(parsed) {
            Ok(true) => Ok(serde_json::json!({
                "removed": true,
                "ref": parsed.to_string(),
                "env": env.as_str(),
            })),
            Ok(false) => Err(("not_found", format!("{parsed} is not stored in env {env}"))),
            Err(e) => Err(ops_err(&e)),
        },
        Op::Rotate => {
            let secret = req_secret(req)?;
            match scoped.rotate(parsed, secret) {
                Ok(rec) => Ok(record_json(env, r, &rec)),
                Err(e) => Err(ops_err(&e)),
            }
        }
        // vault_op only routes the four ref ops here.
        other => Err(("op_error", format!("op {other} is not a vault op"))),
    }
}

/// `put`/`rotate` need a non-empty `value` — same rule as the
/// CLI's empty-secret refusal.
fn req_secret(req: &Request) -> Result<Secret, (&'static str, String)> {
    match req.value.as_deref() {
        Some(v) if !v.is_empty() => Ok(Secret::new(v)),
        _ => Err((
            "bad_request",
            format!("op '{}' needs a non-empty value", req.op),
        )),
    }
}

/// Map an [`OpsError`] to a wire `(code, message)`.
fn ops_err(e: &OpsError) -> (&'static str, String) {
    match e {
        OpsError::Vault(VaultError::NotFound { .. }) | OpsError::NotStored { .. } => {
            ("not_found", e.to_string())
        }
        OpsError::Ref(_) | OpsError::Env(_) | OpsError::Actor(_) => ("bad_request", e.to_string()),
        _ => ("op_error", e.to_string()),
    }
}

/// The canonical record shape — same fields as `record_json` in
/// the CLI: resolved ref + env + record metadata. `value` is added
/// by `get` only.
fn record_json(env: &Env, r: &Ref, rec: &SecretRecord) -> serde_json::Value {
    serde_json::json!({
        "ref": r.to_string(),
        "env": env.as_str(),
        "version": rec.version,
        "created_at": rec.created_at.to_string(),
        "rotated_at": rec.rotated_at.map(|t| t.to_string()),
    })
}

/// The `check` result — mirrors `arca check --json`, plus `ok`.
fn check_json(report: &arca_ops::CheckReport) -> serde_json::Value {
    let secrets: serde_json::Map<String, serde_json::Value> = report
        .secrets
        .iter()
        .map(|s| {
            (
                s.var.clone(),
                serde_json::json!({
                    "ref": s.rref,
                    "env": s.env,
                    "resolves": s.resolves,
                }),
            )
        })
        .collect();
    let vaults: Vec<serde_json::Value> = report
        .vaults
        .iter()
        .map(|v| {
            serde_json::json!({
                "name": v.name,
                "kind": v.kind,
                "path": v.path,
                "ok": v.ok,
                "error": v.error,
            })
        })
        .collect();
    let guardrails = report.guardrails.as_ref().map(|g| {
        serde_json::json!({
            "default": match g.default {
                arca_core::GuardDefault::Allow => "allow",
                arca_core::GuardDefault::Deny => "deny",
            },
            "allow": g.allow.iter().map(|o| o.as_str()).collect::<Vec<_>>(),
            "deny": g.deny.iter().map(|o| o.as_str()).collect::<Vec<_>>(),
            "deny_adhoc": g.deny_adhoc,
        })
    });
    serde_json::json!({
        "ok": report.problems.is_empty(),
        "config": report.config_path,
        "env": report.env.as_str(),
        "vault": report.vault,
        "vaults": vaults,
        "secrets": secrets,
        "guardrails": guardrails,
        "errors": report.problems,
    })
}

/// The serve-protocol self-description — what `{"op":"manifest"}`
/// answers. Keep honest: this is what the loop actually does.
fn manifest() -> serde_json::Value {
    serde_json::json!({
        "name": "arca serve",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": "arca-serve/1",
        "wire": "one JSON request per stdin line → one JSON response per stdout line; audit ndjson stays on stderr",
        "ops": {
            "get": "value of a ref — consults the PDP; errors not_found when absent",
            "put": "store a secret — needs {ref, value}; consults the PDP",
            "rm": "remove a secret — needs {ref}; consults the PDP",
            "rotate": "new value + version bump — needs {ref, value}; consults the PDP",
            "list": "refs + record metadata in an env — no PDP consult",
            "check": "config + declared-surface report — no PDP consult",
            "manifest": "this document — no config needed per-request",
            "shutdown": "reply then exit cleanly; EOF exits too",
        },
        "request": {
            "id": "any JSON value — echoed verbatim (absent → null)",
            "op": "one of the ops above",
            "ref": "arca://ns/name — env pin honored (get/put/rm/rotate)",
            "env": "per-request env override — else the loaded context",
            "value": "plaintext — put/rotate only; on the wire, never in args",
            "actor": "claimed actor id — advisory, recorded on audit events",
            "attestation": "opaque identity claim — passed to the PDP verbatim, never interpreted",
        },
        "response": {
            "ok": {"id": "echoed", "ok": true, "result": "…", "policy": "{allow, reason?, obligations?} — present when a PDP was consulted"},
            "error": {"id": "echoed|null", "ok": false, "error": {"code": "…", "message": "…"}},
        },
        "error_codes": {
            "bad_request": "malformed line, missing ref/value, invalid ref/env/actor",
            "unknown_op": "op not in the table",
            "denied": "guardrail or PDP refusal",
            "pdp_unreachable": "PDP failed and allow_on_error is false (fail-closed)",
            "not_found": "ref not stored in that env",
            "op_error": "vault/config/io failure",
        },
        "pdp": {
            "config": "[serve.pdp] exec = \"cmd…\" — run via sh -c once per vault-touching op; request JSON on stdin, verdict JSON on stdout",
            "request": {"actor": "claimed id", "attestation": "opaque claim", "op": "…", "env": "resolved env", "ref": "resolved path (pin stripped)"},
            "verdict": {"allow": "bool — required", "reason": "string?", "obligations": "[string]?"},
            "unreachable": "spawn failure, non-zero exit, or unparseable verdict → fail CLOSED unless [serve.pdp] allow_on_error = true",
        },
        "notes": [
            "actor/attestation are advisory claims — arca verifies neither; real signing is the PDP's side",
            "values cross this wire deliberately: get returns them, put/rotate take them — the parent owns the channel",
            "guardrails still apply (advisory); arca://<env>/… pins still resolve",
            "run/env/deploy/init/ui stay single-shot CLI verbs — serve covers the Vault port peregrin embeds",
        ],
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use arca_identity_file::FileIdentity;
    use arca_ops::LoadOpts;
    use arca_vault_age::AgeVault;

    /// An `Ops` loaded from a real `arca.toml` in a tempdir — the
    /// `local` vault is a fresh age vault, the identity a real
    /// keyfile. `extra` is appended to the config verbatim
    /// (`[serve.pdp]`, `[guardrails]`, …).
    fn fixture(extra: &str) -> (tempfile::TempDir, Ops) {
        let home = tempfile::tempdir().unwrap();
        let id_path = home.path().join("identity.txt");
        let vault_path = home.path().join("vault.age");
        let (identity, _) = arca_identity::ensure(&FileIdentity::new(&id_path)).unwrap();
        AgeVault::open(&vault_path, identity, &[]).unwrap();
        let toml_path = home.path().join("arca.toml");
        std::fs::write(
            &toml_path,
            format!(
                "[vaults.local]\nkind = \"age\"\npath = {:?}\nidentity = {:?}\n\n{extra}",
                vault_path.display().to_string(),
                id_path.display().to_string(),
            ),
        )
        .unwrap();
        let ops = Ops::load(LoadOpts {
            config_path: Some(toml_path.display().to_string()),
            env: Some("dev".into()),
            actor: Some("test-actor".into()),
        })
        .unwrap();
        (home, ops)
    }

    /// A tiny PDP script — writes `body` to stdout, or `cat`s the
    /// request into `capture` first. Exec driver runs `sh -c` so a
    /// bare path works as the command.
    fn pdp_script(home: &tempfile::TempDir, capture: bool, body: &str) -> String {
        let path = home.path().join("pdp.sh");
        let first = if capture {
            format!("cat > {}\n", home.path().join("pdp-req.json").display())
        } else {
            "cat >/dev/null\n".to_string()
        };
        std::fs::write(&path, format!("#!/bin/sh\n{first}{body}\n")).unwrap();
        // exec runs `sh -c <cmd>` — `sh <path>` needs no chmod.
        format!("sh {}", path.display())
    }

    /// Run the loop over `input` bytes, return the decoded replies.
    fn serve(ops: &Ops, input: &[u8]) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        run(ops, &mut &input[..], &mut out).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn replies(ops: &Ops, input: &str) -> Vec<serde_json::Value> {
        serve(ops, input.as_bytes())
    }

    #[test]
    fn malformed_lines_are_bad_request_and_the_loop_continues() {
        let (_home, ops) = fixture("");
        let input = concat!(
            "not json at all\n",
            "\n",             // blank — skipped
            "   \n",          // whitespace — skipped
            "{\"op\": 42}\n", // valid json, wrong shape
            "{\"id\": 7}\n",  // missing op
            "{\"id\": 9, \"op\": \"manifest\"}\n",
        );
        let out = replies(&ops, input);
        assert_eq!(out.len(), 4, "{out:?}");
        for (i, line) in out[..3].iter().enumerate() {
            assert_eq!(line["ok"], false, "{i}: {line}");
            assert_eq!(line["error"]["code"], "bad_request", "{i}: {line}");
        }
        assert_eq!(out[1]["id"], serde_json::Value::Null);
        assert_eq!(out[2]["id"], 7, "the id survives a missing-op parse");
        assert_eq!(out[3]["ok"], true, "the loop keeps serving");
        assert_eq!(out[3]["result"]["protocol"], "arca-serve/1");
    }

    #[test]
    fn non_utf8_lines_are_bad_request_not_a_crash() {
        let (_home, ops) = fixture("");
        let mut input = b"{\"id\":1}\xff\xfe\n".to_vec();
        input.extend_from_slice(b"{\"id\":2,\"op\":\"manifest\"}\n");
        let out = serve(&ops, &input);
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[0]["error"]["code"], "bad_request");
        assert_eq!(out[1]["ok"], true);
    }

    #[test]
    fn field_validation_is_bad_request() {
        let (_home, ops) = fixture("");
        let cases = [
            // missing ref
            "{\"op\":\"get\"}",
            // unparsable ref
            "{\"op\":\"get\",\"ref\":\"not-a-ref\"}",
            // invalid env
            "{\"op\":\"list\",\"env\":\"PROD!!\"}",
            // invalid actor id
            "{\"op\":\"list\",\"actor\":\"has space\"}",
            // put/rotate need a value
            "{\"op\":\"put\",\"ref\":\"arca://a/b\"}",
            "{\"op\":\"rotate\",\"ref\":\"arca://a/b\",\"value\":\"\"}",
        ];
        for line in cases {
            let out = replies(&ops, &format!("{line}\n"));
            assert_eq!(out.len(), 1, "{line}");
            assert_eq!(out[0]["ok"], false, "{line}");
            assert_eq!(out[0]["error"]["code"], "bad_request", "{line}");
        }
    }

    #[test]
    fn vault_ops_roundtrip() {
        let (_home, ops) = fixture("");
        let input = concat!(
            "{\"id\":1,\"op\":\"put\",\"ref\":\"arca://a/b\",\"value\":\"fake-one\"}\n",
            "{\"id\":2,\"op\":\"get\",\"ref\":\"arca://a/b\"}\n",
            "{\"id\":3,\"op\":\"list\"}\n",
            "{\"id\":4,\"op\":\"rotate\",\"ref\":\"arca://a/b\",\"value\":\"fake-two\"}\n",
            "{\"id\":5,\"op\":\"get\",\"ref\":\"arca://a/b\"}\n",
            "{\"id\":6,\"op\":\"rm\",\"ref\":\"arca://a/b\"}\n",
            "{\"id\":7,\"op\":\"get\",\"ref\":\"arca://a/b\"}\n",
            "{\"id\":8,\"op\":\"rm\",\"ref\":\"arca://a/b\"}\n",
            "{\"id\":9,\"op\":\"rotate\",\"ref\":\"arca://gone\",\"value\":\"x\"}\n",
        );
        let out = replies(&ops, input);
        assert_eq!(out.len(), 9, "{out:?}");
        assert_eq!(out[0]["ok"], true);
        assert_eq!(out[0]["result"]["version"], 1);
        assert_eq!(out[1]["result"]["value"], "fake-one");
        assert_eq!(out[2]["result"]["refs"][0]["ref"], "arca://a/b");
        assert_eq!(out[3]["result"]["version"], 2);
        assert!(out[3]["result"]["rotated_at"].is_string());
        assert_eq!(out[4]["result"]["value"], "fake-two");
        assert_eq!(out[5]["result"]["removed"], true);
        assert_eq!(out[6]["error"]["code"], "not_found", "{:?}", out[6]);
        assert_eq!(out[7]["error"]["code"], "not_found");
        assert_eq!(out[8]["error"]["code"], "not_found");
    }

    #[test]
    fn per_request_env_and_env_pins() {
        let (_home, ops) = fixture("");
        let input = concat!(
            "{\"id\":1,\"op\":\"put\",\"ref\":\"arca://a/x\",\"value\":\"v\",\"env\":\"prod\"}\n",
            "{\"id\":2,\"op\":\"list\",\"env\":\"prod\"}\n",
            "{\"id\":3,\"op\":\"get\",\"ref\":\"arca://a/x\"}\n",
            "{\"id\":4,\"op\":\"put\",\"ref\":\"arca://staging/pinned\",\"value\":\"v2\"}\n",
            "{\"id\":5,\"op\":\"get\",\"ref\":\"arca://pinned\",\"env\":\"staging\"}\n",
        );
        let out = replies(&ops, input);
        assert_eq!(out[0]["result"]["env"], "prod");
        assert_eq!(out[1]["result"]["env"], "prod");
        assert_eq!(out[1]["result"]["refs"].as_array().unwrap().len(), 1);
        assert_eq!(out[2]["error"]["code"], "not_found", "dev holds nothing");
        assert_eq!(out[3]["result"]["env"], "staging", "the pin routes the put");
        assert_eq!(out[4]["result"]["value"], "v2");
    }

    #[test]
    fn check_manifest_shutdown_and_unknown_ops() {
        let (_home, ops) = fixture("");
        let input = concat!(
            "{\"id\":1,\"op\":\"check\"}\n",
            "{\"id\":2,\"op\":\"manifest\"}\n",
            "{\"id\":3,\"op\":\"run\"}\n",
            "{\"id\":4,\"op\":\"wat\"}\n",
            "{\"id\":5,\"op\":\"shutdown\"}\n",
            "{\"id\":6,\"op\":\"manifest\"}\n", // after shutdown — never read
        );
        let out = replies(&ops, input);
        assert_eq!(out.len(), 5, "shutdown ends the loop: {out:?}");
        assert_eq!(out[0]["result"]["ok"], true, "{:?}", out[0]);
        assert_eq!(out[0]["result"]["vaults"][0]["ok"], true);
        assert_eq!(out[1]["result"]["protocol"], "arca-serve/1");
        assert_eq!(out[2]["error"]["code"], "unknown_op");
        assert!(
            out[2]["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not served"),
            "{:?}",
            out[2]
        );
        assert_eq!(out[3]["error"]["code"], "unknown_op");
        assert_eq!(out[4]["ok"], true);
        assert_eq!(out[4]["result"]["shutdown"], true);
    }

    #[test]
    fn pdp_allow_carries_the_verdict_through() {
        let home = tempfile::tempdir().unwrap();
        let cmd = pdp_script(
            &home,
            true,
            "printf '%s' '{\"allow\":true,\"reason\":\"p-ok\",\"obligations\":[\"redact\"]}'",
        );
        let (_h, ops) = fixture(&format!("[serve.pdp]\nexec = \"{cmd}\"\n"));
        // put lands so get has something to decide on.
        replies(
            &ops,
            "{\"op\":\"put\",\"ref\":\"arca://a/b\",\"value\":\"fake\"}\n",
        );
        let out = replies(
            &ops,
            "{\"id\":1,\"op\":\"get\",\"ref\":\"arca://a/b\",\"actor\":\"agent:bot-1\",\"attestation\":\"fake-token\"}\n",
        );
        assert_eq!(out[0]["ok"], true, "{:?}", out[0]);
        assert_eq!(out[0]["result"]["value"], "fake");
        assert_eq!(out[0]["policy"]["allow"], true);
        assert_eq!(out[0]["policy"]["reason"], "p-ok");
        assert_eq!(out[0]["policy"]["obligations"][0], "redact");
        // the consult carried actor/env/op/ref — including the
        // claimed actor and the opaque attestation, verbatim.
        let sent: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.path().join("pdp-req.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(sent["actor"], "agent:bot-1");
        assert_eq!(sent["attestation"], "fake-token");
        assert_eq!(sent["op"], "get");
        assert_eq!(sent["env"], "dev");
        assert_eq!(sent["ref"], "a/b");
    }

    #[test]
    fn pdp_deny_blocks_the_op_before_the_vault() {
        let home = tempfile::tempdir().unwrap();
        let cmd = pdp_script(
            &home,
            false,
            "printf '%s' '{\"allow\":false,\"reason\":\"no reads\"}'",
        );
        let (_h, ops) = fixture(&format!("[serve.pdp]\nexec = \"{cmd}\"\n"));
        let out = replies(
            &ops,
            "{\"id\":1,\"op\":\"get\",\"ref\":\"arca://a/b\"}\n{\"id\":2,\"op\":\"list\"}\n",
        );
        assert_eq!(out[0]["error"]["code"], "denied");
        assert!(out[0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no reads"));
        assert_eq!(out[0]["policy"]["allow"], false);
        // non-vault ops don't consult — list still works.
        assert_eq!(out[1]["ok"], true);
    }

    #[test]
    fn unreachable_pdp_fails_closed_by_default() {
        let home = tempfile::tempdir().unwrap();
        let cmd = pdp_script(&home, false, "exit 3");
        let (_h, ops) = fixture(&format!("[serve.pdp]\nexec = \"{cmd}\"\n"));
        let out = replies(
            &ops,
            "{\"op\":\"put\",\"ref\":\"arca://a/b\",\"value\":\"x\"}\n",
        );
        assert_eq!(out[0]["ok"], false);
        assert_eq!(out[0]["error"]["code"], "pdp_unreachable");
        assert_eq!(out[0]["policy"]["allow"], false);
        // and nothing was stored — the consult happens before open
        let out = replies(&ops, "{\"op\":\"list\"}\n");
        assert_eq!(out[0]["result"]["refs"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn allow_on_error_runs_with_a_degraded_verdict() {
        let home = tempfile::tempdir().unwrap();
        let cmd = pdp_script(&home, false, "printf 'garbage'");
        let (_h, ops) = fixture(&format!(
            "[serve.pdp]\nexec = \"{cmd}\"\nallow_on_error = true\n"
        ));
        let out = replies(
            &ops,
            "{\"op\":\"put\",\"ref\":\"arca://a/b\",\"value\":\"x\"}\n{\"op\":\"get\",\"ref\":\"arca://a/b\"}\n",
        );
        assert_eq!(out[0]["ok"], true, "{:?}", out[0]);
        assert_eq!(out[0]["policy"]["allow"], true);
        assert!(out[0]["policy"]["reason"]
            .as_str()
            .unwrap()
            .contains("allow_on_error"));
        assert_eq!(out[1]["result"]["value"], "x");
    }

    #[test]
    fn guardrails_still_deny_advisory() {
        let (_home, ops) = fixture("[guardrails]\ndeny = [\"get\"]\n");
        let out = replies(
            &ops,
            "{\"op\":\"put\",\"ref\":\"arca://a/b\",\"value\":\"x\"}\n{\"op\":\"get\",\"ref\":\"arca://a/b\"}\n",
        );
        assert_eq!(out[0]["ok"], true);
        assert_eq!(out[1]["error"]["code"], "denied");
        assert!(out[1]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("guardrails"));
    }

    #[test]
    fn eof_is_a_clean_exit() {
        let (_home, ops) = fixture("");
        // no trailing line at all
        let out = replies(&ops, "");
        assert!(out.is_empty());
    }
}

//! arca — secrets broker for agents. Sealed, not spilled.
//!
//! Code and config carry `arca://` references, never values; `arca
//! run` materializes declared secrets as env vars in the child only.
//! Every materialization — and every denial/failure — emits an
//! audit ndjson line on stderr.
//!
//! Values never enter via CLI args — `put`/`rotate` read stdin or a
//! hidden `/dev/tty` prompt (args land in shell history and
//! `/proc`). `get` is the escape hatch that prints a value to
//! stdout; `[guardrails]` can advisory-deny it for agents.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Read, Write};
use std::process::ExitCode;

use arca_core::{Op, Ref, Secret};
use arca_ops::{AuditEvent, LoadOpts, Ops, OpsError, Outcome};
use clap::{Parser, Subcommand};

/// arca — secrets broker for agents: `arca://` refs, redaction,
/// env-injection instead of plaintext.
///
/// Refs name logical secrets (`arca://billing/stripe`); `--env`
/// picks the resolution context. `[secrets]` in arca.toml bounds
/// what `run`/`env` may inject. `manifest`/`schema` describe the
/// surface for machines; every verb honors `--json`.
#[derive(Parser)]
#[command(name = "arca", version, about, long_about)]
struct Cli {
    /// Emit JSON — stable machine shapes on every verb; errors are
    /// `{"error": "…"}` on stderr with a non-zero exit.
    #[arg(long, global = true)]
    json: bool,

    /// Env context — `dev`/`staging`/`prod`/custom. Overrides
    /// `ARCA_ENV`; default `dev`.
    #[arg(long, global = true, value_name = "ENV")]
    env: Option<String>,

    /// Config file, overriding the search
    /// ($ARCA_CONFIG → ./arca.toml → ~/.config/arca/arca.toml).
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<String>,

    /// Actor attribution for audit events (default $ARCA_ACTOR,
    /// else $USER).
    #[arg(long, global = true, value_name = "ACTOR")]
    actor: Option<String>,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Bootstrap: generate the age identity (0600) if missing and
    /// verify the vault opens.
    Init,
    /// Store a secret. The value comes from stdin (`printf … | arca
    /// put arca://ns/name`) or a hidden terminal prompt — never
    /// args, which land in shell history and `/proc`.
    Put {
        /// `arca://` ref, e.g. arca://billing/stripe.
        rref: String,
    },
    /// Print a secret's value to stdout — the escape hatch. Prefer
    /// `arca run` so values never enter agent context; guardrails
    /// can deny this op.
    Get {
        /// `arca://` ref.
        rref: String,
    },
    /// List the refs stored in the selected env — names only, never
    /// values.
    List,
    /// Remove a secret from the selected env.
    Rm {
        /// `arca://` ref.
        rref: String,
    },
    /// Rotate a secret to a new value (stdin or hidden prompt, like
    /// `put`); bumps version + rotated_at.
    Rotate {
        /// `arca://` ref.
        rref: String,
    },
    /// Run a command with declared secrets injected as env vars.
    ///
    /// Injection surface = `[secrets]` in arca.toml plus `-s
    /// VAR=arca://ref` ad-hoc pairs; nothing else is injectable.
    /// Two exec modes after `--`:
    ///
    /// `arca run -- 'pnpm test | tee log'` — one string → `bash -c`
    ///
    /// `arca run -- pnpm test --filter x` — argv → direct exec
    ///
    /// argv mode needs no shell quoting — prefer it when the
    /// command has arguments. Exit code is the child's.
    Run {
        /// Extra `VAR=arca://ref` pairs to inject.
        #[arg(short = 's', long = "secret", value_name = "VAR=ARCA_REF")]
        secrets: Vec<String>,
        /// Command to run (after `--`). One arg → `bash -c`;
        /// several → `argv[0]` exec'd directly with the rest.
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
    /// Print `export VAR='…'` lines for `eval "$(arca env)"` — the
    /// human path (`run` is the agent path). Values print to
    /// stdout: `--json` emits a `VAR → value` map.
    Env {
        /// Extra `VAR=arca://ref` pairs to resolve.
        #[arg(short = 's', long = "secret", value_name = "VAR=ARCA_REF")]
        secrets: Vec<String>,
    },
    /// Validate config + the declared surface: `[envs]` routes
    /// point at declared vaults, vaults decrypt, every `[secrets]`
    /// ref resolves in its routed env. Exit 1 on findings.
    Check,
    /// Rewrite stored records for a wire/config upgrade. Currently
    /// one transform: `--private-refs` re-keys every plaintext-named
    /// row in each env routed to a cf vault with
    /// `[vaults.*] private_refs = true` — ciphertext moves verbatim,
    /// old keys are deleted after the new rows land.
    Migrate {
        /// Re-key cf-vault rows under HMAC-derived storage keys,
        /// attaching the encrypted `name` blob (P-004).
        #[arg(long)]
        private_refs: bool,
    },
    /// Push a stored secret to a write-only deploy target
    /// (`[deploy.NAME]` in arca.toml, e.g. a CF Secrets Store).
    /// The vault is the source of truth; the sink never gives the
    /// value back. With one configured target, `--target` may be
    /// omitted.
    Deploy {
        /// `arca://` ref to push.
        rref: String,
        /// Deploy target name (`[deploy.NAME]`); required when
        /// several are configured.
        #[arg(long, value_name = "NAME")]
        target: Option<String>,
    },
    /// Interactive vault browser (requires the `tui` feature).
    Ui,
    /// Capabilities, JSON shapes, env vars, audit contract — agent
    /// self-discovery (always JSON).
    Manifest,
    /// JSON Schema for `arca.toml`.
    Schema,
    /// Print the CLI reference as markdown (docs pipeline —
    /// `bun run docs:cli` pipes it to `docs/reference/cli.md`).
    #[command(hide = true, name = "gen-cli-docs")]
    GenCliDocs,
}

fn main() -> ExitCode {
    sigpipe::reset();
    let cli = Cli::parse();
    match &cli.command {
        // Pure introspection — before config load, no audit event.
        Cmd::Manifest => json_out(&manifest()),
        Cmd::GenCliDocs => out(&clap_markdown::help_markdown::<Cli>()),
        Cmd::Schema => out(&schema_json()),
        cmd => {
            let op = cmd_op(cmd);
            let rref = cmd_rref(cmd);
            let ops = match load(&cli) {
                Ok(ops) => ops,
                Err(e) => {
                    audit_fallback(&cli, op, rref, Outcome::Error);
                    return fail(&cli, &e.to_string());
                }
            };
            // Advisory guardrails — accident prevention, not
            // policy; enforced before the op touches the vault.
            if let Some(g) = ops.config().guardrails.as_ref() {
                if !g.allows(op) {
                    let rref = norm_rref(&ops, rref);
                    ops.emit(op, rref.as_deref(), Outcome::Denied);
                    // guardrails only exist when a config file
                    // loaded — config_path is always Some here
                    #[allow(clippy::unwrap_used)]
                    let at = ops
                        .config_path()
                        .expect("guardrails imply a loaded config file")
                        .display()
                        .to_string();
                    return fail(
                        &cli,
                        &format!(
                            "op '{op}' denied by guardrails in {at} \
                             (advisory — remove/relax the rule to allow)"
                        ),
                    );
                }
            }
            match run_cmd(cmd, &cli, &ops) {
                Ok(code) => code,
                Err(e) => {
                    let rref = norm_rref(&ops, rref);
                    ops.emit(op, rref.as_deref(), Outcome::Error);
                    fail(&cli, &e.to_string())
                }
            }
        }
    }
}

/// The [`Op`] a subcommand maps to — what guardrails and audit
/// record.
fn cmd_op(cmd: &Cmd) -> Op {
    match cmd {
        Cmd::Init => Op::Init,
        Cmd::Put { .. } => Op::Put,
        Cmd::Get { .. } => Op::Get,
        Cmd::List => Op::List,
        Cmd::Rm { .. } => Op::Rm,
        Cmd::Rotate { .. } => Op::Rotate,
        Cmd::Run { .. } => Op::Run,
        Cmd::Env { .. } => Op::Env,
        Cmd::Check => Op::Check,
        Cmd::Migrate { .. } => Op::Migrate,
        Cmd::Deploy { .. } => Op::Deploy,
        Cmd::Ui => Op::Ui,
        Cmd::Manifest => Op::Manifest,
        Cmd::Schema => Op::Schema,
        // Handled before ops load — this arm is only for
        // exhaustiveness, never reached.
        Cmd::GenCliDocs => Op::Schema,
    }
}

/// The raw ref arg for verbs that take one — for denied/error
/// audit events, before (or regardless of) parse.
fn cmd_rref(cmd: &Cmd) -> Option<&str> {
    match cmd {
        Cmd::Put { rref }
        | Cmd::Get { rref }
        | Cmd::Rm { rref }
        | Cmd::Rotate { rref }
        | Cmd::Deploy { rref, .. } => Some(rref),
        _ => None,
    }
}

/// Normalize a requested ref to the resolved-path form success
/// events carry (`env pin` stripped, no scheme) so the audit
/// stream correlates regardless of outcome. Falls back to the raw
/// arg when it doesn't parse — the honest record of a bad request.
fn norm_rref(ops: &Ops, raw: Option<&str>) -> Option<String> {
    raw.map(|s| {
        Ref::new(s)
            .map(|r| ops.resolve(&r).1.path().to_string())
            .unwrap_or_else(|_| s.to_string())
    })
}

/// Audit an op that never got an `Ops` (config/env/actor
/// resolution failed) — best-effort actor + env from flags and
/// process env.
fn audit_fallback(cli: &Cli, op: Op, rref: Option<&str>, outcome: Outcome) {
    let actor = cli
        .actor
        .clone()
        .or_else(|| std::env::var("ARCA_ACTOR").ok())
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "anon".into());
    let env = cli
        .env
        .clone()
        .or_else(|| std::env::var("ARCA_ENV").ok())
        .unwrap_or_else(|| "dev".into());
    let rref = rref.map(|s| {
        Ref::new(s)
            .map(|r| r.path().to_string())
            .unwrap_or_else(|_| s.to_string())
    });
    arca_ops::audit::emit(&AuditEvent::op(&actor, op, rref.as_deref(), &env, outcome));
}

fn load(cli: &Cli) -> Result<Ops, OpsError> {
    Ops::load(LoadOpts {
        config_path: cli.config.clone(),
        env: cli.env.clone(),
        actor: cli.actor.clone(),
    })
}

fn run_cmd(cmd: &Cmd, cli: &Cli, ops: &Ops) -> Result<ExitCode, OpsError> {
    match cmd {
        Cmd::Init => {
            let info = ops.init()?;
            ops.emit(Op::Init, None, Outcome::Ok);
            if cli.json {
                return Ok(json_out(&serde_json::json!({
                    "identity": info.identity_path,
                    "created": info.identity_created,
                    "recipient": info.recipient,
                    "vault": info.vault_path,
                })));
            }
            eprintln!(
                "identity: {}{}",
                info.identity_path.display(),
                if info.identity_created {
                    " (generated)"
                } else {
                    ""
                }
            );
            eprintln!("recipient: {}", info.recipient);
            eprintln!("vault:     {}", info.vault_path.display());
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Put { rref } => {
            let parsed = parse_ref(rref)?;
            let secret = read_secret(&format!("value for {rref}: "))?;
            let rec = ops.put(&parsed, secret)?;
            if cli.json {
                let mut v = record_json(ops, &parsed, &rec);
                v["ok"] = true.into();
                return Ok(json_out(&v));
            }
            eprintln!("stored {rref} (v{})", rec.version);
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Get { rref } => {
            let parsed = parse_ref(rref)?;
            match ops.get(&parsed)? {
                Some(rec) => {
                    if cli.json {
                        let mut v = record_json(ops, &parsed, &rec);
                        v["value"] = rec.value.expose().into();
                        return Ok(json_out(&v));
                    }
                    if std::io::stdout().is_terminal() {
                        eprintln!("note: secret value printed to terminal");
                    }
                    println!("{}", rec.value.expose());
                    Ok(ExitCode::SUCCESS)
                }
                None => Err(OpsError::Msg(format!(
                    "{rref} is not stored in env {}",
                    ops.resolve(&parsed).0
                ))),
            }
        }
        Cmd::List => {
            let (env, refs) = ops.list(None)?;
            ops.emit(Op::List, None, Outcome::Ok);
            if cli.json {
                let (_, records) = ops.list_records(None)?;
                let arr: Vec<serde_json::Value> = records
                    .iter()
                    .map(|(r, rec)| {
                        serde_json::json!({
                            "ref": r.to_string(),
                            "env": env.as_str(),
                            "version": rec.version,
                            "created_at": rec.created_at.to_string(),
                            "rotated_at": rec.rotated_at.map(|t| t.to_string()),
                        })
                    })
                    .collect();
                return Ok(json_out(&serde_json::Value::Array(arr)));
            }
            for r in refs {
                println!("{r}");
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Rm { rref } => {
            let parsed = parse_ref(rref)?;
            let (env, _) = ops.resolve(&parsed);
            if !ops.rm(&parsed)? {
                return Err(OpsError::Msg(format!("{rref} is not stored in env {env}")));
            }
            if cli.json {
                return Ok(json_out(&serde_json::json!({
                    "ok": true,
                    "removed": true,
                    "ref": parsed.to_string(),
                    "env": env.as_str(),
                })));
            }
            eprintln!("removed {rref}");
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Rotate { rref } => {
            let parsed = parse_ref(rref)?;
            let secret = read_secret(&format!("new value for {rref}: "))?;
            let rec = ops.rotate(&parsed, secret)?;
            if cli.json {
                let mut v = record_json(ops, &parsed, &rec);
                v["ok"] = true.into();
                return Ok(json_out(&v));
            }
            eprintln!("rotated {rref} (v{})", rec.version);
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Run { secrets, cmd } => {
            let vars = ops.resolve_for_injection(Op::Run, &parse_pairs(secrets)?)?;
            let exposed: Vec<(String, String)> = vars
                .iter()
                .map(|(k, v)| (k.clone(), v.expose().to_string()))
                .collect();
            let cwd = std::env::current_dir()?;
            // One arg → `bash -c` (shell syntax); several → exec
            // argv[0] directly, no shell quoting.
            let status = if cmd.len() == 1 {
                arca_shell::run_with_env(&cwd, &cmd[0], &exposed)?
            } else {
                arca_shell::exec_with_env(&cwd, cmd, &exposed)?
            };
            Ok(ExitCode::from(status.code().unwrap_or(1) as u8))
        }
        Cmd::Env { secrets } => {
            let vars = ops.resolve_for_injection(Op::Env, &parse_pairs(secrets)?)?;
            if cli.json {
                let map: BTreeMap<&String, &str> =
                    vars.iter().map(|(k, v)| (k, v.expose())).collect();
                return Ok(json_out(&serde_json::json!(map)));
            }
            let stdout = std::io::stdout();
            let mut w = stdout.lock();
            for (k, v) in &vars {
                writeln!(w, "export {k}={}", sh_escape(v.expose()))?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Check => {
            let report = ops.check();
            ops.emit(Op::Check, None, Outcome::Ok);
            let ok = report.problems.is_empty();
            if cli.json {
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
                    })
                });
                let code = json_out(&serde_json::json!({
                    "config": report.config_path,
                    "env": report.env.as_str(),
                    "vault": report.vault,
                    "vaults": vaults,
                    "secrets": secrets,
                    "guardrails": guardrails,
                    "errors": report.problems,
                }));
                debug_assert_eq!(code, ExitCode::SUCCESS);
            } else {
                match &report.config_path {
                    Some(p) => eprintln!("config: {}", p.display()),
                    None => eprintln!("config: (none found — defaults)"),
                }
                eprintln!("env: {} → vault {:?}", report.env, report.vault,);
                for v in &report.vaults {
                    match &v.error {
                        None => eprintln!("vault {:?} ({}): readable", v.name, v.kind),
                        Some(e) => eprintln!("vault {:?} ({}): {e}", v.name, v.kind),
                    }
                }
                for s in &report.secrets {
                    eprintln!(
                        "secret {} → {} (env {}): {}",
                        s.var,
                        s.rref,
                        s.env,
                        if s.resolves { "resolves" } else { "NOT STORED" }
                    );
                }
                if let Some(g) = &report.guardrails {
                    let deny: Vec<_> = g.deny.iter().map(|o| o.as_str()).collect();
                    eprintln!("guardrails: configured (deny: {})", deny.join(", "));
                }
                if report.problems.is_empty() {
                    eprintln!("ok");
                } else {
                    for p in &report.problems {
                        eprintln!("problem: {p}");
                    }
                }
            }
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Cmd::Migrate { private_refs } => {
            if !private_refs {
                return Err(OpsError::Msg(
                    "migrate needs a target — currently only `--private-refs`".into(),
                ));
            }
            let reports = ops.migrate_private_refs()?;
            ops.emit(Op::Migrate, None, Outcome::Ok);
            if cli.json {
                let migrated: Vec<serde_json::Value> = reports
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "vault": r.vault,
                            "env": r.env.as_str(),
                            "rewritten": r.rewritten,
                            "already_private": r.already_private,
                        })
                    })
                    .collect();
                return Ok(json_out(&serde_json::json!({
                    "ok": true,
                    "migrated": migrated,
                })));
            }
            for r in &reports {
                eprintln!(
                    "vault {:?} env {}: rewrote {} ref(s) ({} already private)",
                    r.vault,
                    r.env,
                    r.rewritten.len(),
                    r.already_private
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Deploy { rref, target } => {
            let parsed = parse_ref(rref)?;
            let report = ops.deploy(&parsed, target.as_deref())?;
            if cli.json {
                return Ok(json_out(&serde_json::json!({
                    "ok": true,
                    "ref": report.rref.to_string(),
                    "env": report.env.as_str(),
                    "target": report.target,
                    "secret_name": report.secret_name,
                    "created": report.created,
                })));
            }
            eprintln!(
                "deployed {} → {:?} as {} ({})",
                report.rref,
                report.target,
                report.secret_name,
                if report.created { "created" } else { "updated" }
            );
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Ui => {
            #[cfg(feature = "tui")]
            {
                arca_tui::run(ops)?;
                ops.emit(Op::Ui, None, Outcome::Ok);
                Ok(ExitCode::SUCCESS)
            }
            #[cfg(not(feature = "tui"))]
            {
                eprintln!("`arca ui` requires the `tui` feature (on by default)");
                Ok(ExitCode::FAILURE)
            }
        }
        Cmd::Manifest | Cmd::Schema | Cmd::GenCliDocs => {
            unreachable!("handled before ops load")
        }
    }
}

fn parse_ref(s: &str) -> Result<Ref, OpsError> {
    Ok(s.parse::<Ref>()?)
}

/// `-s VAR=arca://ref` pairs.
fn parse_pairs(raw: &[String]) -> Result<Vec<(String, String)>, OpsError> {
    raw.iter()
        .map(|s| {
            s.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .ok_or_else(|| OpsError::Msg(format!("-s wants VAR=arca://ref, got {s:?}")))
        })
        .collect()
}

/// Secrets arrive on stdin (the scripted path) or a hidden
/// `/dev/tty` prompt (the human path) — never args. One trailing
/// line terminator is stripped; the rest is the value verbatim
/// (newlines allowed on the pipe path).
fn read_secret(prompt: &str) -> Result<Secret, OpsError> {
    let stdin = std::io::stdin();
    let raw = if stdin.is_terminal() {
        rpassword::prompt_password(prompt)
            .map_err(|e| OpsError::Msg(format!("read secret: {e}")))?
    } else {
        let mut buf = String::new();
        stdin
            .lock()
            .read_to_string(&mut buf)
            .map_err(|e| OpsError::Msg(format!("read stdin: {e}")))?;
        buf
    };
    let v = raw
        .strip_suffix("\r\n")
        .or_else(|| raw.strip_suffix('\n'))
        .unwrap_or(&raw);
    if v.is_empty() {
        return Err(OpsError::Msg("empty secret".into()));
    }
    Ok(Secret::new(v))
}

/// Shell single-quote escape for `arca env` output — `'` becomes
/// `'\''`, so any value is safe inside the quotes.
fn sh_escape(v: &str) -> String {
    format!("'{}'", v.replace('\'', "'\\''"))
}

/// The canonical record shape: resolved ref + env plus record
/// metadata. Callers add `value`/`ok` as needed — never by
/// default.
fn record_json(ops: &Ops, parsed: &Ref, rec: &arca_vault::SecretRecord) -> serde_json::Value {
    let (env, r) = ops.resolve(parsed);
    serde_json::json!({
        "ref": r.to_string(),
        "env": env.as_str(),
        "version": rec.version,
        "created_at": rec.created_at.to_string(),
        "rotated_at": rec.rotated_at.map(|t| t.to_string()),
    })
}

/// The machine-readable capability doc — verbs, JSON shapes, env
/// vars, the audit contract. Keep honest: this is what the binary
/// actually does, not what it plans to.
fn manifest() -> serde_json::Value {
    serde_json::json!({
        "name": "arca",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "secrets broker for agents — arca:// refs in code/config, env-var injection at run time, ndjson audit on stderr",
        "repository": "https://github.com/9aia/arca",
        "protocol": "arca/1",
        "ops": arca_core::Op::ALL.iter().map(|o| o.as_str()).collect::<Vec<_>>(),
        "verbs": {
            "init": "generate the age identity (0600) if missing; verify the vault opens",
            "put": "store a secret — value via stdin or hidden /dev/tty prompt, never args",
            "get": "print a secret's value (the escape hatch — prefer run)",
            "list": "refs stored in the selected env — names only, never values",
            "rm": "remove a secret",
            "rotate": "new value via stdin/prompt; version bump + rotated_at",
            "run": "inject [secrets]+`-s` into a child — one arg → bash -c, several → argv exec (no shell)",
            "env": "export lines for eval (human path); --json → VAR→value map",
            "check": "validate config, vault readability, declared-surface resolution; exit 1 on findings",
            "migrate": "rewrite stored records for a wire upgrade — `--private-refs` re-keys cf rows under derived keys",
            "deploy": "push a stored value to a write-only [deploy.NAME] target (sink, never a source)",
            "ui": "interactive vault browser (tui feature)",
            "manifest": "this document (always JSON; before config load, no audit event)",
            "schema": "JSON Schema for arca.toml (before config load)",
            "gen-cli-docs": "hidden — the CLI reference as markdown (docs pipeline)",
        },
        "flags": {
            "--json": "stable shapes below; errors are {\"error\": \"…\"} on stderr, non-zero exit",
            "--env ENV": "resolution context (dev/staging/prod/custom; overrides ARCA_ENV)",
            "--config PATH": "config override (else $ARCA_CONFIG → ./arca.toml → ~/.config/arca/arca.toml)",
            "--actor ACTOR": "audit attribution (else ARCA_ACTOR → $USER → anon)",
            "-s VAR=arca://ref": "ad-hoc injection pair on run/env (repeatable)",
            "--target NAME": "deploy target on `arca deploy` (required when several [deploy.*] tables exist)",
            "--private-refs": "on `arca migrate` — re-key cf-vault rows under HMAC-derived keys",
        },
        "json_shapes": {
            "init": {"identity": "path", "created": true, "recipient": "age1…", "vault": "path"},
            "put": {"ok": true, "ref": "arca://ns/name", "env": "dev", "version": 1, "created_at": "rfc3339", "rotated_at": "rfc3339|null"},
            "rotate": "same as put",
            "get": {"ref": "arca://ns/name", "env": "dev", "value": "plaintext", "version": 1, "created_at": "rfc3339", "rotated_at": "rfc3339|null"},
            "list": [{"ref": "arca://ns/name", "env": "dev", "version": 1, "created_at": "rfc3339", "rotated_at": "rfc3339|null"}],
            "rm": {"ok": true, "removed": true, "ref": "arca://ns/name", "env": "dev"},
            "env": {"VAR": "plaintext value"},
            "check": {
                "config": "path|null", "env": "dev", "vault": "local",
                "vaults": [{"name": "local", "kind": "age", "path": "path|null", "ok": true, "error": "…|null"}],
                "secrets": {"VAR": {"ref": "arca://ns/name", "env": "dev", "resolves": true}},
                "guardrails": {"default": "allow|deny", "allow": ["run"], "deny": ["get"]},
                "errors": ["finding…"],
            },
            "deploy": {"ok": true, "ref": "arca://ns/name", "env": "dev", "target": "prod", "secret_name": "dev_ns_name", "created": true},
            "migrate": {"ok": true, "migrated": [{"vault": "cloud", "env": "prod", "rewritten": ["ns/name"], "already_private": 0}]},
            "run": "passthrough — child stdout/stderr/exit code; --json changes nothing",
            "error": {"error": "message (stderr, non-zero exit)"},
        },
        "run_modes": {
            "shell": "arca run -- 'cmd | pipeline' — a single arg runs via bash -c",
            "argv": "arca run -- prog arg… — multiple args exec argv[0] directly, no shell",
        },
        "env_vars": {
            "ARCA_CONFIG": "config file override",
            "ARCA_ENV": "env context (default dev)",
            "ARCA_ACTOR": "audit actor attribution",
            "ARCA_IDENTITY_FILE": "age identity file override",
            "ARCA_CF_TOKEN": "default cf-vault bearer env var ([vaults.*] token_env overrides the name)",
            "CLOUDFLARE_API_TOKEN": "default deploy-token env var ([deploy.*] token_env overrides the name)",
        },
        "config": {
            "search": ["--config", "$ARCA_CONFIG", "./arca.toml", "~/.config/arca/arca.toml"],
            "tables": ["vaults", "envs", "secrets", "deploy", "guardrails"],
            "schema": "arca schema",
            "env_pin": "arca://prod/ns/name pins the env regardless of context",
        },
        "vault_kinds": ["age", "cf"],
        "deploy_kinds": ["cf-secrets-store"],
        "audit": {
            "wire": "ndjson lines on stderr",
            "event": {"ts": "rfc3339", "actor": "…", "op": "…", "rref": "…|absent when op-scoped", "env": "…", "outcome": "ok|denied|error"},
            "shape": "per-ref event per materialized/mutated ref (put/get/rotate/rm/deploy, run/env); op-scoped event for init/list/check/ui; denied on guardrail refusal; error on op failure; manifest/schema emit none; a run child's exit code is not an arca failure",
        },
        "features": {
            "tui": cfg!(feature = "tui"),
        },
        "notes": [
            "values never enter via CLI args — put/rotate read stdin or a hidden /dev/tty prompt",
            "[secrets] bounds what run/env may inject (least privilege); -s covers one-offs",
            "guardrails are advisory — accident prevention, not policy; ARCA_ACTOR is self-claimed",
            "list never carries values; get/env are the only surfaces that print them",
            "cf vaults store per-secret age ciphertext on a self-hosted Worker + D1 — the worker never sees plaintext",
            "cf wire is arca-vault-cf/2: ArcaHmac signed requests (token never travels) with per-env scopes; bearer still accepted during migration",
            "[vaults.*] private_refs = true hides ref names from the cf store — HMAC-derived keys + encrypted name blobs; `arca migrate --private-refs` rewrites existing rows",
            "deploy targets are write-only sinks — the value crosses to the sink in plaintext, never fetched back",
        ],
    })
}

fn out(s: &str) -> ExitCode {
    println!("{s}");
    ExitCode::SUCCESS
}

/// Serializing a `serde_json::Value` cannot fail — the expect
/// documents that invariant rather than guarding a real path.
#[allow(clippy::unwrap_used)]
fn json_out(v: &serde_json::Value) -> ExitCode {
    out(&serde_json::to_string_pretty(v).expect("Value serializes"))
}

/// The `arca schema` payload — generated schema serializes
/// infallibly, same invariant as [`json_out`].
#[allow(clippy::unwrap_used)]
fn schema_json() -> String {
    serde_json::to_string_pretty(&schemars::schema_for!(arca_core::Config))
        .expect("generated schema serializes")
}

/// Error path honoring `--json`: `{"error": …}` on stderr for
/// machines, `arca: …` for humans.
fn fail(cli: &Cli, msg: &str) -> ExitCode {
    if cli.json {
        eprintln!("{}", serde_json::json!({ "error": msg }));
    } else {
        eprintln!("arca: {msg}");
    }
    ExitCode::FAILURE
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn cmd_op_maps_introspection_verbs_for_exhaustiveness() {
        // These verbs return before ops load — cmd_op only ever
        // sees them if the dispatch order above is broken, but the
        // match still has to be total.
        assert_eq!(cmd_op(&Cmd::Manifest), Op::Manifest);
        assert_eq!(cmd_op(&Cmd::Schema), Op::Schema);
        assert_eq!(cmd_op(&Cmd::GenCliDocs), Op::Schema);
    }

    #[test]
    #[should_panic(expected = "handled before ops load")]
    fn run_cmd_on_an_introspection_verb_panics() {
        let cli = Cli {
            json: false,
            env: None,
            config: None,
            actor: None,
            command: Cmd::Manifest,
        };
        let home = tempfile::tempdir().unwrap();
        temp_env::with_vars(
            [
                ("HOME", Some(home.path().to_str().unwrap())),
                ("ARCA_CONFIG", None::<&str>),
            ],
            || {
                let ops = Ops::load(LoadOpts::default()).unwrap();
                let _ = run_cmd(&Cmd::Manifest, &cli, &ops);
            },
        );
    }
}

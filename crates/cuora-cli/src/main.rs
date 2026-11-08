//! cuora — secrets broker for agents. Sealed, not spilled.
//!
//! Code and config carry `cuora://` references, never values; `cuora
//! run` materializes declared secrets as env vars in the child only.
//! Every materialization — and every denial/failure — emits an
//! audit event: ndjson on stderr, plus OTLP spans when
//! `OTEL_EXPORTER_OTLP_ENDPOINT` is set.
//!
//! Values never enter via CLI args — `put`/`rotate` read stdin or a
//! hidden `/dev/tty` prompt (args land in shell history and
//! `/proc`). `get` is the escape hatch that prints a value to
//! stdout; `[guardrails]` can advisory-deny it for agents.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Read, Write};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use cuora_core::{Op, Ref, Secret};
use cuora_ops::{env_var, AuditEvent, LoadOpts, Ops, OpsError, Outcome};

/// cuora — secrets broker for agents: `cuora://` refs, redaction,
/// env-injection instead of plaintext.
///
/// Refs name logical secrets (`cuora://billing/stripe`); `--env`
/// picks the resolution context. `[secrets]` in cuora.toml bounds
/// what `run`/`env` may inject. `manifest`/`schema` describe the
/// surface for machines; every verb honors `--json`.
#[derive(Parser)]
#[command(name = "cuora", version, about, long_about)]
struct Cli {
    /// Emit JSON — stable machine shapes on every verb; errors are
    /// `{"error": "…"}` on stderr with a non-zero exit.
    #[arg(long, global = true)]
    json: bool,

    /// Env context — `dev`/`staging`/`prod`/custom. Overrides
    /// `CUORA_ENV`; default `dev`.
    #[arg(long, global = true, value_name = "ENV")]
    env: Option<String>,

    /// Config file, overriding the search
    /// ($CUORA_CONFIG → ./cuora.toml → ~/.config/cuora/cuora.toml).
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<String>,

    /// Actor attribution for audit events (default $CUORA_ACTOR,
    /// else $USER).
    #[arg(long, global = true, value_name = "ACTOR")]
    actor: Option<String>,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Bootstrap: store the age identity in the system keyring
    /// when a session secret store is reachable — importing an
    /// existing identity.txt rather than orphaning it — else the
    /// `~/.config/cuora/identity.txt` keyfile (0600). Verifies the
    /// vault opens.
    Init {
        /// Force the identity keyfile even when a keyring is
        /// available — the headless/CI mode.
        #[arg(long)]
        with_file: bool,
    },
    /// Store a secret. The value comes from stdin (`printf … | cuora
    /// put cuora://ns/name`) or a hidden terminal prompt — never
    /// args, which land in shell history and `/proc`.
    Put {
        /// `cuora://` ref, e.g. cuora://billing/stripe.
        rref: String,
    },
    /// Print a secret's value to stdout — the escape hatch. Prefer
    /// `cuora run` so values never enter agent context; guardrails
    /// can deny this op.
    Get {
        /// `cuora://` ref.
        rref: String,
    },
    /// List the refs stored in the selected env — names only, never
    /// values.
    List,
    /// Remove a secret from the selected env.
    Rm {
        /// `cuora://` ref.
        rref: String,
    },
    /// Rotate a secret to a new value (stdin or hidden prompt, like
    /// `put`); bumps version + rotated_at.
    Rotate {
        /// `cuora://` ref.
        rref: String,
    },
    /// Run a command with declared secrets injected as env vars.
    ///
    /// Injection surface = `[secrets]` in cuora.toml plus `-s
    /// VAR=cuora://ref` ad-hoc pairs; nothing else is injectable.
    /// Two exec modes after `--`:
    ///
    /// `cuora run -- 'pnpm test | tee log'` — one string → `bash -c`
    ///
    /// `cuora run -- pnpm test --filter x` — argv → direct exec
    ///
    /// argv mode needs no shell quoting — prefer it when the
    /// command has arguments. Exit code is the child's.
    Run {
        /// Extra `VAR=cuora://ref` pairs to inject (refused when
        /// `[guardrails] deny_adhoc` is set).
        #[arg(short = 's', long = "secret", value_name = "VAR=CUORA_REF")]
        secrets: Vec<String>,
        /// Command to run (after `--`). One arg → `bash -c`;
        /// several → `argv[0]` exec'd directly with the rest.
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
    /// Print `export VAR='…'` lines for `eval "$(cuora env)"` — the
    /// human path (`run` is the agent path). Values print to
    /// stdout: `--json` emits a `VAR → value` map.
    Env {
        /// Extra `VAR=cuora://ref` pairs to resolve.
        #[arg(short = 's', long = "secret", value_name = "VAR=CUORA_REF")]
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
    /// (`[deploy.NAME]` in cuora.toml, e.g. a CF Secrets Store).
    /// The vault is the source of truth; the sink never gives the
    /// value back. With one configured target, `--target` may be
    /// omitted.
    Deploy {
        /// `cuora://` ref to push.
        rref: String,
        /// Deploy target name (`[deploy.NAME]`); required when
        /// several are configured.
        #[arg(long, value_name = "NAME")]
        target: Option<String>,
    },
    /// Serve the Vault port over ndjson — one JSON request per
    /// stdin line, one response per stdout line, audit events on
    /// stderr unchanged. For orchestrators embedding cuora
    /// (peregrin); with `[serve.pdp]` configured, a policy
    /// decision point is consulted before every vault-touching
    /// op. `{"op":"manifest"}` on the wire describes the protocol.
    Serve,
    /// Interactive vault browser (requires the `tui` feature).
    Ui,
    /// Capabilities, JSON shapes, env vars, audit contract — agent
    /// self-discovery (always JSON).
    Manifest,
    /// JSON Schema for `cuora.toml`.
    Schema,
    /// Print the CLI reference as markdown (docs pipeline —
    /// `bun run docs:cli` pipes it to `docs/reference/cli.md`).
    #[command(hide = true, name = "gen-cli-docs")]
    GenCliDocs,
}

fn main() -> ExitCode {
    sigpipe::reset();
    let cli = Cli::parse();
    let code = dispatch(&cli);
    // Buffered audit events export as OTLP spans here — every
    // exit path funnels through this one flush.
    cuora_ops::audit::flush();
    code
}

fn dispatch(cli: &Cli) -> ExitCode {
    match &cli.command {
        // Pure introspection — before config load, no audit event.
        Cmd::Manifest => json_out(&manifest()),
        Cmd::GenCliDocs => out(&clap_markdown::help_markdown::<Cli>()),
        Cmd::Schema => out(&schema_json()),
        cmd => {
            let op = cmd_op(cmd);
            let rref = cmd_rref(cmd);
            let ops = match load(cli) {
                Ok(ops) => ops,
                Err(e) => {
                    audit_fallback(cli, op, rref, Outcome::Error);
                    return fail(cli, &e.to_string());
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
                        cli,
                        &format!(
                            "op '{op}' denied by guardrails in {at} \
                             (advisory — remove/relax the rule to allow)"
                        ),
                    );
                }
            }
            match run_cmd(cmd, cli, &ops) {
                Ok(code) => code,
                Err(e) => {
                    let rref = norm_rref(&ops, rref);
                    ops.emit(op, rref.as_deref(), Outcome::Error);
                    fail(cli, &e.to_string())
                }
            }
        }
    }
}

/// The [`Op`] a subcommand maps to — what guardrails and audit
/// record.
fn cmd_op(cmd: &Cmd) -> Op {
    match cmd {
        Cmd::Init { .. } => Op::Init,
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
        Cmd::Serve => Op::Serve,
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
        .or_else(|| env_var("CUORA_ACTOR"))
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "anon".into());
    let env = cli
        .env
        .clone()
        .or_else(|| env_var("CUORA_ENV"))
        .unwrap_or_else(|| "dev".into());
    let rref = rref.map(|s| {
        Ref::new(s)
            .map(|r| r.path().to_string())
            .unwrap_or_else(|_| s.to_string())
    });
    cuora_ops::audit::emit(&AuditEvent::op(&actor, op, rref.as_deref(), &env, outcome));
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
        Cmd::Init { with_file } => {
            let info = ops.init(&cuora_ops::InitOpts {
                with_file: *with_file,
            })?;
            ops.emit(Op::Init, None, Outcome::Ok);
            if cli.json {
                return Ok(json_out(&serde_json::json!({
                    "identity": info.identity,
                    "backend": info.identity_backend,
                    "created": info.identity_created,
                    "migrated": info.migrated,
                    "file_remains": info.file_remains,
                    "recipient": info.recipient,
                    "vault": info.vault_path,
                })));
            }
            eprintln!(
                "identity: {}{}",
                info.identity,
                if info.identity_created {
                    " (generated)"
                } else {
                    ""
                }
            );
            // Requires a session secret store holding no identity —
            // unreachable on headless CI where platform() always
            // fails; exercised at ops level where sources inject.
            #[cfg(not(coverage))]
            if info.migrated {
                if let Some(p) = &info.file_remains {
                    eprintln!(
                        "migrated:  imported into the keyring; {} kept as a \
                         backup — remove it when ready",
                        p.display()
                    );
                }
            }
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
                cuora_shell::run_with_env(&cwd, &cmd[0], &exposed)?
            } else {
                cuora_shell::exec_with_env(&cwd, cmd, &exposed)?
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
            // A check that found problems exits 1 — the audit
            // outcome mirrors it, not a blanket `ok`.
            ops.emit(
                Op::Check,
                None,
                if report.problems.is_empty() {
                    Outcome::Ok
                } else {
                    Outcome::Error
                },
            );
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
                            cuora_core::GuardDefault::Allow => "allow",
                            cuora_core::GuardDefault::Deny => "deny",
                        },
                        "allow": g.allow.iter().map(|o| o.as_str()).collect::<Vec<_>>(),
                        "deny": g.deny.iter().map(|o| o.as_str()).collect::<Vec<_>>(),
                        "deny_adhoc": g.deny_adhoc,
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
        Cmd::Serve => {
            // Trust-root warning: a `[serve.pdp] exec` line is
            // config that authorizes shell exec. When it arrives
            // via the *implicit* ./cuora.toml search — not a pinned
            // --config/$CUORA_CONFIG and not the user's home
            // config — the working directory itself is the trust
            // root, so say so loudly before serving.
            let implicit_local = cli.config.is_none()
                && std::env::var_os("CUORA_CONFIG").is_none()
                && std::env::var_os("ARCA_CONFIG").is_none()
                && ops.config_path().is_some_and(|p| {
                    p == std::path::Path::new("cuora.toml")
                        || p == std::path::Path::new("arca.toml")
                });
            if implicit_local
                && ops
                    .config()
                    .serve
                    .as_ref()
                    .and_then(|s| s.pdp.as_ref())
                    .is_some()
            {
                eprintln!(
                    "warning: ./cuora.toml declares [serve.pdp] exec — \
                     this config is a trust root authorizing shell execution \
                     and was found on the implicit search path. \
                     Pin it with --config or $CUORA_CONFIG if this is intended."
                );
            }
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            cuora_serve::run(ops, &mut stdin.lock(), &mut stdout.lock())?;
            ops.emit(Op::Serve, None, Outcome::Ok);
            Ok(ExitCode::SUCCESS)
        }
        // Manifest/Schema/GenCliDocs are handled before ops load —
        // by dispatch only Ui reaches this arm, so the introspection
        // variants share it rather than own a provably-dead region.
        Cmd::Ui | Cmd::Manifest | Cmd::Schema | Cmd::GenCliDocs => {
            #[cfg(feature = "tui")]
            {
                cuora_tui::run(ops)?;
                ops.emit(Op::Ui, None, Outcome::Ok);
                Ok(ExitCode::SUCCESS)
            }
            #[cfg(not(feature = "tui"))]
            {
                eprintln!("`cuora ui` requires the `tui` feature (on by default)");
                Ok(ExitCode::FAILURE)
            }
        }
    }
}

fn parse_ref(s: &str) -> Result<Ref, OpsError> {
    Ok(s.parse::<Ref>()?)
}

/// `-s VAR=cuora://ref` pairs.
fn parse_pairs(raw: &[String]) -> Result<Vec<(String, String)>, OpsError> {
    raw.iter()
        .map(|s| {
            s.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .ok_or_else(|| OpsError::Msg(format!("-s wants VAR=cuora://ref, got {s:?}")))
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

/// Shell single-quote escape for `cuora env` output — `'` becomes
/// `'\''`, so any value is safe inside the quotes.
fn sh_escape(v: &str) -> String {
    format!("'{}'", v.replace('\'', "'\\''"))
}

/// The canonical record shape: resolved ref + env plus record
/// metadata. Callers add `value`/`ok` as needed — never by
/// default.
fn record_json(ops: &Ops, parsed: &Ref, rec: &cuora_vault::SecretRecord) -> serde_json::Value {
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
        "name": "cuora",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "secrets broker for agents — cuora:// refs in code/config, env-var injection at run time, audit on stderr + OTLP spans when OTEL_EXPORTER_OTLP_ENDPOINT is set",
        "repository": "https://github.com/9aia/cuora",
        "protocol": "cuora/1",
        "ops": cuora_core::Op::ALL.iter().map(|o| o.as_str()).collect::<Vec<_>>(),
        "verbs": {
            "init": "store the age identity in the session keyring when reachable (importing an existing keyfile), else the 0600 keyfile; verify the vault opens",
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
            "serve": "ndjson IPC mode for orchestrators — get/put/rm/rotate/list/check/manifest/shutdown on stdin→stdout, audit on stderr, optional [serve.pdp] policy consult",
            "ui": "interactive vault browser (tui feature)",
            "manifest": "this document (always JSON; before config load, no audit event)",
            "schema": "JSON Schema for cuora.toml (before config load)",
            "gen-cli-docs": "hidden — the CLI reference as markdown (docs pipeline)",
        },
        "flags": {
            "--json": "stable shapes below; errors are {\"error\": \"…\"} on stderr, non-zero exit",
            "--env ENV": "resolution context (dev/staging/prod/custom; overrides CUORA_ENV)",
            "--config PATH": "config override (else $CUORA_CONFIG → ./cuora.toml → ~/.config/cuora/cuora.toml)",
            "--actor ACTOR": "audit attribution (else CUORA_ACTOR → $USER → anon)",
            "-s VAR=cuora://ref": "ad-hoc injection pair on run/env (repeatable; refused under [guardrails] deny_adhoc)",
            "--with-file": "on `cuora init` — force the identity keyfile, skip the keyring",
            "--target NAME": "deploy target on `cuora deploy` (required when several [deploy.*] tables exist)",
            "--private-refs": "on `cuora migrate` — re-key cf-vault rows under HMAC-derived keys",
        },
        "json_shapes": {
            "init": {"identity": "path|keyring locator", "backend": "file|keyring", "created": true, "migrated": false, "file_remains": "path|null", "recipient": "age1…", "vault": "path"},
            "put": {"ok": true, "ref": "cuora://ns/name", "env": "dev", "version": 1, "created_at": "rfc3339", "rotated_at": "rfc3339|null"},
            "rotate": "same as put",
            "get": {"ref": "cuora://ns/name", "env": "dev", "value": "plaintext", "version": 1, "created_at": "rfc3339", "rotated_at": "rfc3339|null"},
            "list": [{"ref": "cuora://ns/name", "env": "dev", "version": 1, "created_at": "rfc3339", "rotated_at": "rfc3339|null"}],
            "rm": {"ok": true, "removed": true, "ref": "cuora://ns/name", "env": "dev"},
            "env": {"VAR": "plaintext value"},
            "check": {
                "config": "path|null", "env": "dev", "vault": "local",
                "vaults": [{"name": "local", "kind": "age", "path": "path|null", "ok": true, "error": "…|null"}],
                "secrets": {"VAR": {"ref": "cuora://ns/name", "env": "dev", "resolves": true}},
                "guardrails": {"default": "allow|deny", "allow": ["run"], "deny": ["get"], "deny_adhoc": false},
                "errors": ["finding…"],
            },
            "deploy": {"ok": true, "ref": "cuora://ns/name", "env": "dev", "target": "prod", "secret_name": "dev_ns_name", "created": true},
            "migrate": {"ok": true, "migrated": [{"vault": "cloud", "env": "prod", "rewritten": ["ns/name"], "already_private": 0}]},
            "run": "passthrough — child stdout/stderr/exit code; --json changes nothing",
            "error": {"error": "message (stderr, non-zero exit)"},
        },
        "run_modes": {
            "shell": "cuora run -- 'cmd | pipeline' — a single arg runs via bash -c",
            "argv": "cuora run -- prog arg… — multiple args exec argv[0] directly, no shell",
        },
        "env_vars": {
            "CUORA_CONFIG": "config file override",
            "CUORA_ENV": "env context (default dev)",
            "CUORA_ACTOR": "audit actor attribution",
            "CUORA_IDENTITY_FILE": "explicit identity keyfile path — pins file mode, skipping the session keyring",
            "CUORA_IDENTITY_BACKEND": "keyring (default, fail closed — no file fallback) | auto (keyring → keyfile fallback, headless) | file (never probe the keyring); wins over [identity] backend",
            "CUORA_CF_TOKEN": "default cf-vault bearer env var ([vaults.*] token_env overrides the name)",
            "CLOUDFLARE_API_TOKEN": "default deploy-token env var ([deploy.*] token_env overrides the name)",
            "CUORA_AUDIT_STDERR": "0/false/off/no silences the stderr ndjson audit wire (OTLP-only mode); unset keeps it",
            "OTEL_EXPORTER_OTLP_ENDPOINT": "OTLP/HTTP collector base — audit events export as cuora.<op> spans to <endpoint>/v1/traces (JSON)",
            "OTEL_EXPORTER_OTLP_HEADERS": "k=v,k2=v2 auth headers for the OTLP endpoint (values percent-decoded)",
            "OTEL_EXPORTER_OTLP_TIMEOUT": "export timeout in ms (default 10000)",
            "OTEL_SERVICE_NAME": "service.name resource attribute on exported spans (default cuora)",
            "OTEL_SDK_DISABLED": "true/1 disables OTLP export entirely",
        },
        "config": {
            "search": ["--config", "$CUORA_CONFIG", "./cuora.toml", "~/.config/cuora/cuora.toml"],
            "tables": ["vaults", "envs", "secrets", "deploy", "guardrails", "serve", "identity"],
            "schema": "cuora schema",
            "env_pin": "cuora://prod/ns/name pins the env regardless of context",
        },
        "vault_kinds": ["age", "cf"],
        "deploy_kinds": ["cf-secrets-store"],
        "serve": {
            "wire": "one JSON request per stdin line → one JSON response per stdout line; audit stays on stderr",
            "ops": ["get", "put", "rm", "rotate", "list", "check", "manifest", "shutdown"],
            "request": {"id": "any — echoed", "op": "…", "ref": "cuora://…", "env": "…", "value": "…", "actor": "claimed", "attestation": "opaque→PDP"},
            "error_codes": ["bad_request", "unknown_op", "denied", "pdp_unreachable", "not_found", "op_error"],
            "pdp": "[serve.pdp] exec = \"cmd…\" — sh -c per consulted op (get/put/rm/rotate/list/check; timeout_ms deadline, default 5000); request on stdin, {allow,reason?,obligations?} on stdout; unreachable fails CLOSED unless allow_on_error; child env is scrubbed to the cuora-shell allowlist",
            "details": "send {\"op\":\"manifest\"} on the wire for the full protocol doc",
        },
        "audit": {
            "wire": "ndjson lines on stderr (default; CUORA_AUDIT_STDERR=0 silences) + OTLP/HTTP JSON spans to $OTEL_EXPORTER_OTLP_ENDPOINT/v1/traces when set — span per event named cuora.<op>, attributes cuora.op/cuora.rref/cuora.env/cuora.actor/cuora.outcome (+cuora.policy.* on serve)",
            "event": {"ts": "rfc3339", "actor": "…", "op": "…", "rref": "…|absent when op-scoped", "env": "…", "outcome": "ok|denied|error", "policy?": "{allow, reason?, obligations?} — serve-path PDP verdicts only"},
            "shape": "per-ref event per materialized/mutated ref (put/get/rotate/rm/deploy, run/env); op-scoped event for init/list/check/ui; denied on guardrail refusal; error on op failure; manifest/schema emit none; a run child's exit code is not an cuora failure",
            "surfaces": "ui mutes live stderr for the session and replays held events on exit (alt-screen safety); serve flushes OTLP per request; CLI verbs flush once at exit; export failures warn once, never fail the op",
        },
        "features": {
            "tui": cfg!(feature = "tui"),
        },
        "notes": [
            "values never enter via CLI args — put/rotate read stdin or a hidden /dev/tty prompt",
            "[secrets] bounds what run/env may inject (least privilege); -s covers one-offs",
            "guardrails are advisory — accident prevention, not policy; CUORA_ACTOR is self-claimed",
            "run children start from an empty environment — only a small allowlist plus injected [secrets] cross; parent credentials (CUORA_*, deploy tokens) never leak",
            "list never carries values; get/env are the only surfaces that print them",
            "cf vaults store per-secret age ciphertext on a self-hosted Worker + D1 — the worker never sees plaintext",
            "cf wire is cuora-vault-cf/2: CuoraHmac signed requests (token never travels) with per-env scopes; bearer still accepted during migration",
            "[vaults.*] private_refs = true hides ref names from the cf store — HMAC-derived keys + encrypted name blobs; `cuora migrate --private-refs` rewrites existing rows",
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

/// The `cuora schema` payload — generated schema serializes
/// infallibly, same invariant as [`json_out`].
#[allow(clippy::unwrap_used)]
fn schema_json() -> String {
    serde_json::to_string_pretty(&schemars::schema_for!(cuora_core::Config))
        .expect("generated schema serializes")
}

/// Error path honoring `--json`: `{"error": …}` on stderr for
/// machines, `cuora: …` for humans.
fn fail(cli: &Cli, msg: &str) -> ExitCode {
    if cli.json {
        eprintln!("{}", serde_json::json!({ "error": msg }));
    } else {
        eprintln!("cuora: {msg}");
    }
    ExitCode::FAILURE
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn cmd_op_maps_every_variant() {
        // Introspection verbs return before ops load — cmd_op only
        // ever sees them if the dispatch order above is broken, but
        // the match still has to be total. All other arms map 1:1
        // to the audit [`Op`].
        let cases = [
            (Cmd::Init { with_file: false }, Op::Init),
            (
                Cmd::Put {
                    rref: "cuora://a/b".into(),
                },
                Op::Put,
            ),
            (
                Cmd::Get {
                    rref: "cuora://a/b".into(),
                },
                Op::Get,
            ),
            (Cmd::List, Op::List),
            (
                Cmd::Rm {
                    rref: "cuora://a/b".into(),
                },
                Op::Rm,
            ),
            (
                Cmd::Rotate {
                    rref: "cuora://a/b".into(),
                },
                Op::Rotate,
            ),
            (
                Cmd::Run {
                    secrets: vec![],
                    cmd: vec!["true".into()],
                },
                Op::Run,
            ),
            (Cmd::Env { secrets: vec![] }, Op::Env),
            (Cmd::Check, Op::Check),
            (Cmd::Migrate { private_refs: true }, Op::Migrate),
            (
                Cmd::Deploy {
                    rref: "cuora://a/b".into(),
                    target: None,
                },
                Op::Deploy,
            ),
            (Cmd::Ui, Op::Ui),
            (Cmd::Serve, Op::Serve),
            (Cmd::Manifest, Op::Manifest),
            (Cmd::Schema, Op::Schema),
            (Cmd::GenCliDocs, Op::Schema),
        ];
        for (cmd, op) in cases {
            assert_eq!(cmd_op(&cmd), op);
        }
    }

    #[test]
    fn run_cmd_folded_arm_takes_the_ui_path() {
        // The folded arm documents the dispatch invariant — an
        // introspection verb reaching run_cmd shares Ui's arm rather
        // than owning a dead region. Whatever it returns, the arm is
        // exercised; a non-tty env must never panic.
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
                ("CUORA_CONFIG", None::<&str>),
            ],
            || {
                let ops = Ops::load(LoadOpts::default()).unwrap();
                let _ = run_cmd(&Cmd::Manifest, &cli, &ops);
            },
        );
    }
}

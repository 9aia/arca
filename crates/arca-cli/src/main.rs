//! arca — secrets broker for agents. Sealed, not spilled.
//!
//! Code and config carry `arca://` references, never values; `arca
//! run` materializes declared secrets as env vars in the child only.
//! Every materialization emits an audit ndjson line on stderr.
//!
//! Values never enter via CLI args — `put`/`rotate` read stdin
//! (args land in shell history and `/proc`). `get` is the escape
//! hatch that prints a value to stdout; guardrails can deny it for
//! agents once enforcement lands.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Read, Write};
use std::process::ExitCode;

use arca_core::{Op, Ref, Secret};
use arca_ops::{LoadOpts, Ops};
use clap::{Parser, Subcommand};

/// arca — secrets broker for agents: `arca://` refs, redaction,
/// env-injection instead of plaintext.
#[derive(Parser)]
#[command(name = "arca", version, about, long_about = None)]
struct Cli {
    /// Emit JSON (accepted everywhere; default output for machines).
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
    /// verify the local vault opens.
    Init,
    /// Store a secret. The value comes from stdin — never args.
    Put {
        /// `arca://` ref, e.g. arca://billing/stripe.
        rref: String,
    },
    /// Print a secret's value to stdout.
    Get {
        /// `arca://` ref.
        rref: String,
    },
    /// List refs stored in the selected env.
    List,
    /// Remove a secret.
    Rm {
        /// `arca://` ref.
        rref: String,
    },
    /// Rotate a secret to a new value (stdin); bumps version +
    /// rotated_at.
    Rotate {
        /// `arca://` ref.
        rref: String,
    },
    /// Run a command with declared secrets injected as env vars.
    /// Declared surface = `[secrets]` in arca.toml plus `-s`
    /// overrides; nothing else may be injected.
    Run {
        /// Extra `VAR=arca://ref` pairs to inject.
        #[arg(short = 's', long = "secret", value_name = "VAR=ARCA_REF")]
        secrets: Vec<String>,
        /// The command line to run (after `--`).
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
    /// Print `export VAR='…'` lines for `eval "$(arca env)"` — the
    /// human path (`run` is the agent path).
    Env {
        /// Extra `VAR=arca://ref` pairs to resolve.
        #[arg(short = 's', long = "secret", value_name = "VAR=ARCA_REF")]
        secrets: Vec<String>,
    },
    /// Validate config + the declared secret surface; report the
    /// resolved routing. Exit 1 on findings.
    Check,
    /// Interactive vault browser (requires the `tui` feature).
    Ui,
    /// Capabilities + protocol version — agent self-discovery.
    Manifest,
    /// JSON Schema for `arca.toml`.
    Schema,
}

fn main() -> ExitCode {
    sigpipe::reset();
    let cli = Cli::parse();
    match &cli.command {
        Cmd::Manifest => json_out(&manifest()),
        Cmd::Schema => {
            match serde_json::to_string_pretty(&schemars::schema_for!(arca_core::Config)) {
                Ok(s) => out(&s),
                Err(e) => err(&format!("schema: {e}")),
            }
        }
        cmd => match load(&cli).and_then(|ops| run_cmd(cmd, &cli, &ops)) {
            Ok(code) => code,
            Err(e) => err(&e.to_string()),
        },
    }
}

fn load(cli: &Cli) -> Result<Ops, arca_ops::OpsError> {
    Ops::load(LoadOpts {
        config_path: cli.config.clone(),
        env: cli.env.clone(),
        actor: cli.actor.clone(),
    })
}

fn run_cmd(cmd: &Cmd, cli: &Cli, ops: &Ops) -> Result<ExitCode, arca_ops::OpsError> {
    match cmd {
        Cmd::Init => {
            let info = ops.init()?;
            if cli.json {
                return Ok(json_out(&serde_json::json!({
                    "vault": info.vault_path,
                    "identity": info.identity_path,
                    "identity_created": info.identity_created,
                    "recipient": info.recipient,
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
            let secret = read_secret_stdin()?;
            let rec = ops.put(&parse_ref(rref)?, secret)?;
            if cli.json {
                return Ok(json_out(&record_json(&rec)));
            }
            eprintln!("stored {rref} (v{})", rec.version);
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Get { rref } => match ops.get(&parse_ref(rref)?)? {
            Some(rec) => {
                if cli.json {
                    let mut v = record_json(&rec);
                    v["value"] = rec.value.expose().into();
                    return Ok(json_out(&v));
                }
                println!("{}", rec.value.expose());
                Ok(ExitCode::SUCCESS)
            }
            None => Ok(err(&format!("{rref} is not stored in env {}", ops.env()))),
        },
        Cmd::List => {
            let (env, refs) = ops.list(None)?;
            if cli.json {
                return Ok(json_out(&serde_json::json!({
                    "env": env.as_str(),
                    "refs": refs.iter().map(ToString::to_string).collect::<Vec<_>>(),
                })));
            }
            for r in refs {
                println!("{r}");
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Rm { rref } => {
            let removed = ops.rm(&parse_ref(rref)?)?;
            if cli.json {
                return Ok(json_out(&serde_json::json!({ "removed": removed })));
            }
            eprintln!("{}", if removed { "removed" } else { "not stored" });
            Ok(if removed {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Cmd::Rotate { rref } => {
            let secret = read_secret_stdin()?;
            let rec = ops.rotate(&parse_ref(rref)?, secret)?;
            if cli.json {
                return Ok(json_out(&record_json(&rec)));
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
            let status = arca_shell::run_with_env(&cwd, &cmd.join(" "), &exposed)?;
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
                if writeln!(w, "export {k}={}", sh_escape(v.expose())).is_err() {
                    return Ok(ExitCode::FAILURE);
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Check => {
            let report = ops.check();
            if cli.json {
                let ok = report.problems.is_empty();
                let code = json_out(&serde_json::json!({
                    "config": report.config_path,
                    "env": report.env.as_str(),
                    "vault": report.vault,
                    "secrets": report.secrets,
                    "problems": report.problems,
                }));
                if code != ExitCode::SUCCESS {
                    return Ok(code);
                }
                return Ok(if ok {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                });
            }
            match &report.config_path {
                Some(p) => eprintln!("config: {}", p.display()),
                None => eprintln!("config: (none found — defaults)"),
            }
            eprintln!(
                "env: {} → vault {:?} · {} declared secret(s)",
                report.env, report.vault, report.secrets
            );
            if report.problems.is_empty() {
                eprintln!("ok");
                Ok(ExitCode::SUCCESS)
            } else {
                for p in &report.problems {
                    eprintln!("problem: {p}");
                }
                Ok(ExitCode::FAILURE)
            }
        }
        Cmd::Ui => {
            #[cfg(feature = "tui")]
            {
                arca_tui::run(ops)?;
                Ok(ExitCode::SUCCESS)
            }
            #[cfg(not(feature = "tui"))]
            {
                eprintln!("`arca ui` requires the `tui` feature (on by default)");
                Ok(ExitCode::FAILURE)
            }
        }
        Cmd::Manifest | Cmd::Schema => unreachable!("handled before ops load"),
    }
}

fn parse_ref(s: &str) -> Result<Ref, arca_ops::OpsError> {
    Ok(s.parse::<Ref>()?)
}

/// `-s VAR=arca://ref` pairs.
fn parse_pairs(raw: &[String]) -> Result<Vec<(String, String)>, arca_ops::OpsError> {
    raw.iter()
        .map(|s| {
            s.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .ok_or_else(|| {
                    arca_ops::OpsError::Msg(format!("-s wants VAR=arca://ref, got {s:?}"))
                })
        })
        .collect()
}

/// Secrets arrive on stdin only — one trailing line terminator is
/// stripped, the rest is the value verbatim (newlines allowed).
fn read_secret_stdin() -> Result<Secret, arca_ops::OpsError> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Err(arca_ops::OpsError::Msg(
            "stdin is a terminal — pipe the value (secrets never enter via args)".into(),
        ));
    }
    let mut buf = String::new();
    stdin
        .lock()
        .read_to_string(&mut buf)
        .map_err(|e| arca_ops::OpsError::Msg(format!("read stdin: {e}")))?;
    let v = buf
        .strip_suffix("\r\n")
        .or_else(|| buf.strip_suffix('\n'))
        .unwrap_or(&buf);
    if v.is_empty() {
        return Err(arca_ops::OpsError::Msg("empty secret on stdin".into()));
    }
    Ok(Secret::new(v))
}

/// Shell single-quote escape for `arca env` output.
fn sh_escape(v: &str) -> String {
    format!("'{}'", v.replace('\'', "'\\''"))
}

fn record_json(rec: &arca_vault::SecretRecord) -> serde_json::Value {
    serde_json::json!({
        "version": rec.version,
        "created_at": rec.created_at.to_string(),
        "rotated_at": rec.rotated_at.map(|t| t.to_string()),
    })
}

fn manifest() -> serde_json::Value {
    serde_json::json!({
        "name": "arca",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": "arca/1",
        "ops": arca_core::Op::ALL.iter().map(|o| o.as_str()).collect::<Vec<_>>(),
        "features": {
            "tui": cfg!(feature = "tui"),
        },
        "env_vars": ["ARCA_CONFIG", "ARCA_ENV", "ARCA_ACTOR", "ARCA_IDENTITY_FILE"],
        "audit": "ndjson events on stderr",
    })
}

fn out(s: &str) -> ExitCode {
    println!("{s}");
    ExitCode::SUCCESS
}

fn json_out(v: &serde_json::Value) -> ExitCode {
    match serde_json::to_string_pretty(v) {
        Ok(s) => out(&s),
        Err(e) => err(&format!("json: {e}")),
    }
}

fn err(msg: &str) -> ExitCode {
    eprintln!("arca: {msg}");
    ExitCode::FAILURE
}

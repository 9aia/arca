//! Engine ops — `cli.<verb>` wire queries dispatch into the `cuora`
//! engine lib through this binary's own `run` verb: each call
//! re-execs `tamias-driver-cuora run <argv>` with the request's
//! params mapped to cuora's globals, so `cuora::dispatch` writes to
//! the child's captured stdout and never touches the serve loop's
//! ndjson stream — and every mutation keeps the engine's own
//! semantics (config search, audit events, guardrails).
//!
//! `ops` answers the ops manifest as JSON — *derived from the
//! engine's own clap tree* ([`cuora::command`]), so a verb can't
//! exist on the CLI and be missing here: declared args become
//! typed params (`arg`/`opt`/`flag`), doc strings ride along
//! unchanged.
//!
//! cuora has three verbs that can never be request/response ops —
//! they ride `run` passthrough / generated-surface subcommands
//! (inherited stdio) instead, and are listed in `surfaces`:
//!
//! - `serve` — the unbounded ndjson IPC loop; it never returns.
//! - `put`/`rotate` — bounded, but the secret *value* enters via
//!   stdin or a hidden `/dev/tty` prompt, never argv (args land in
//!   shell history and `/proc`). A wire op child (`Command::output`)
//!   has no usable stdin, so these verbs can only ever fail as ops.
//!   A `params-stdin` channel is org task T-008 — until it lands,
//!   secret entry stays on the streaming surfaces where stdin is
//!   inherited. No param ever carries a value: `put`/`rotate` take
//!   only the `cuora://` ref.
//!
//! `mutates` is honest: `init` writes the identity + verifies the
//! vault, `rm`/`migrate`/`deploy` rewrite vault or sink state, and
//! `run` spawns arbitrary child processes — side effects beyond
//! engine-state writes, so it gates like a mutator. `get`/`env`
//! materialize values into captured stdout (callers must treat op
//! output as secret); `list`/`check`/`manifest`/`schema` only read.
//! Streaming *flags* would join `SKIP_PARAMS` — none exist today.

use clap::{Arg, ArgAction, Command};
use pavo_protocol::wire::{QueryResult, Response};
use serde_json::{json, Map, Value};

/// Unbounded surfaces — verbs that never return (`serve`), plus
/// the stdin-bound secret-entry verbs (`put`/`rotate`, see module
/// docs). They can never be request/response ops.
const SURFACES: &[&str] = &["serve", "put", "rotate"];

/// Params excluded from ops — flags that turn a bounded verb into
/// a stream, or params that must never ride argv. None today:
/// secret *values* are never params (stdin/prompt only).
const SKIP_PARAMS: &[&str] = &[];

/// Verbs with side effects beyond reads — vault/identity/config
/// writes (`init`/`rm`/`migrate`/`deploy`) and arbitrary child exec
/// (`run`). Mutators belong behind a deliberate trigger, not
/// ambient view polling.
const MUTATES: &[&str] = &["init", "rm", "migrate", "deploy", "run"];

/// `(name, clap spec)` for every leaf verb. Hidden plumbing
/// (`gen-cli-docs`) is a docs-pipeline verb, not an engine op —
/// it stays reachable through `run` passthrough.
fn verbs() -> Vec<(String, Command)> {
    fn walk(prefix: &str, cmd: &Command, out: &mut Vec<(String, Command)>) {
        for sub in cmd.get_subcommands() {
            let name = sub.get_name();
            if name == "help" || sub.is_hide_set() {
                continue;
            }
            let dotted = match prefix {
                "" => name.to_string(),
                _ => format!("{prefix}.{name}"),
            };
            if sub.has_subcommands() {
                walk(&dotted, sub, out);
            } else {
                out.push((dotted, sub.clone()));
            }
        }
    }
    let mut out = Vec::new();
    walk("", &cuora::command(), &mut out);
    out
}

fn op_cmd(name: &str) -> Option<Command> {
    verbs()
        .into_iter()
        .find(|(n, _)| n == name && !SURFACES.contains(&n.as_str()))
        .map(|(_, c)| c)
}

/// Args that can't ride the wire: streaming flags (above), the
/// driver-resolved `--config` global, and clap's own help/version
/// actions.
fn skipped(a: &Arg) -> bool {
    SKIP_PARAMS.contains(&a.get_id().as_str())
        || a.get_id() == "config"
        || matches!(
            a.get_action(),
            ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong | ArgAction::Version
        )
}

/// A clap arg → the manifest's typed param kind — `arg` =
/// positional, `opt` = `--name <v>`, `flag` = `--name` — the
/// contract `pavo-driver-kit`'s surface layer renders args from.
fn kind(a: &Arg) -> &'static str {
    if a.is_positional() {
        "arg"
    } else if matches!(
        a.get_action(),
        ArgAction::SetTrue | ArgAction::SetFalse | ArgAction::Count
    ) {
        "flag"
    } else {
        "opt"
    }
}

fn param_json(a: &Arg) -> Value {
    // Wire name = the flag name (`--private-refs` → `private-refs`),
    // not the field id (`private_refs`) — generated surfaces spell
    // params `--<name>`.
    let id = a.get_id().as_str();
    let name = match a.is_positional() {
        true => id,
        false => a.get_long().unwrap_or(id),
    };
    json!({
        "name": name,
        "kind": kind(a),
        "doc": a.get_help().map(|d| d.to_string()).unwrap_or_default(),
    })
}

/// The `ops` query/`ops` argv verb payload — self-describing so
/// hosts and agents discover the engine surface without argv help.
/// `config`/`env`/`actor`/`json` are cuora's globals — accepted by
/// every op, listed once on the manifest itself; `config` is
/// driver-resolved and handed to the child as `--config`.
pub fn manifest() -> Value {
    json!({
        "engine": "cuora",
        "invoke": "query `cli.<verb>` with the listed params, or argv `tamias-driver-cuora run <cuora argv>`",
        "globals": [
            {"name": "config", "kind": "opt", "doc": "cuora.toml path — forwarded to the child as --config; absent, the CLI's own search applies ($CUORA_CONFIG → ./cuora.toml → ~/.config/cuora/cuora.toml)."},
            {"name": "env", "kind": "opt", "doc": "Env context — dev/staging/prod/custom (the CLI's --env global; overrides CUORA_ENV)."},
            {"name": "actor", "kind": "opt", "doc": "Audit actor attribution (the CLI's --actor global; else CUORA_ACTOR → $USER)."},
            {"name": "json", "kind": "flag", "doc": "Emit JSON — stable machine shapes on every verb (the CLI's --json global)."},
        ],
        "ops": verbs().iter()
            .filter(|(n, _)| !SURFACES.contains(&n.as_str()))
            .map(|(n, c)| json!({
                "query": format!("cli.{n}"),
                "doc": c.get_about().map(|d| d.to_string()).unwrap_or_default(),
                "params": c.get_arguments()
                    .filter(|a| !skipped(a))
                    .map(param_json)
                    .collect::<Vec<_>>(),
                "mutates": MUTATES.contains(&n.as_str()),
            }))
            .collect::<Vec<_>>(),
        "surfaces": SURFACES,
    })
}

/// Is `query` an engine op (`cli.<verb>`)? Returns the verb name.
pub fn op_of(query: &str) -> Option<String> {
    query
        .strip_prefix("cli.")
        .filter(|n| op_cmd(n).is_some())
        .map(str::to_string)
}

/// A wire scalar → its argv token (numbers/bools stringify).
fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// All values of a param — arrays flatten, scalars wrap.
fn values(v: &Value) -> Vec<String> {
    match v {
        Value::Array(xs) => xs.iter().filter_map(scalar).collect(),
        other => scalar(other).into_iter().collect(),
    }
}

/// `cli.<verb>` params → cuora argv (no argv[0] — `run` prepends
/// it). Globals come first: `config` resolves to `--config`, and
/// `env`/`actor`/`json` map to the CLI's globals by hand — clap
/// globals never appear in a subcommand's `get_arguments()`, so
/// the param loop below can't see them (same trick as mimus's
/// `--format`). Declared params map by kind — positionals land
/// last, in schema order.
fn argv(cmd: &Command, name: &str, params: &Map<String, Value>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(c) = params.get("config").and_then(scalar) {
        out.extend(["--config".into(), c]);
    }
    for (flag, key) in [("--env", "env"), ("--actor", "actor")] {
        if let Some(v) = params.get(key).and_then(scalar) {
            out.extend([flag.into(), v]);
        }
    }
    if params.get("json").and_then(Value::as_bool).unwrap_or(false) {
        out.push("--json".into());
    }
    out.extend(name.split('.').map(String::from));
    for a in cmd.get_arguments() {
        if skipped(a) || a.is_positional() {
            continue;
        }
        let id = a.get_id().as_str();
        let long = a.get_long().unwrap_or(id);
        let raw = params
            .get(long)
            .or_else(|| params.get(id))
            .or_else(|| params.get(id.replace('_', "-").as_str()));
        let Some(raw) = raw else { continue };
        match kind(a) {
            "flag" => {
                if raw.as_bool().unwrap_or(false) {
                    out.push(format!("--{long}"));
                }
            }
            _ => {
                for s in values(raw) {
                    out.push(format!("--{long}"));
                    out.push(s);
                }
            }
        }
    }
    // `last = true` positionals (`run`'s trailing command) only
    // parse after a `--` separator — everything before it is fair
    // game for flags, so the separator goes down here, after all
    // options are emitted.
    if cmd.get_positionals().any(|a| a.is_last_set()) {
        out.push("--".into());
    }
    let mut positionals: Vec<String> = Vec::new();
    for a in cmd.get_positionals() {
        if skipped(a) {
            continue;
        }
        let id = a.get_id().as_str();
        if let Some(raw) = params
            .get(id)
            .or_else(|| params.get(id.replace('_', "-").as_str()))
        {
            positionals.extend(values(raw));
        }
    }
    out.extend(positionals);
    out
}

/// The bare-name entry the embedded surfaces use (`OpSpec.name` is
/// `cli.`-stripped); the wire path goes `op_of` → `run_verb`.
#[cfg(feature = "mcp")]
pub fn run_op(op: &str, params: &Map<String, Value>) -> Response {
    match op_of(&format!("cli.{}", op.strip_prefix("cli.").unwrap_or(op))) {
        Some(name) => run_verb(&name, params),
        None => Response::err(format!("unknown op: {op}")),
    }
}

/// Run one engine op by name: re-exec `run` in a child with
/// captured output, and hand back `{op, argv, code, stdout,
/// stderr}` as the query payload. `ok:false` is reserved for spawn
/// failures — a nonzero `code` is the engine's own exit status
/// (e.g. `check` findings), honest data. The child's stdin is
/// null: secret-entry verbs (`put`/`rotate`) are surfaces, never
/// ops, precisely because they need it.
pub fn run_verb(name: &str, params: &Map<String, Value>) -> Response {
    let Some(cmd) = op_cmd(name) else {
        return Response::err(format!("unknown op: {name}"));
    };
    let args = argv(&cmd, name, params);
    let out = std::env::current_exe().and_then(|exe| {
        std::process::Command::new(exe)
            .arg("run")
            .args(&args)
            .output()
    });
    let data = match out {
        Ok(o) => json!({
            "op": name,
            "argv": args,
            "code": o.status.code().unwrap_or(-1),
            "stdout": String::from_utf8_lossy(&o.stdout),
            "stderr": String::from_utf8_lossy(&o.stderr),
        }),
        Err(e) => return Response::err(format!("run {name}: {e}")),
    };
    Response::ok_typed(&QueryResult {
        query: format!("cli.{name}"),
        data,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn op_of_accepts_cli_prefix_only() {
        assert_eq!(op_of("cli.list").unwrap(), "list");
        assert_eq!(op_of("cli.check").unwrap(), "check");
        assert!(op_of("cli.serve").is_none()); // unbounded surface
        assert!(op_of("cli.put").is_none()); // stdin-bound surface
        assert!(op_of("cli.rotate").is_none());
        assert!(op_of("list").is_none()); // view query, not an op
        assert!(op_of("cli.gen-cli-docs").is_none()); // hidden plumbing
        assert!(op_of("cli.bogus").is_none());
    }

    #[test]
    fn manifest_lists_every_non_surface_verb_as_cli_query() {
        let m = manifest();
        let ops = m["ops"].as_array().unwrap();
        for (n, _) in verbs()
            .iter()
            .filter(|(n, _)| !SURFACES.contains(&n.as_str()))
        {
            assert!(
                ops.iter().any(|o| o["query"] == json!(format!("cli.{n}"))),
                "{n} missing from ops manifest"
            );
        }
        assert_eq!(m["surfaces"], json!(SURFACES));
        for o in ops {
            for p in o["params"].as_array().unwrap() {
                assert!(["arg", "opt", "flag"].contains(&p["kind"].as_str().unwrap()));
                // streaming/stdin params never ride the wire
                assert!(!SKIP_PARAMS.contains(&p["name"].as_str().unwrap()));
                // --config is a driver-resolved global, never a per-op param
                assert_ne!(p["name"], json!("config"));
            }
        }
        // mutates honesty — the side-effect set is exactly these
        let mutators: Vec<&str> = ops
            .iter()
            .filter(|o| o["mutates"] == json!(true))
            .filter_map(|o| o["query"].as_str())
            .collect();
        assert_eq!(
            mutators,
            vec!["cli.init", "cli.rm", "cli.run", "cli.migrate", "cli.deploy"]
        );
    }

    #[test]
    fn argv_maps_params_to_cuora_flags() {
        let cmd = op_cmd("deploy").unwrap();
        let mut p = Map::new();
        p.insert("config".into(), json!("/cfg/cuora.toml"));
        p.insert("env".into(), json!("prod"));
        p.insert("actor".into(), json!("agent"));
        p.insert("json".into(), json!(true));
        p.insert("rref".into(), json!("cuora://billing/stripe"));
        p.insert("target".into(), json!("prod"));
        assert_eq!(
            argv(&cmd, "deploy", &p),
            vec![
                "--config",
                "/cfg/cuora.toml",
                "--env",
                "prod",
                "--actor",
                "agent",
                "--json",
                "deploy",
                "--target",
                "prod",
                "cuora://billing/stripe",
            ]
        );
    }

    #[test]
    fn argv_run_flattens_secret_pairs_and_cmd() {
        let cmd = op_cmd("run").unwrap();
        let mut p = Map::new();
        p.insert("secret".into(), json!(["A=cuora://a", "B=cuora://b"]));
        p.insert("cmd".into(), json!(["pnpm", "test"]));
        assert_eq!(
            argv(&cmd, "run", &p),
            vec![
                "run",
                "--secret",
                "A=cuora://a",
                "--secret",
                "B=cuora://b",
                "--",
                "pnpm",
                "test",
            ]
        );
    }

    #[test]
    fn argv_omits_globals_when_absent() {
        let cmd = op_cmd("list").unwrap();
        assert_eq!(argv(&cmd, "list", &Map::new()), vec!["list"]);
    }
}

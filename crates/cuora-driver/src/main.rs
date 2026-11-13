//! tamias-driver-cuora — the pavo view driver for cuora, plus the
//! engine ops surface.
//!
//! Verbs (the tamias driver contract):
//!
//! - `manifest` — print the [`DriverManifest`] JSON (discovery
//!   probe; never touches vault state).
//! - `capabilities` — `{"capabilities": ["pavo"], "verbs": […]}`.
//! - `ops` — the engine ops manifest: every bounded `cuora` CLI
//!   verb as a `cli.<verb>` wire query, derived from the engine's
//!   own clap tree ([`cuora::command`]). Unbounded/stdin-bound
//!   verbs (`serve`, `put`, `rotate`) are `surfaces`, never ops.
//! - `run <argv>` — engine passthrough: `tamias-driver-cuora run
//!   list` ≡ `cuora list`. Also the child side of `cli.*` wire
//!   queries (`ops::run_verb` re-execs this same verb so engine
//!   output never touches the serve stream).
//! - `cli <cuora argv>` — the tamias `bins[]` shim verb: `tamias
//!   bin exec` calls it with the standard `--dir`/`--name` driver
//!   flags ahead of the user's argv; they're stripped and the rest
//!   runs through `cuora::run_from` in-process with inherited
//!   stdio — full engine parity (`serve` streams, `put` reads
//!   stdin, globals/`--json`/`--version` all work).
//! - `serve` — ndjson JSON-RPC on stdio: `{"op":"manifest"}`,
//!   `{"op":"views"}`, `{"op":"view","view":…,"params":{…}}`,
//!   `{"op":"query","query":…,"params":{…}}` →
//!   `{"ok":true,"result":…}` / `{"ok":false,"error":…}`.
//!   `query:"ops"` answers the ops manifest; `query:"cli.<verb>"`
//!   runs the engine op.
//!
//! Optional surfaces: feature `mcp` embeds pavo-driver-kit's
//! surface layer — `mcp` serves the ops manifest as MCP tools on
//! stdio, equivalent to the standalone `tamias-driver-mcp` surface
//! driver minus the PATH/discovery hop. (`cli` needs no feature:
//! it's the bins-shim passthrough above, not a generated surface —
//! the engine's own argv grammar is the contract.)
//!
//! Target resolution: each `view`/`query` builds `cuora_ops::Ops`
//! from the request params — `config`/`env`/`actor` map to the
//! CLI's `--config`/`--env`/`--actor`; absent params fall through
//! to `$CUORA_CONFIG` → `./cuora.toml` → `~/.config/cuora/cuora.toml`,
//! `$CUORA_ENV`, `$CUORA_ACTOR`/`$USER` — identical to `cuora`
//! verbs. See `data.rs` for the secrets-safety contract: the view
//! side renders vault *metadata* only, never values. Ops are the
//! engine surface — `get`/`env` ops return values in captured
//! stdout by design (the escape hatches); `put`/`rotate`/`serve`
//! stay surfaces because they need stdin/never return.

mod data;
mod ops;
mod sealed;
#[cfg(feature = "mcp")]
mod surface;
mod views;

use pavo_driver_kit as kit;
use pavo_protocol::wire::{self, Request, Response, ViewSummary};
use serde_json::{json, Value};

fn capabilities_json() -> Value {
    let m = views::manifest();
    // The kit's shape plus the view count every sibling driver
    // advertises.
    let mut v = kit::capabilities_value(&m);
    v["views"] = json!(m.views.len());
    // Sealed-pack surface (tamias ADR-0004): the content kinds this
    // driver claims (`find_driver`'s capabilities fallback maps
    // `type: secrets` packs here) and the extra argv verbs
    // `tamias driver exec` may dispatch.
    v["types"] = json!(sealed::types());
    let mut verbs = m.verbs.clone();
    verbs.extend(sealed::verbs().iter().map(|s| s.to_string()));
    verbs.extend([
        "install".to_string(),
        "deploy".to_string(),
        "uninstall".to_string(),
    ]);
    v["verbs"] = json!(verbs);
    v
}

/// Dispatch one parsed request to its response.
fn dispatch(req: &Request) -> Response {
    match req {
        Request::Manifest => Response::ok_typed(&views::manifest()),
        Request::Views => {
            let summaries: Vec<ViewSummary> =
                views::specs().iter().map(ViewSummary::from).collect();
            Response::ok_typed(&summaries)
        }
        Request::View { view, params } => match views::view_result(view, params) {
            Ok(result) => Response::ok_typed(&result),
            Err(e) => Response::err(e),
        },
        Request::Render { kind, .. } => Response::err(format!(
            "tamias-driver-cuora claims no renderers (kind '{kind}')"
        )),
        Request::Query { query, params } => {
            // `ops` and `cli.<verb>` are engine ops; everything
            // else is a view query — unknown names fail the op,
            // degraded reads travel as `{"error": …}` data (the
            // marker renderers understand), matching the reference
            // driver's contract.
            if query == "ops" {
                Response::ok_typed(&wire::QueryResult {
                    query: query.clone(),
                    data: ops::manifest(),
                })
            } else if let Some(v) = ops::op_of(query) {
                ops::run_verb(&v, params)
            } else if views::known_query(query) {
                let mut ctx = data::Ctx::open(params);
                let payload = views::query_data(&mut ctx, query, params);
                Response::ok_typed(&wire::QueryResult {
                    query: query.clone(),
                    data: payload,
                })
            } else {
                Response::err(format!("unknown query: {query}"))
            }
        }
        // The protocol grew `submit`/`jobs`/`notifications` ops after
        // this driver was written; cuora mutations go through
        // `cli.<verb>`/`run`, never `submit`, so they answer
        // `ok:false` like any unsupported op.
        Request::Submit { .. } | Request::Jobs | Request::Notifications => {
            Response::err("tamias-driver-cuora implements no submit/jobs/notifications ops")
        }
    }
}

/// `cli` verb argv (after the `cli` token) → engine argv (argv[0]
/// included). `tamias bin exec`/`driver exec` prepend the standard
/// driver flags — `--dir <payload>` `--name <pkg>` — which the
/// engine doesn't know; consume the leading run of those two
/// (`--flag v` and `--flag=v` forms) and pass everything else
/// through untouched. `--config` is deliberately NOT stripped: it's
/// also a cuora global, so a user-passed `--config` must reach the
/// engine's own config search.
fn engine_argv(args: &[String]) -> Vec<String> {
    let mut out = vec!["cuora".to_string()];
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--dir" || a == "--name" {
            // flag + separate value — skip both (a trailing flag
            // with no value just ends the scan).
            i = (i + 2).min(args.len());
        } else if a.starts_with("--dir=") || a.starts_with("--name=") {
            i += 1;
        } else {
            break;
        }
    }
    out.extend(args[i..].iter().cloned());
    out
}

fn serve() -> i32 {
    // The kit owns the ndjson loop — stdin EOF and a dead client
    // both end it quietly, and either way the driver exits 0.
    let _ = kit::serve_stdio(dispatch);
    0
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("manifest") => {
            println!("{}", kit::manifest_json(&views::manifest()));
            0
        }
        Some("capabilities") => {
            println!(
                "{}",
                serde_json::to_string_pretty(&capabilities_json()).unwrap_or_default()
            );
            0
        }
        Some("ops") => {
            println!("{}", ops::manifest());
            0
        }
        // Engine passthrough — `tamias-driver-cuora run list` is
        // `cuora list`. Also the child side of `cli.*` wire
        // queries (ops::run_verb re-execs this same verb).
        Some("run") => {
            let argv = std::iter::once("cuora".to_string()).chain(args.iter().skip(1).cloned());
            std::process::exit(cuora::run_from(argv));
        }
        // The tamias `bins[]` shim verb (P-023): `tamias bin exec`
        // runs `<driver> cli --dir <payload> --name <pkg> <cuora
        // argv>` for a declared `shim: cli` bin. cuora's engine IS
        // the CLI, so this is a passthrough — strip the two standard
        // driver flags and hand the rest to `cuora::run_from` with
        // inherited stdio. That keeps full engine parity a generated
        // ops→CLI surface can't express: globals before the verb
        // (`--env`/`--config`/`--actor`), `--json` after it,
        // `--version`/`--help`, the unbounded `serve` ndjson loop,
        // and stdin-bound `put`/`rotate` all behave exactly like the
        // retired `cuora` binary.
        Some("cli") => {
            let argv = engine_argv(&args[1..]);
            std::process::exit(cuora::run_from(argv));
        }
        // Sealed packs (tamias ADR-0004): author-side `seal`, and
        // consumer-side `open`/`materialize`/`recipients`/`packs`
        // over installed packs — plus the `install`/`deploy`/
        // `uninstall` lifecycle no-ops tamias dispatches for
        // `type: secrets` packs.
        Some(verb @ ("seal" | "open" | "materialize" | "recipients" | "packs")) => {
            std::process::exit(sealed::run(verb, &args[1..]));
        }
        Some(verb @ ("install" | "deploy" | "uninstall")) => {
            std::process::exit(sealed::run(verb, &args[1..]));
        }
        // Optional embedded surface (feature-gated): the
        // pavo-driver-kit surface layer renders this driver's own
        // ops manifest as MCP tools over stdio — the same ops the
        // `cli.*` wire queries and the standalone
        // tamias-driver-mcp binary expose.
        #[cfg(feature = "mcp")]
        Some("mcp") => {
            let m = kit::surface::OpsManifest::from_value(&ops::manifest()).unwrap_or_default();
            match kit::surface::mcp::serve_stdio(&m, &mut surface::Engine) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("tamias-driver-cuora mcp: {e}");
                    1
                }
            }
        }
        Some("serve") | None => serve(),
        _ => {
            eprintln!(
                "usage: tamias-driver-cuora manifest|capabilities|ops|run <argv>|cli <cuora argv>|serve\n       sealed: seal|open|materialize|recipients|packs (+ install|deploy|uninstall no-ops)\n       (feature `mcp` adds `mcp`)"
            );
            1
        }
    };
    std::process::exit(code);
}

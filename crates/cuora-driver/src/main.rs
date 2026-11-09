//! tamias-driver-cuora — the pavo view driver for cuora.
//!
//! Verbs (the tamias driver contract):
//!
//! - `manifest` — print the [`DriverManifest`] JSON (discovery
//!   probe; never touches vault state).
//! - `capabilities` — `{"capabilities": ["pavo"], "verbs": […]}`.
//! - `serve` — ndjson JSON-RPC on stdio: `{"op":"manifest"}`,
//!   `{"op":"views"}`, `{"op":"view","view":…,"params":{…}}`,
//!   `{"op":"query","query":…,"params":{…}}` →
//!   `{"ok":true,"result":…}` / `{"ok":false,"error":…}`.
//!
//! Target resolution: each `view`/`query` builds `cuora_ops::Ops`
//! from the request params — `config`/`env`/`actor` map to the
//! CLI's `--config`/`--env`/`--actor`; absent params fall through
//! to `$CUORA_CONFIG` → `./cuora.toml` → `~/.config/cuora/cuora.toml`,
//! `$CUORA_ENV`, `$CUORA_ACTOR`/`$USER` — identical to `cuora`
//! verbs. See `data.rs` for the secrets-safety contract: this
//! driver renders vault *metadata* only, never values.

mod data;
mod views;

use pavo_driver_kit as kit;
use pavo_protocol::wire::{self, Request, Response, ViewSummary};

fn capabilities_json() -> serde_json::Value {
    let m = views::manifest();
    serde_json::json!({
        "capabilities": m.capabilities,
        "verbs": m.verbs,
        "views": m.views.len(),
    })
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
            // Unknown names fail the op; everything else answers
            // `ok:true` — degraded reads travel as `{"error": …}`
            // data (the marker renderers understand), matching the
            // reference driver's contract.
            if !views::known_query(query) {
                return Response::err(format!("unknown query: {query}"));
            }
            let mut ctx = data::Ctx::open(params);
            let payload = views::query_data(&mut ctx, query, params);
            Response::ok_typed(&wire::QueryResult {
                query: query.clone(),
                data: payload,
            })
        }
        // The protocol grew `submit`/`jobs`/`notifications` ops after
        // this driver was written; cuora is read-only with no feeds,
        // so they answer `ok:false` like any unsupported op.
        Request::Submit { .. } | Request::Jobs | Request::Notifications => {
            Response::err("tamias-driver-cuora implements no submit/jobs/notifications ops")
        }
    }
}

fn serve() -> i32 {
    // The kit owns the ndjson loop — stdin EOF and a dead client
    // both end it quietly, and either way the driver exits 0.
    let _ = kit::serve_stdio(dispatch);
    0
}

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("manifest") => {
            println!("{}", kit::manifest_json(&views::manifest()));
        }
        Some("capabilities") => {
            println!(
                "{}",
                serde_json::to_string_pretty(&capabilities_json()).unwrap_or_default()
            );
        }
        Some("serve") => std::process::exit(serve()),
        _ => {
            eprintln!("usage: tamias-driver-cuora manifest|capabilities|serve");
            std::process::exit(1);
        }
    }
}

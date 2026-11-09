//! views — the driver's manifest: every ViewSpec plus the wiring
//! that turns query bindings into [`ViewResult`] data.
//!
//! Read path: all data comes from [`crate::data::Ctx`] — one
//! `cuora_ops::Ops` per wire op, loaded exactly like the CLI
//! (`--config`/`--env`/`--actor` → the `config`/`env`/`actor`
//! request params). Nothing here proxies `cuora serve`: the reads
//! are in-process through `cuora-ops`, and the driver never mutates
//! — `put`/`rm`/`rotate`/`deploy` stay on the `cuora` verbs and the
//! `cuora serve` wire.
//!
//! **Secrets safety (hard requirement):** no view, query, or param
//! can render a secret *value*. Refs carry names only; `refs`/
//! `secret` read `SecretRecord`s for bookkeeping and drop `value`
//! uninspected; there is deliberately no reveal op — the TUI's
//! audited `r`→`r` reveal has no pavo equivalent by design.
//!
//! TUI → view mapping (the retirement checklist — `cuora-tui`'s
//! surfaces and where each landed):
//!
//! | TUI surface                          | pavo view / query                    |
//! |--------------------------------------|--------------------------------------|
//! | env tabs (dev/staging/prod + customs)| `cuora:envs` table, `cuora:tree`     |
//! | ref list + filter (`/`)              | `cuora:refs` table (`--param env=`)  |
//! | detail pane (ref, version, stamps)   | `cuora:secret` (`--param ref=`)      |
//! | detail pane vault kind/location      | `cuora:vaults` / `cuora:envs`        |
//! | "vault not readable" error line      | `{"error": …}` payloads in every view|
//! | `R` reload                            | every `view`/`query` is a fresh load |
//! | two-step `r`→`r` value reveal         | NOT PORTED — values never render     |
//! | `q`/navigation/filter keys            | host chrome (pavo owns interaction)  |
//! | `cuora check` verb                   | `cuora:check` view                    |
//! | `[secrets]` declared surface          | `cuora:declared` table                |
//! | `cuora ui` itself                     | `tamias-driver-cuora` (this crate)   |

use pavo_driver_kit as kit;
use pavo_protocol::{DriverManifest, NodeBase, ViewNode, ViewSpec};
use serde_json::{Map, Value};

use crate::data::{self, Ctx};

// ---------------------------------------------------------------- helpers

fn titled(title: &str) -> NodeBase {
    kit::base().title(title).build()
}

fn bound(title: &str, query: &str) -> NodeBase {
    kit::base().title(title).query(query).build()
}

fn col(title: &str, children: Vec<ViewNode>) -> ViewNode {
    kit::column(titled(title), children)
}

fn kv(title: &str, query: &str) -> ViewNode {
    kit::kv(bound(title, query), Vec::new())
}

fn table(title: &str, query: &str, columns: &[(&str, &str)]) -> ViewNode {
    // `sortable_table`: every table this driver declares sorts.
    kit::sortable_table(
        bound(title, query),
        columns
            .iter()
            .map(|(key, t)| kit::table_column(key, t))
            .collect(),
        Vec::new(),
    )
}

fn tree(title: &str, query: &str) -> ViewNode {
    kit::expandable_tree(bound(title, query), Vec::new())
}

fn spec(
    id: &str,
    title: &str,
    route: &str,
    description: &str,
    queries: &[&str],
    root: ViewNode,
) -> ViewSpec {
    let mut view = kit::view(id, title, route, root).description(description);
    for q in queries {
        view = view.query(kit::binding(q, "query"));
    }
    view.build()
}

// ---------------------------------------------------------------- manifest

/// Every view the driver declares — static (no vault state needed;
/// `manifest` must answer anywhere, even with no cuora config).
pub fn specs() -> Vec<ViewSpec> {
    vec![
        spec(
            "status",
            "Status",
            "/cuora/status",
            "Config/env/vault digest — where cuora.toml came from, the env context, and surface counts. Safe to poll; no vault I/O.",
            &["summary"],
            col("Cuora status", vec![kv("Cuora", "summary")]),
        ),
        spec(
            "vaults",
            "Vaults",
            "/cuora/vaults",
            "Every known vault — kind, path/url, cache scope, routed envs. Config-only; live readability lives in `cuora:check`.",
            &["vaults"],
            table(
                "Vaults",
                "vaults",
                &[
                    ("name", "Vault"),
                    ("kind", "Kind"),
                    ("location", "Location"),
                    ("cache", "Cache"),
                    ("envs", "Envs"),
                    ("configured", "Configured"),
                ],
            ),
        ),
        spec(
            "envs",
            "Envs",
            "/cuora/envs",
            "Env → vault routing — the TUI's tab strip as a table (dev/staging/prod plus [envs] customs).",
            &["envs"],
            table(
                "Envs",
                "envs",
                &[
                    ("env", "Env"),
                    ("well_known", "Well-known"),
                    ("vault", "Vault"),
                    ("kind", "Kind"),
                    ("location", "Location"),
                ],
            ),
        ),
        spec(
            "refs",
            "Refs",
            "/cuora/refs",
            "Stored refs + bookkeeping for one env — names, versions, timestamps. NEVER values. `--param env=` picks the env (default: selected env).",
            &["refs"],
            table(
                "Refs",
                "refs",
                &[
                    ("ref", "Ref"),
                    ("version", "Ver"),
                    ("created", "Created"),
                    ("rotated", "Rotated"),
                    ("vault", "Vault"),
                ],
            ),
        ),
        spec(
            "tree",
            "Tree",
            "/cuora/tree",
            "Env → refs hierarchy across every known env — the TUI's env tabs as an expandable tree. Per-env failures render inline.",
            &["tree"],
            tree("Vaults by env", "tree"),
        ),
        spec(
            "secret",
            "Secret",
            "/cuora/secret",
            "One ref's metadata — env, vault, version, timestamps, stored?. Requires --param ref=cuora://…. NEVER the value; there is no reveal verb.",
            &["secret"],
            kv("Secret", "secret"),
        ),
        spec(
            "declared",
            "Declared",
            "/cuora/declared",
            "The [secrets] injection surface — env-var name → cuora:// ref with resolved env and vault. Names only.",
            &["declared"],
            table(
                "Declared secrets",
                "declared",
                &[
                    ("var", "Var"),
                    ("ref", "Ref"),
                    ("resolved_ref", "Resolved"),
                    ("env", "Env"),
                    ("vault", "Vault"),
                ],
            ),
        ),
        spec(
            "check",
            "Check",
            "/cuora/check",
            "`cuora check` as a view — vault readability probe, declared-secret resolution, and findings. Probes live backends; may be slow for remote vaults.",
            &["check_info", "check_vaults", "check_secrets", "check_problems"],
            col(
                "Cuora check",
                vec![
                    kv("Check", "check_info"),
                    table(
                        "Vaults",
                        "check_vaults",
                        &[
                            ("name", "Vault"),
                            ("kind", "Kind"),
                            ("path", "Path"),
                            ("ok", "OK"),
                            ("error", "Error"),
                        ],
                    ),
                    table(
                        "Declared secrets",
                        "check_secrets",
                        &[
                            ("var", "Var"),
                            ("ref", "Ref"),
                            ("env", "Env"),
                            ("resolves", "Resolves"),
                        ],
                    ),
                    table("Problems", "check_problems", &[("problem", "Problem")]),
                ],
            ),
        ),
    ]
}

/// The `manifest` op result.
pub fn manifest() -> DriverManifest {
    // `contribution` defaults priority to 100; the slots below order
    // explicitly (lower sorts first).
    let mut navbar = kit::contribution("refs", "Cuora", "/cuora/refs");
    navbar.priority = 30;
    let mut dashboard = kit::contribution("status", "Cuora status", "/cuora/status");
    dashboard.priority = 35;
    let mut statusbar = kit::contribution("status", "Cuora vaults", "/cuora/status");
    statusbar.priority = 45;

    // `Manifest::new` already defaults verbs to ["manifest","serve"]
    // and capabilities to ["pavo"] — the same values the literal
    // declared by hand.
    let mut m = kit::Manifest::new("cuora", "Cuora")
        .version(env!("CARGO_PKG_VERSION"))
        .icon("🐢")
        .navbar(navbar)
        .dashboard(dashboard)
        .statusbar(statusbar);
    for v in specs() {
        m = m.view(v);
    }
    m.build()
}

// ---------------------------------------------------------------- queries

/// Every query name the driver answers — the `query` op rejects
/// anything outside this set with `ok:false`.
pub fn known_query(name: &str) -> bool {
    matches!(
        name,
        "summary"
            | "vaults"
            | "envs"
            | "declared"
            | "refs"
            | "tree"
            | "secret"
            | "check_info"
            | "check_vaults"
            | "check_secrets"
            | "check_problems"
    )
}

/// `{"op":"query","query":name,"params":{…}}` → the data payload.
/// Unknown names return an `{"error": …}` object (defensive — the
/// wire layer rejects them before this runs).
pub fn query_data(ctx: &mut Ctx, name: &str, params: &Map<String, Value>) -> Value {
    if !known_query(name) {
        return data::error_payload(format!("unknown query '{name}'"));
    }
    crate::data::query_data(ctx, name, params)
}

// ---------------------------------------------------------------- view op

/// `{"op":"view","view":id,"params":{…}}` → [`pavo_protocol::wire::ViewResult`].
///
/// One [`Ctx`] (one `Ops` load) feeds every binding in the view —
/// `cuora:check`'s four bindings share a single `ops.check()` probe
/// via the context's memo.
pub fn view_result(
    view_id: &str,
    params: &Map<String, Value>,
) -> Result<pavo_protocol::wire::ViewResult, String> {
    let spec = specs()
        .into_iter()
        .find(|v| v.id == view_id)
        .ok_or_else(|| format!("unknown view: {view_id}"))?;

    let mut ctx = Ctx::open(params);
    let mut data_map = Map::new();
    for b in &spec.queries {
        // static binding params first, caller params override
        let mut merged = b.params.clone();
        for (k, v) in params {
            merged.insert(k.clone(), v.clone());
        }
        let payload = query_data(&mut ctx, &b.name, &merged);
        data_map.insert(b.name.clone(), payload);
    }

    Ok(pavo_protocol::wire::ViewResult {
        view: spec,
        data: data_map,
    })
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn manifest_advertises_pavo_and_lists_every_view() {
        let m = manifest();
        assert!(m.verbs.iter().any(|v| v == "serve"));
        assert!(m.capabilities.iter().any(|c| c == "pavo"));
        let ids: std::collections::BTreeSet<&str> = m.views.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids.len(), m.views.len(), "duplicate view ids");
        for want in [
            "status", "vaults", "envs", "refs", "tree", "secret", "declared", "check",
        ] {
            assert!(ids.contains(want), "missing view {want}");
        }
    }

    #[test]
    fn every_binding_is_a_known_query() {
        for v in specs() {
            for b in &v.queries {
                assert!(known_query(&b.name), "{}: unknown query {}", v.id, b.name);
            }
        }
    }

    #[test]
    fn no_view_can_bind_a_value_bearing_query() {
        // The secrets-safety contract, asserted structurally: the
        // query set is exactly the metadata surface — there is no
        // `get`/`reveal`/`value` binding a view could accidentally
        // wire up.
        for name in [
            "get",
            "reveal",
            "value",
            "secret_value",
            "materialize",
            "inject",
        ] {
            assert!(!known_query(name), "{name} must not be a query");
        }
    }

    #[test]
    fn degraded_ctx_yields_error_payloads_not_wire_errors() {
        // A config path that can't load → every query degrades to
        // {"error": …} inside ok:true data.
        let mut params = Map::new();
        params.insert("config".into(), json!("/nonexistent/cuora.toml"));
        let mut ctx = Ctx::open(&params);
        for name in [
            "summary", "vaults", "envs", "declared", "refs", "tree", "secret",
        ] {
            let v = query_data(&mut ctx, name, &Map::new());
            assert!(
                v.get("error").is_some() || v.get("items").is_some(),
                "{name}: {v}"
            );
        }
        // tree degrades per-env (envs() still lists well-known) —
        // each node carries an unreadable detail, not a crash.
        let tree = query_data(&mut ctx, "tree", &Map::new());
        let items = tree["items"].as_array().unwrap();
        assert!(!items.is_empty());
        for item in items {
            let detail = item["detail"].as_str().unwrap_or("");
            assert!(detail.contains("unreadable"), "{detail}");
        }
    }

    #[test]
    fn secret_query_requires_ref_param() {
        let ctx_ok = Ctx::open(&Map::new());
        let mut ctx = ctx_ok;
        let v = query_data(&mut ctx, "secret", &Map::new());
        assert!(v["error"].as_str().unwrap().contains("ref"), "{v}");
    }

    #[test]
    fn view_result_echoes_spec_and_fills_bindings() {
        let mut params = Map::new();
        params.insert("config".into(), json!("/nonexistent/cuora.toml"));
        let r = view_result("status", &params).unwrap();
        assert_eq!(r.view.id, "status");
        assert!(r.data.contains_key("summary"));
        assert!(r.data["summary"].get("error").is_some());
        assert!(view_result("nope", &params).is_err());
    }
}

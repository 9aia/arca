# Contributing

Issues and PRs are **disabled** — the project is agent-maintained and
dogfoods its own PM ledger (`project/`). Fork freely under MIT.

## Layout

`crates/` — flat rust workspace, ports-and-adapters. `cuora-core` is
pure domain (no I/O); `cuora-config`/`cuora-vault`/`cuora-deploy` are
ports, `-toml`/`-age`/`-cf` crates their impls; `cuora-ops` is the ops
shell; `cuora` (`crates/cuora-cli`) is the composition root. `xtask/`
is repo chores.

## Rules of thumb

- New source/sink = a new impl crate, not a core change.
- `cargo xtask check` and `cargo xtask test` must stay green.
- Coverage: `cargo xtask coverage` — cargo-llvm-cov HTML report in
  `target/llvm-cov/html`; pass `--fail-under-lines 100` to gate.
- Keep the binary dependency-light and sync; no async runtime.
- Commits: conventional prefixes (`feat:`/`fix:`/`refactor:`/`docs:`/`chore:`).
- Never commit secrets — `cuora.toml` takes names of env vars, not
  values; no plaintext secret in tests, fixtures, docs, or logs.
- Never hand-edit `project/` when a `myco` verb exists — the ledger
  is myco's, verbs are the audit trail.

## Releasing

No crates.io publish, no git tags — the install path is the tamias
pack (`libraries/cuora`): `cargo xtask pack` stages the compiled
`tamias-driver-cuora` payload and `tamias install
--allow-drivers ./libraries/cuora` deploys the generated `cuora`
shim. The `cuora`/`tamias-driver-cuora` crates still build under
plain `cargo build`/`cargo test` — the `mcp` surface stays
feature-gated and nothing optional is required.

## Docs

Follow Diátaxis: `docs/tutorials/` (learn) · `docs/how-to/` (tasks) ·
`docs/reference/` (lookup) · `docs/explanation/` (why). API reference
is generated from doc comments — keep `lib.rs` module docs accurate;
`docs/gen-*.py` renders cli/config/crate-map from source.

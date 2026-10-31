# Contributing

Issues and PRs are **disabled** — the project is agent-maintained and
dogfoods its own PM ledger (`project/`). Fork freely under MIT.

## Layout

`crates/` — flat rust workspace, ports-and-adapters. `arca-core` is
pure domain (no I/O); `arca-config` is a port, `arca-config-toml` its
file impl; `arca-git` and `arca-shell` shell out; `arca`
(`crates/arca-cli`) is the composition root. `xtask/` is repo chores.

## Rules of thumb

- New source/sink = a new impl crate, not a core change.
- `cargo xtask check` and `cargo xtask test` must stay green.
- Coverage: `cargo xtask coverage` — cargo-llvm-cov HTML report in
  `target/llvm-cov/html`; pass `--fail-under-lines 100` to gate.
- Keep the binary dependency-light and sync; no async runtime.
- Commits: conventional prefixes (`feat:`/`fix:`/`refactor:`/`docs:`/`chore:`).
- Never commit secrets — `arca.toml` takes names of env vars, not values.
- Never hand-edit `project/` when a `arca` verb exists — verbs are the
  audit trail.

## Releasing

`scripts/release.sh` runs on `main` pushes via bupha's dogfood loop —
bump, publish, tag (`{crate}-v{ver}`), dist binaries. See
`docs/explanation/release-model.md` once the docs site lands.

## Docs

Follow Diátaxis: `docs/tutorials/` (learn) · `docs/how-to/` (tasks) ·
`docs/reference/` (lookup) · `docs/explanation/` (why). API reference
is generated from doc comments — keep `lib.rs` module docs accurate;
`docs/gen-*.py` renders cli/config/crate-map from source.

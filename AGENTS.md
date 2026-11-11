# Cuora

Secrets broker for agents — sealed, not spilled. Named for the box
turtle *Cuora* — it seals itself with a hinged shell. Code and
config carry `cuora://` references, never values; the `cuora` CLI
materializes secrets as env vars at run time; every
materialization emits an audit event — ndjson on stderr, plus
OTLP spans when `OTEL_EXPORTER_OTLP_ENDPOINT` is set. The skill
(`.agents/skills/`) teaches agents the verbs, not file formats.

<!-- cuora:agent-awareness -->

<!-- myco:agent-awareness -->

## Agent awareness

- Session start / any work discussion: run `myco status --prompt`
  before proposing next steps — know the board first.
- User describes new work → offer to capture it (`myco todo add`,
  `issue new`, `proposal new`). Don't wait to be asked.
- Picking up a task: `myco resume <ID>` is the briefing.
- Wrapping up: `myco handoff <ID>` anything resumable; keep statuses
  honest (claimed → in-progress → blocked/done).

<!-- /myco:agent-awareness -->

<!-- sturn:agent-awareness -->

## Sturn awareness

- **No PRs.** The pushed branch/commit is the deliverable; review
  streams continuously (sturn WAL), merge is a human/sturn call.
- sturn-wired repo (`sturn.toml` present): `sturn claim "<glob>"
  --ws <ws>` *before* edits — claims surface conflicts in ms.
- Ops via `sturn write|rename|delete`; `sturn snap` folds the ws
  into a real commit; `sturn push` publishes heads + refs/sturn/*.
- `sturn leave` on exit — drops the actor's claims.

<!-- /sturn:agent-awareness -->

<!-- kuda:agent-awareness -->

## Kuda awareness

- `.kuda/` cards are committed artifacts — read them before
  re-exploring (`kuda map`, `kuda card <mod>`); `kuda status`
  reports per-card drift vs HEAD.
- Code changes that invalidate a card → regenerate it (`kuda
  sync`) in the same change; cards ride the diff they describe.
- `.kuda/index.sqlite` is gitignored + rebuildable; the `*.yaml`
  cards are the checked-in source of truth.

<!-- /kuda:agent-awareness -->


## Layout

Ports-and-adapters, flat `crates/` workspace. `cuora-core` is pure
domain (no I/O); ports are traits, impls are separate crates;
`crates/cuora-cli` (package `cuora`) is the engine lib. There is no
hand-rolled `cuora` binary: the command ships as the tamias pack in
`libraries/cuora/` — a generated `bins[]` shim →
`tamias-driver-cuora`'s `cli` passthrough → `cuora::run_from`
(P-023 pilot).

| Crate | Role |
| --- | --- |
| `crates/cuora-core` | `cuora://` refs, `Secret` redacting newtype, env axis (dev/staging/prod), actor attribution — pure, no I/O |
| `crates/cuora-config` | Config port — where `cuora.toml`-shaped config comes from |
| `crates/cuora-config-toml` | Config impl — explicit path → `$CUORA_CONFIG` → `./cuora.toml` → `~/.config/cuora/` |
| `crates/cuora-vault` | Vault port — readable secret stores: `get/put/list/rotate`, per-env variants |
| `crates/cuora-vault-age` | Local vault impl — age-encrypted file (v1) |
| `crates/cuora-vault-cf` | Remote vault impl — self-hosted CF Worker + D1, per-secret client-side age ciphertext (`worker/` has the deployable JS) |
| `crates/cuora-vault-cached` | Composite vault — read-through age-encrypted cache wrapping any impl; `cache`/`cache_ttl_secs` on `[vaults.*]` (P-007) |
| `crates/cuora-deploy` | Deploy port — write-only push targets (sinks, never sources) |
| `crates/cuora-deploy-cf` | Deploy impl — CF account Secrets Store via API v4 |
| `crates/cuora-shell` | `bash -c <cmd>` + env injection — the `cuora run` path |
| `crates/cuora-ops` | Ops I/O — vault open/save, ref resolution, audit events, verb plumbing |
| `crates/cuora-notice` | Process-wide `cuora:` stderr notices — muted/replayed for terminal-owning surfaces |
| `crates/cuora-testkit` | Vault conformance suite — every impl passes the same contract tests |
| `crates/cuora-driver` | `tamias-driver-cuora` — pavo view driver (read-only vault *metadata*; never renders values) + engine ops surface (`ops`/`run`/`cli.*` wire queries; `cli` = the bins-shim engine passthrough, `mcp` feature-gated); replaced the TUI |
| `crates/cuora-serve` | ndjson IPC surface — thin front-end over cuora-ops; `cuora serve`, the `[serve.pdp]` exec consult (ADR-0003) |
| `crates/cuora-cli` | The `cuora` engine lib — clap, `--json` everywhere, `manifest`/`schema`; drivers consume `cuora::command()`/`parse_from()`/`dispatch()`/`run_from()`. No `[[bin]]` — e2e tests live in `cuora-driver/tests/` and spawn `tamias-driver-cuora run` |
| `libraries/cuora` | The tamias pack — `tamias.yaml` declares `bins: [{name: cuora, shim: cli, driver: cuora}]`; `cargo xtask pack` stages `drivers/bin/<triple>/tamias-driver-cuora` (gitignored) |
| `xtask` (pkg `cuora-xtask`) | check / test / worker / coverage / pack / install / site |

`cuora-vault-aws` stays a spike — proved on a local branch that the
`Vault` port fits a second backend family unchanged, then deleted
per PL-001. No `cuora-sdk` by default — env vars are the interface;
revisit only if a Rust-native embed is ever needed.

## Boundary

cuora owns **secret materialization** — refs, vaults, env
injection, redaction, audit. Neighbors own theirs:

- **myco** — work memory; this repo's `project/` ledger is myco's.
- **sturn** — ephemeral coordination (live claims/ops/topics).
- **kuda** — code memory.
- **peregrin** — orchestration; campaigns consume secrets via
  `cuora run`; embedding + the policy-decision wire is `cuora serve`
  (ndjson, ADR-0003) — cuora consults, peregrin decides.
- **bupha** — CI gate (was: asserts no plaintext secrets land in
  pipelines); bupha is the CI repo again — the org gate is
  mid-migration back from the peregrin `ci-gate` campaign (PL-013).
- **mimus** — critic; secrets resolve env → cuora → legacy
  (keyring, `~/.config/mimus/env`) via its `mimus-cuora` adapter
  shelling `cuora` — vaults are canonical, writes land in them.
- **sepia** — session lifecycle.

A hosted vault service is in scope (it's a store impl);
multi-actor arbitration or a daemon belongs to peregrin.

## Commands

```bash
cargo xtask check          # fmt + clippy -D warnings
cargo xtask test           # whole suite (nextest if present);
                           # + worker smoke test when bun is installed
cargo xtask worker         # bun test on the real worker/index.js
                           # (bun:sqlite D1 shim)
cargo xtask pack           # stage libraries/cuora driver payload
                           # (release tamias-driver-cuora → drivers/bin/<triple>/)
cargo xtask install        # release-install tamias-driver-cuora →
                           # ~/.cargo/bin (the `cuora` command itself is
                           # the tamias pack's generated bins[] shim —
                           # `cargo xtask pack` + `tamias install
                           # --allow-drivers ./libraries/cuora`)
cargo xtask coverage       # llvm-cov report (HTML → target/llvm-cov/html);
                           # extra args pass through, e.g. --fail-under-lines 100
cargo xtask site           # build the docs site (needs docs/ worktree)
```

Git hooks live in `.hooks/` (`core.hooksPath` = `.hooks`):
pre-commit runs `cargo fmt --check`, pre-push `cargo xtask check`.

## The worktree anatomy

`project/` and `docs/` are git worktrees of orphan branches —
`project` (the PM ledger, created by `myco init`), `docs` (the
VitePress site → GitHub Pages). Both are gitignored on `main`; never
commit their contents to `main`.

## Rules

- Workspace lints are shared (`Cargo.toml [workspace.lints]`):
  `unsafe_code` forbidden, `unwrap_used` warned — `-D warnings` in CI
  means no warnings tolerated. Tests `#![allow]` liberally.
- No `unsafe`. Edition 2021.
- Keep the binary dependency-light and sync; no async runtime.
  Vault impls introduce HTTP — prefer blocking clients; async only
  behind a feature if an embeddable SDK is ever added.
- Secret values never appear in tests, fixtures, `project/`, logs,
  or docs — fixtures use fake values.
- `Secret` never exposes plaintext: no `Display`/`Debug` of the
  value; explicit `.expose()` is the only way out.
- Every materialization is auditable: actor, ref, env, outcome,
  timestamp — ndjson on stderr + OTLP span export when
  `OTEL_EXPORTER_OTLP_ENDPOINT` is configured; terminal-owning
  surfaces can mute live stderr and replay on exit.
- Secrets never enter via CLI args (args land in shell history and
  `/proc`) — stdin, prompt, or keychain only.
- Agents never hand-edit `project/` when a `myco` verb exists.
- `.agents/skills/` is the canonical skill location;
  `skills-lock.json` pins upstream skills.

## Docs

`docs/` worktree — VitePress + Diátaxis (tutorials/how-to/reference/
explanation) → GitHub Pages at `9aia.com/cuora`. `docs/gen-*.py`
renders cli/config/crate-map from source — keep `lib.rs` module docs
accurate; they feed the generated reference.

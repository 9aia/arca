# Arca

Secrets broker for agents — sealed, not spilled. Named for *Arca
noae* (the ark clam) and Latin *arca*, the strongbox. Code and
config carry `arca://` references, never values; the `arca` CLI
materializes secrets as env vars at run time; every
materialization hits the audit ledger. The skill
(`.agents/skills/`) teaches agents the verbs, not file formats.

<!-- arca:agent-awareness -->

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

## Layout

Ports-and-adapters, flat `crates/` workspace. `arca-core` is pure
domain (no I/O); ports are traits, impls are separate crates;
`crates/arca-cli` (package `arca`) is the composition root.

| Crate | Role |
| --- | --- |
| `crates/arca-core` | `arca://` refs, `Secret` redacting newtype, env axis (dev/staging/prod), actor/policy model — pure, no I/O |
| `crates/arca-config` | Config port — where `arca.toml`-shaped config comes from |
| `crates/arca-config-toml` | Config impl — explicit path → `$ARCA_CONFIG` → `./arca.toml` → `~/.config/arca/` |
| `crates/arca-vault` | Vault port — readable secret stores: `get/put/list/rotate`, per-env variants |
| `crates/arca-vault-age` | Local vault impl — age-encrypted file (v1) |
| `crates/arca-shell` | `bash -c <cmd>` + env injection — the `arca run` path |
| `crates/arca-ops` | Ops I/O — vault open/save, ref resolution, audit ledger, verb plumbing |
| `crates/arca-testkit` | Golden fixtures + conformance — every vault impl passes the same contract tests |
| `crates/arca-tui` | Interactive vault TUI (ratatui) — thin front-end over arca-ops; `arca ui`, feature-gated |
| `crates/arca-cli` | The `arca` binary — clap, `--json` everywhere, `manifest`/`schema` |
| `xtask` | check / test / coverage / install / site |

Planned: `arca-vault-cf` (self-hosted Worker + D1, client-side
age-encrypted), `arca-deploy` + `arca-deploy-cf` (write-only push
targets — CF Secrets Store, Workers secrets), `arca-vault-aws`.
No `arca-sdk` by default — env vars are the interface; revisit
only if a Rust-native embed is ever needed.

Scaffold note: the workspace was copied from the `myco` anatomy —
`arca-git`, `arca-index(-redb)`, `arca-store(-md)` are vestigial
renames of the PM stack, kept or re-mapped per
`project/proposals/`.

## Boundary

arca owns **secret materialization** — refs, vaults, env
injection, redaction, audit. Neighbors own theirs:

- **myco** — work memory; this repo's `project/` ledger is myco's.
- **sturn** — ephemeral coordination (live claims/ops/topics).
- **kuda** — code memory.
- **peregrin** — orchestration; campaigns consume secrets via
  `arca run`. A future `arca serve` (ndjson) is a later ADR.
- **bupha** — CI gate; asserts no plaintext secrets land in
  pipelines.
- **mimus** — critic; its env → keyring → `~/.config/mimus/env`
  resolution migrates to arca vaults.
- **sepia** — session lifecycle.

A hosted vault service is in scope (it's a store impl);
multi-actor arbitration or a daemon belongs to peregrin.

## Commands

```bash
cargo xtask check          # fmt + clippy -D warnings
cargo xtask test           # whole suite (nextest if present)
cargo xtask install        # release-install arca → ~/.cargo/bin
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
- Every materialization is auditable: actor, ref, env, timestamp.
- Secrets never enter via CLI args (args land in shell history and
  `/proc`) — stdin, prompt, or keychain only.
- Agents never hand-edit `project/` when a `myco` verb exists.
- `.agents/skills/` is the canonical skill location;
  `skills-lock.json` pins upstream skills.

## Docs

`docs/` worktree — VitePress + Diátaxis (tutorials/how-to/reference/
explanation) → GitHub Pages at `9aia.com/arca`. `docs/gen-*.py`
renders cli/config/crate-map from source — keep `lib.rs` module docs
accurate; they feed the generated reference.

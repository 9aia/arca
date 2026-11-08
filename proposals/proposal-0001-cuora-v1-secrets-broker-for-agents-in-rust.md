+++
id = "P-001"
title = "cuora v1 — secrets broker for agents, in Rust"
status = "open"
created = "2026-10-31"
owner = "human:luis"
priority = "high"
+++

Status: **proposal v1**. Replaces nothing — greenfield repo scaffolded
from the myco anatomy (ports/adapters, xtask, worktrees, hooks,
AGENTS/README/docs skeletons). Ambition: agents never see plaintext —
secrets arrive as env vars; every materialization is audited.

## Problem

Agents (and CI, scripts, machines) need API keys, but every
plaintext handoff is a leak surface: prompts, tool output, logs,
`.env` pasted into configs, hardcoded literals. arca removes the
handoff: code carries `arca://` references, never values.

## Locked decisions

- **Reference model** — `arca://name` URIs in code/config; nothing
  sensitive in repos. Resolved at runtime by the vault port.
- **Env vars are the interface** — `arca run -- cmd` injects
  resolved secrets into the child env; `arca env` prints exports
  for `eval`. No SDK in the agent's runtime. `Secret` newtype
  (no Display/Debug of plaintext, explicit `.expose()`) governs
  arca's own memory; a Rust SDK only if ever needed.
- **Environments are first-class** — `dev`/`staging`/`prod`
  variants of the same ref; env is also a *routing axis* —
  `dev` → local vault, `prod` → cloud vault, per config
  association. Explicit pin: `arca://prod/name` override.
- **Crypto: age** — recipients/identities (rage); ciphertext at
  rest even in our own vault.
- **v1 = local-first** — `arca-vault-age`: age-encrypted vault
  file under `~/.local/share/arca` (or `~/.config/arca`).
  Dogfoods immediately, no cloud dependency.
- **Later vaults** — `arca-vault-cf`: self-hosted on Cloudflare
  (Worker + D1 recommended: strong consistency + audit rows in
  the same store; KV eventual consistency fights rotation).
  Client-side age encryption — CF stores opaque ciphertext only;
  port to AWS/etc. trivially. `arca-vault-aws` after.
- **Deploy port, later** — `arca-deploy` + `arca-deploy-cf` for
  write-only targets (CF Secrets Store, Workers secrets): CF's
  store is write-only — values can be pushed, never fetched, so
  it's a sink, not a source.
- **Sync/async: both** — sync traits + blocking HTTP for the CLI
  (myco-style light deps); async only behind a feature if an
  embeddable SDK ever lands.
- **Audit ledger** — every materialization records actor
  (`human:luis`, `devin`, `claude`), ref, env, timestamp.
- **Secrets never via CLI args** — stdin/prompt/keychain only
  (args land in shell history + `/proc`).

## Crate map (target)

| Crate | Role |
| --- | --- |
| `arca-core` | `arca://` refs, `Secret` newtype, env axis, actor attribution — pure, no I/O |
| `arca-config` / `-toml` | config port + impl |
| `arca-vault` | readable-store port: get/put/list/rotate, env variants |
| `arca-vault-age` | local age file — v1 impl |
| `arca-shell` | shell-out + env injection (`arca run`) |
| `arca-ops` | verb plumbing, ref resolution, audit writes |
| `arca-testkit` | vault conformance — every impl passes the same tests |
| `arca-tui` | ratatui vault browser, feature-gated |
| `arca-cli` | `arca` binary — clap, `--json`, manifest/schema |

Vestigial scaffold crates (renamed PM stack): `arca-git`,
`arca-index(-redb)`, `arca-store(-md)` — kept or re-mapped here.

## Surfaces

- **skill** — teaches agents the verbs (written once verbs exist)
- **CLI** — `--json` everywhere, `manifest`/`schema` self-discovery
- **TUI** — ratatui vault browser for humans (`arca ui`)

## Phases

1. ~~scaffold: copy myco anatomy → rename → compile~~ (done)
2. domain: `arca-core` (refs, Secret, env) + `arca-vault` port +
   `arca-vault-age` + testkit conformance; re-map vestigial crates
3. verbs: `put/get/list/run/env`, audit ledger, `arca.toml`
   env→vault association, skill
4. cloud: `arca-vault-cf` (Worker + D1), then `arca-deploy-cf`,
   `arca-vault-aws`
5. TUI polish, docs site, release via dist

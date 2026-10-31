# arca

*Secrets broker for agents — sealed, not spilled.*

Named for *Arca noae* — the ark clam — and Latin *arca*, the
strongbox. Agents need API keys, but every plaintext handoff is a
leak surface: prompts, tool output, logs, `.env` pasted into
configs. arca removes the handoff — code carries `arca://`
references, never values; the CLI materializes secrets as env vars
at run time; the audit ledger records every materialization.

```
agent ──► arca run -- cmd ──► resolve refs ──► inject env ──► child process
                │                                  (agent never sees the key)
                └──► --json everywhere + manifest/schema self-discovery
```

## Features

- **references, not values** — `arca://name` URIs in code/config;
  nothing sensitive lives in the repo at all
- **env vars are the interface** — `arca run` injects resolved
  secrets into the child environment; `arca env` prints exports for
  `eval`. No SDK needed in the agent's runtime
- **environments are first-class** — `dev`/`staging`/`prod`
  variants behind the same ref; env is also a routing axis:
  `dev` → local vault, `prod` → cloud vault — same ref, different
  backend
- **ports-and-adapters vaults** — the `arca-vault` trait with
  `arca-vault-age` (local age-encrypted file) first; self-hosted
  Cloudflare (Worker + D1, client-side encrypted) and AWS SM
  later; `arca-deploy` covers write-only push targets
- **redaction by construction** — a `Secret` newtype with no
  Display/Debug of plaintext; explicit `.expose()` is the only
  way out
- **audit ledger** — every materialization records actor
  (`human:luis`, `devin`, `claude`), ref, env, timestamp
- **surfaces for every actor** — skill teaches agents the verbs,
  `--json`/`manifest`/`schema` CLI for machines, ratatui
  `arca ui` for humans

## Position in the stack

arca = **secret materialization**. Neighbors: **myco** (work
memory), **sturn** (ephemeral coordination), **kuda** (code
memory), **peregrin** (orchestration — campaigns consume via
`arca run`), **bupha** (CI gate), **mimus** (critic — its key
resolution migrates to arca), **sepia** (session lifecycle).

## Status

Scaffold — the workspace anatomy is copied from
[myco](https://github.com/9aia/myco) (ports/adapters crates,
xtask, `docs/` + `project/` worktrees, hooks). Domain internals
are being built per `project/proposals/`.

## Install

```sh
cargo install --git https://github.com/9aia/arca arca
cargo xtask install        # from a checkout → ~/.cargo/bin
```

## Docs

Docs site lands on the `docs` branch (VitePress + Diátaxis →
GitHub Pages). Repo docs:
[AGENTS](AGENTS.md) · [CONTRIBUTING](CONTRIBUTING.md) ·
[SECURITY](SECURITY.md) · [LICENSE](LICENSE) (MIT) ·
[project](project/index.md) — TODO, backlog, ADRs, proposals

# arca

*Secrets broker for agents — sealed, not spilled.*

Named for *Arca noae* — the ark clam — and Latin *arca*, the
strongbox. Agents need API keys, but every plaintext handoff is a
leak surface: prompts, tool output, logs, `.env` pasted into
configs. arca removes the handoff — code carries `arca://`
references, never values; the CLI materializes secrets as env vars
at run time; every materialization emits an audit event on stderr.

```
agent ──► arca run -- cmd ──► resolve refs ──► inject env ──► child process
                │                                  (agent never sees the key)
                └──► --json everywhere + manifest/schema self-discovery
```

## Quickstart

```sh
cargo install --git https://github.com/9aia/arca arca

arca init                                   # age identity, 0600, once
printf '%s' "$STRIPE_KEY" | arca put arca://billing/stripe

# the agent path — the value exists only inside the child
# (one string arg → bash -c, so $STRIPE_KEY expands there)
arca run -- 'curl -H "Authorization: Bearer $STRIPE_KEY" \
    https://api.stripe.com/v1/charges'

# the human path
eval "$(arca env -s STRIPE_KEY=arca://billing/stripe)"
```

Values never enter via CLI args — `put`/`rotate` read stdin or a
hidden prompt. `arca get` is the escape hatch; prefer `run` so
values never enter agent context.

## Verbs

| Verb | What it does |
| --- | --- |
| `arca init` | Generate the age identity (0600) if missing; verify the vault opens |
| `arca put <ref>` | Store a secret — value via stdin or hidden `/dev/tty` prompt, never args |
| `arca get <ref>` | Print a secret's value (the escape hatch) |
| `arca list` | Refs stored in the selected env — names only, never values |
| `arca rm <ref>` | Remove a secret |
| `arca rotate <ref>` | New value via stdin/prompt; version bump + `rotated_at` |
| `arca run [-s VAR=ref] -- cmd` | Inject `[secrets]` + ad-hoc pairs into a child. One arg → `bash -c`; several → argv exec, no shell |
| `arca env [-s VAR=ref]` | `export VAR='…'` lines for `eval` — the human path |
| `arca check` | Validate config, vault readability, declared-surface resolution; exit 1 on findings |
| `arca ui` | Interactive vault browser (TUI) |
| `arca manifest` / `arca schema` | Machine-readable capabilities / `arca.toml` JSON Schema |

`--json` works on every verb (stable shapes in `arca manifest`);
`--env`, `--config`, `--actor` are global. Audit = ndjson events
on stderr: `{ts, actor, op, rref?, env, outcome}` — one per
materialization, `denied` on guardrail refusal, `error` on
failure.

## Config — `arca.toml`

Searched at `--config` → `$ARCA_CONFIG` → `./arca.toml` →
`~/.config/arca/arca.toml`. `arca schema` emits the spec;
[examples/arca.toml.example](examples/arca.toml.example) is the
commented tour.

```toml
[vaults.local]                # the built-in age vault ("local" is implicit)
kind = "age"
# path, identity, recipients  # optional overrides

[envs]                        # env → vault routing; unlisted envs → "local"
prod = { vault = "local" }

[secrets]                     # the declared surface run/env may inject
STRIPE_KEY = "arca://billing/stripe"

[guardrails]                  # advisory op table — accident prevention,
deny = ["get"]                # not policy (ARCA_ACTOR is self-claimed)
```

Refs: `arca://ns/name` names the secret; `--env`/`ARCA_ENV` picks
the resolution context (`dev` default); `arca://prod/ns/name`
pins the env explicitly. Envs are also the routing axis — same
ref, different backend per env.

## Features

- **references, not values** — `arca://name` URIs in code/config;
  nothing sensitive lives in the repo at all
- **env vars are the interface** — `arca run` injects resolved
  secrets into the child environment; `arca env` prints exports for
  `eval`. No SDK needed in the agent's runtime
- **environments are first-class** — `dev`/`staging`/`prod`
  variants behind the same ref; env is also a routing axis:
  `dev` → local vault, `prod` → cloud vault
- **ports-and-adapters vaults** — the `arca-vault` trait with
  `arca-vault-age` (local age-encrypted file) and `arca-vault-cf`
  (self-hosted Worker + D1, client-side age ciphertext) today;
  `arca-deploy` covers write-only push targets (`arca-deploy-cf`
  → CF Secrets Store)
- **redaction by construction** — a `Secret` newtype with no
  Display/Debug of plaintext; explicit `.expose()` is the only
  way out
- **audit wire** — every materialization, denial, and failure is
  an ndjson event on stderr: actor, ref, env, outcome, timestamp
- **surfaces for every actor** — `.agents/skills/` teaches agents
  the verbs, `--json`/`manifest`/`schema` for machines, ratatui
  `arca ui` for humans

## Position in the stack

arca = **secret materialization**. Neighbors: **myco** (work
memory), **sturn** (ephemeral coordination), **kuda** (code
memory), **peregrin** (orchestration — campaigns consume via
`arca run`; audit aggregation + policy are its), **bupha** (CI
gate), **mimus** (critic — its key resolution migrates to arca),
**sepia** (session lifecycle).

## Status

Working v1 — local age vault + remote `cf` vault (self-hosted
Worker + D1), deploy sinks, the full verb surface, TUI browser,
audit wire, agent skill, docs site. `arca serve` (ndjson IPC for
peregrin) and a KMS-backed crypto port are later ADRs per
`project/plans/` and `project/proposals/`.

## Install

```sh
cargo install --git https://github.com/9aia/arca arca
cargo xtask install        # from a checkout → ~/.cargo/bin
```

## Docs

The docs site lives on the `docs` branch as a worktree (VitePress
+ Diátaxis → `9aia.com/arca`); `cargo xtask site` builds it.
Repo docs:
[AGENTS](AGENTS.md) · [CONTRIBUTING](CONTRIBUTING.md) ·
[SECURITY](SECURITY.md) · [LICENSE](LICENSE) (MIT) ·
[project](project/index.md) — TODO, backlog, ADRs, proposals

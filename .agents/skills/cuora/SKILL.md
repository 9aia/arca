---
name: cuora
description: Materialize secrets via the cuora CLI — cuora:// refs, env-var injection with `run`, stdin-only writes, --json surfaces, advisory guardrails. Trigger when a task needs API keys, tokens, or credentials in code, config, or child processes.
metadata:
  version: "1.0.0"
---

# cuora — secrets broker for agents

`cuora` removes the plaintext handoff. Code and config carry
`cuora://` references; the CLI resolves them and injects values as
env vars at run time. **You never see the value — that's the
point.**

## The one rule

**Never read a secret into context.** Don't `cuora get`, don't
`cuora env`, don't cat vault files, don't ask the human to paste
the value. Consume secrets by running the thing that needs them:

```bash
cuora run -- 'curl -H "Authorization: Bearer $STRIPE_KEY" https://api.stripe.com/v1/charges'
```

`run` injects declared secrets into the child's environment and
exits with the child's code. The value lives only in that
process — not in your output, not in logs, not in history.

## Bootstrap

```bash
command -v cuora   # missing? install from the repo:
cargo install --git https://github.com/9aia/cuora cuora
cuora init          # generates ~/.config/cuora/identity.txt (0600)
```

## Discovery — prefer asking the CLI over reading docs

```bash
cuora manifest        # verbs, json shapes, env vars, audit contract (JSON)
cuora schema          # JSON Schema for cuora.toml
cuora <verb> --help   # every verb, self-documenting
```

`--json` is honored on every verb; errors come back as
`{"error": "…"}` on stderr with a non-zero exit.

## The ref model

- `cuora://ns/name` names a logical secret — env is **context**, not
  part of the name: `--env prod` / `CUORA_ENV` / `[envs]` config
  pick `dev`/`staging`/`prod` (default `dev`).
- `cuora://prod/ns/name` **pins** the env regardless of context.
- `[secrets] VAR = "cuora://ref"` in cuora.toml declares what `run`
  may inject — least privilege, `cuora check`-able.

## Verb cheat sheet — by intent

| Intent | Command |
| --- | --- |
| Store a secret | `printf '%s' "$val" \| cuora put cuora://ns/name` |
| Read a value (escape hatch) | `cuora get cuora://ns/name` — **avoid**; see the one rule |
| List refs (names only) | `cuora list [--env prod] [--json]` |
| Remove / rotate | `cuora rm <ref>` · `printf … \| cuora rotate <ref>` |
| Run with secrets (the way to consume) | `cuora run -- prog arg…` (argv exec) · `cuora run -- 'shell line'` (bash -c) |
| Ad-hoc injection | `cuora run -s VAR=cuora://ref -- cmd` (repeatable) |
| Exports for a human shell | `cuora env [-s VAR=ref]` → `export VAR='…'` lines for `eval` |
| Push to a write-only sink | `cuora deploy <ref> [--target NAME]` |
| Validate config + surface | `cuora check [--json]` — exit 1 on findings |
| Bootstrap | `cuora init` |

Two `run` exec modes: **one arg** after `--` → `bash -c` (pipes,
quoting); **several args** → `argv[0]` exec'd directly, no shell —
prefer argv when the command has arguments.

## Writing secrets — stdin only

Values **never** go on the command line (args land in shell
history and `/proc`). Pipe them:

```bash
printf '%s' "$STRIPE_KEY" | cuora put cuora://billing/stripe
pass show stripe/test | cuora put cuora://billing/stripe --env dev
```

`put`/`rotate` strip one trailing newline; the rest is verbatim.
Interactive humans get a hidden prompt instead.

## Config shape (`cuora.toml`, searched ./ → ~/.config/cuora/)

```toml
[vaults.local]            # the built-in age vault; entry only overrides
kind = "age"

[vaults.cloud]            # a remote vault — self-hosted CF Worker + D1
kind = "cf"               # holding per-secret age ciphertext
url = "https://…workers.dev"
token_env = "CUORA_CF_TOKEN"   # env var NAME — the token is never in toml

[envs]                    # env → vault routing; unlisted envs → "local"
prod = { vault = "cloud" }

[secrets]                 # the declared surface run/env may inject
STRIPE_KEY = "cuora://billing/stripe"

[deploy.prod]             # write-only sinks `cuora deploy` pushes to
kind = "cf-secrets-store" #   (value goes in, never comes back)
account_id = "…"
store_id = "…"

[guardrails]              # advisory: accident prevention, NOT policy
deny = ["get"]            # deny wins; default = "allow" for unlisted
```

If a guardrail denies your op, that's the owner saying "agents
don't do this" — don't work around it; use the allowed verb or
surface the denial to the human.

## Cloud vaults (`kind = "cf"`)

A `cf` vault talks HTTPS to a self-hosted Cloudflare Worker + D1
(deployed from `crates/cuora-vault-cf/worker/`). Same verbs, same
identity — values are age-encrypted client-side and the store holds
ciphertext only. Two requirements beyond `local`:

- the worker must be **deployed** — an unreachable `url` makes
  every verb fail with an I/O error (`cuora check` reports the vault
  `ok: false`, never a panic);
- the bearer token must be in the env var `token_env` names
  (`CUORA_CF_TOKEN` by default) — ask the human for it, don't
  invent one.

`cuora deploy cuora://ref [--target NAME]` pushes a *stored* value to
a write-only sink (`[deploy.NAME]`, e.g. a CF Secrets Store). The
value crosses to the sink in plaintext and can never be fetched
back — the vault stays the source of truth. It's a push, not a
sync: use it when a runtime must consume the secret directly from
Cloudflare's own store.

## Audit

Every materialization and every denial/failure emits one ndjson
event on **stderr**: `{ts, actor, op, rref?, env,
outcome: ok|denied|error}`. `CUORA_ACTOR`/`--actor` sets
attribution. Don't parse values out of stderr — there are none;
the wire carries metadata only.

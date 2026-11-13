# cuora

*Secrets broker for agents — sealed, not spilled.*

Named for the box turtle *Cuora* — it seals itself with a hinged
shell, and so does this. Agents need API keys, but every plaintext handoff is a
leak surface: prompts, tool output, logs, `.env` pasted into
configs. cuora removes the handoff — code carries `cuora://`
references, never values; the CLI materializes secrets as env vars
at run time; every materialization emits an audit event — ndjson
on stderr, plus OTLP spans when `OTEL_EXPORTER_OTLP_ENDPOINT` is
set.

```
agent ──► cuora run -- cmd ──► resolve refs ──► inject env ──► child process
                │                                  (agent never sees the key)
                └──► --json everywhere + manifest/schema self-discovery
```

## Quickstart

```sh
# the `cuora` command is a tamias pack (libraries/cuora) — a
generated bins[] shim → tamias-driver-cuora → the engine lib.
# From a cuora checkout:
cargo xtask pack                              # stage the driver payload
tamias install --allow-drivers ./libraries/cuora   # deploys `cuora`
# (release path: `tamias bundle pack libraries/cuora` → pinned tgz ref)

cuora init                                   # age identity (keyring, fail closed), once
printf '%s' "$STRIPE_KEY" | cuora put cuora://billing/stripe

# the agent path — the value exists only inside the child
# (one string arg → bash -c, so $STRIPE_KEY expands there)
cuora run -- 'curl -H "Authorization: Bearer $STRIPE_KEY" \
    https://api.stripe.com/v1/charges'

# the human path
eval "$(cuora env -s STRIPE_KEY=cuora://billing/stripe)"
```

Values never enter via CLI args — `put`/`rotate` read stdin or a
hidden prompt. `cuora get` is the escape hatch; prefer `run` so
values never enter agent context.

## Verbs

| Verb | What it does |
| --- | --- |
| `cuora init` | Generate the age identity (0600) if missing; verify the vault opens |
| `cuora put <ref>` | Store a secret — value via stdin or hidden `/dev/tty` prompt, never args |
| `cuora get <ref>` | Print a secret's value (the escape hatch) |
| `cuora list` | Refs stored in the selected env — names only, never values |
| `cuora rm <ref>` | Remove a secret |
| `cuora rotate <ref>` | New value via stdin/prompt; version bump + `rotated_at` |
| `cuora run [-s VAR=ref] -- cmd` | Inject `[secrets]` + ad-hoc pairs into a child. One arg → `bash -c`; several → argv exec, no shell |
| `cuora env [-s VAR=ref]` | `export VAR='…'` lines for `eval` — the human path |
| `cuora check` | Validate config, vault readability, declared-surface resolution; exit 1 on findings |
| `cuora serve` | ndjson IPC mode for orchestrators — the Vault port over stdin/stdout, optional `[serve.pdp]` policy consult |
| `cuora deploy <ref>` | Push a secret to a write-only sink (e.g. CF Secrets Store) |
| `cuora migrate` | Vault migrations — e.g. `--private-refs` re-keys cf rows to HMAC storage keys |
| `cuora manifest` / `cuora schema` | Machine-readable capabilities / `cuora.toml` JSON Schema |

`--json` works on every verb (stable shapes in `cuora manifest`);
`--env`, `--config`, `--actor` are global. Audit = ndjson events
on stderr: `{ts, actor, op, rref?, env, outcome}` — one per
materialization, `denied` on guardrail refusal, `error` on
failure.

Interactive browsing lives in pavo, not a cuora TUI:
`tamias-driver-cuora` (built from `crates/cuora-driver`) answers
pavo's `manifest`/`views`/`view`/`query` wire with vault
*metadata* views — vaults, envs, refs, per-ref detail, `check` —
and never renders secret values (see the crate README).

## Config — `cuora.toml`

Searched at `--config` → `$CUORA_CONFIG` → `./cuora.toml` →
`~/.config/cuora/cuora.toml`. `cuora schema` emits the spec;
[examples/cuora.toml.example](examples/cuora.toml.example) is the
commented tour.

```toml
[vaults.local]                # the built-in age vault ("local" is implicit)
kind = "age"
# path, identity, recipients  # optional overrides

[vaults.cloud]                # a remote vault (self-hosted Worker + D1)
kind = "cf"
url = "https://cuora-vault.you.workers.dev"
cache = ["dev"]               # optional read-through local cache at
                              # ~/.cache/cuora/ — dev cached, prod live

[envs]                        # env → vault routing; unlisted envs → "local"
prod = { vault = "cloud" }

[secrets]                     # the declared surface run/env may inject
STRIPE_KEY = "cuora://billing/stripe"

[guardrails]                  # advisory op table — accident prevention,
deny = ["get"]                # not policy (CUORA_ACTOR is self-claimed)

[serve.pdp]                   # `cuora serve`'s external policy decision
exec = "peregrin policy"      # point: request on stdin, verdict on
allow_on_error = false        # stdout; unreachable fails closed
```

Refs: `cuora://ns/name` names the secret; `--env`/`CUORA_ENV` picks
the resolution context (`dev` default); `cuora://prod/ns/name`
pins the env explicitly. Envs are also the routing axis — same
ref, different backend per env.

A two-segment ref can also name a **sealed pack** secret —
`cuora://<pack>/<secret>` resolves against tamias-installed
age-ciphertext packs on a vault miss (vault-first; packs are the
migration target, never an override). `cuora://<pack>@<version>/…`
pins a pack version when several are installed — the tamias
`name@version` spelling; unpinned ambiguity is an error, not a
silent pick. `tamias-driver-cuora` owns the author/consumer verbs
(`seal`/`open`/`materialize`/`recipients`/`packs`);
[docs/how-to/sealed-packs.md](docs/how-to/sealed-packs.md) is the
walkthrough (on the `docs` worktree → `9aia.com/cuora`).

## Features

- **references, not values** — `cuora://name` URIs in code/config;
  nothing sensitive lives in the repo at all
- **env vars are the interface** — `cuora run` injects resolved
  secrets into the child environment; `cuora env` prints exports for
  `eval`. No SDK needed in the agent's runtime
- **environments are first-class** — `dev`/`staging`/`prod`
  variants behind the same ref; env is also a routing axis:
  `dev` → local vault, `prod` → cloud vault
- **ports-and-adapters vaults** — the `cuora-vault` trait with
  `cuora-vault-age` (local age-encrypted file), `cuora-vault-cf`
  (self-hosted Worker + D1, client-side age ciphertext), and
  `cuora-vault-cached` (read-through local cache in front of any
  backend — `cache = ["dev"]` in `[vaults.*]`) today;
  `cuora-deploy` covers write-only push targets (`cuora-deploy-cf`
  → CF Secrets Store)
- **redaction by construction** — a `Secret` newtype with no
  Display/Debug of plaintext; explicit `.expose()` is the only
  way out
- **audit wire** — every materialization, denial, and failure is
  an ndjson event on stderr (silenceable via `CUORA_AUDIT_STDERR=0`)
  and an `cuora.<op>` OTLP span when `OTEL_EXPORTER_OTLP_ENDPOINT`
  is set: actor, ref, env, outcome, timestamp
- **surfaces for every actor** — `.agents/skills/` teaches agents
  the verbs, `--json`/`manifest`/`schema` for machines, pavo's
  `tamias-driver-cuora` views for humans

## Position in the stack

cuora = **secret materialization**. Neighbors: **myco** (work
memory), **sturn** (ephemeral coordination), **kuda** (code
memory), **peregrin** (orchestration — campaigns consume via
`cuora run`; audit aggregation + policy are its), **bupha**/**ci-gate**
(CI gate — bupha is the CI repo; the org gate is mid-migration back
from the peregrin `ci-gate` campaign), **mimus**
(critic — its key resolution migrates to cuora), **sepia** (session
lifecycle).

## Campaigns

`campaigns/` holds peregrin campaign definitions that orchestrate
work on this repo. `campaigns/adversarial/` is the self-testing
loop — attack → triage → fix → verify: an adversarial battery
(`cuora serve` PDP-wire repros, wire bounds, env scrubs, audit
redaction, plus `cargo xtask test`), every finding registered as a
myco issue + todo in the `project/` ledger, a fix agent per issue,
and a lint + battery re-run that closes them. The campaign's own
secrets come through `cuora run` — dogfooding the injection path:

```sh
cuora run --env dev -- peregrin campaign run campaigns/adversarial/main.yaml
```

See [`campaigns/adversarial/README.md`](campaigns/adversarial/README.md)
for the battery table, the `[serve.pdp]` policy-decision wiring, and
peregrin-version caveats (campaigns need the peregrin v2 engine).

## Status

Working v1 — local age vault + remote `cf` vault (self-hosted
Worker + D1), deploy sinks, the full verb surface, TUI browser,
audit wire, agent skill, docs site. `cuora serve` — the ndjson IPC
surface + `[serve.pdp]` policy-decision wire for peregrin — is in
(ADR-0003); a KMS-backed crypto port is a later ADR per
`project/plans/` and `project/proposals/`.

## Install

The `cuora` command is a tamias pack (`libraries/cuora`) — there is
no cargo-installed `cuora` binary. With `tamias` installed:

```sh
tamias install 9aia:cuora@0.2.0    # org registry (needs TAMIAS_REGISTRY_TOKEN)
```

From a checkout:

```sh
cargo xtask pack                            # stage the driver payload
tamias install --allow-drivers ./libraries/cuora   # deploys `cuora`
cargo xtask install        # also installs tamias-driver-cuora → ~/.cargo/bin
```

## Docs

The docs site lives on the `docs` branch as a worktree (VitePress
+ Diátaxis → `9aia.com/cuora`); `cargo xtask site` builds it.
Repo docs:
[AGENTS](AGENTS.md) · [CONTRIBUTING](CONTRIBUTING.md) ·
[SECURITY](SECURITY.md) · [LICENSE](LICENSE) (MIT) ·
[project](project/index.md) — TODO, backlog, ADRs, proposals

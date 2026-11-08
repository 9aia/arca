---
url: /cuora/reference/cli.md
---
# Command-Line Help for `cuora`

This document contains the help content for the `cuora` command-line program.

**Command Overview:**

* [`cuora`↴](#cuora)
* [`cuora init`↴](#cuora-init)
* [`cuora put`↴](#cuora-put)
* [`cuora get`↴](#cuora-get)
* [`cuora list`↴](#cuora-list)
* [`cuora rm`↴](#cuora-rm)
* [`cuora rotate`↴](#cuora-rotate)
* [`cuora run`↴](#cuora-run)
* [`cuora env`↴](#cuora-env)
* [`cuora check`↴](#cuora-check)
* [`cuora migrate`↴](#cuora-migrate)
* [`cuora deploy`↴](#cuora-deploy)
* [`cuora serve`↴](#cuora-serve)
* [`cuora ui`↴](#cuora-ui)
* [`cuora manifest`↴](#cuora-manifest)
* [`cuora schema`↴](#cuora-schema)

## `cuora`

cuora — secrets broker for agents: `cuora://` refs, redaction, env-injection instead of plaintext.

Refs name logical secrets (`cuora://billing/stripe`); `--env` picks the resolution context. `[secrets]` in cuora.toml bounds what `run`/`env` may inject. `manifest`/`schema` describe the surface for machines; every verb honors `--json`.

**Usage:** `cuora [OPTIONS] <COMMAND>`

###### **Subcommands:**

* `init` — Bootstrap: store the age identity in the system keyring when a session secret store is reachable — importing an existing identity.txt rather than orphaning it — else the `~/.config/cuora/identity.txt` keyfile (0600). Verifies the vault opens
* `put` — Store a secret. The value comes from stdin (`printf … | cuora put cuora://ns/name`) or a hidden terminal prompt — never args, which land in shell history and `/proc`
* `get` — Print a secret's value to stdout — the escape hatch. Prefer `cuora run` so values never enter agent context; guardrails can deny this op
* `list` — List the refs stored in the selected env — names only, never values
* `rm` — Remove a secret from the selected env
* `rotate` — Rotate a secret to a new value (stdin or hidden prompt, like `put`); bumps version + rotated_at
* `run` — Run a command with declared secrets injected as env vars
* `env` — Print `export VAR='…'` lines for `eval "$(cuora env)"` — the human path (`run` is the agent path). Values print to stdout: `--json` emits a `VAR → value` map
* `check` — Validate config + the declared surface: `[envs]` routes point at declared vaults, vaults decrypt, every `[secrets]` ref resolves in its routed env. Exit 1 on findings
* `migrate` — Rewrite stored records for a wire/config upgrade. Currently one transform: `--private-refs` re-keys every plaintext-named row in each env routed to a cf vault with `[vaults.*] private_refs = true` — ciphertext moves verbatim, old keys are deleted after the new rows land
* `deploy` — Push a stored secret to a write-only deploy target (`[deploy.NAME]` in cuora.toml, e.g. a CF Secrets Store). The vault is the source of truth; the sink never gives the value back. With one configured target, `--target` may be omitted
* `serve` — Serve the Vault port over ndjson — one JSON request per stdin line, one response per stdout line, audit events on stderr unchanged. For orchestrators embedding cuora (peregrin); with `[serve.pdp]` configured, a policy decision point is consulted before every vault-touching op. `{"op":"manifest"}` on the wire describes the protocol
* `ui` — Interactive vault browser (requires the `tui` feature)
* `manifest` — Capabilities, JSON shapes, env vars, audit contract — agent self-discovery (always JSON)
* `schema` — JSON Schema for `cuora.toml`

###### **Options:**

* `--json` — Emit JSON — stable machine shapes on every verb; errors are `{"error": "…"}` on stderr with a non-zero exit
* `--env <ENV>` — Env context — `dev`/`staging`/`prod`/custom. Overrides `CUORA_ENV`; default `dev`
* `--config <PATH>` — Config file, overriding the search ($CUORA_CONFIG → ./cuora.toml → ~/.config/cuora/cuora.toml)
* `--actor <ACTOR>` — Actor attribution for audit events (default $CUORA_ACTOR, else $USER)

## `cuora init`

Bootstrap: store the age identity in the system keyring when a session secret store is reachable — importing an existing identity.txt rather than orphaning it — else the `~/.config/cuora/identity.txt` keyfile (0600). Verifies the vault opens

**Usage:** `cuora init [OPTIONS]`

###### **Options:**

* `--with-file` — Force the identity keyfile even when a keyring is available — the headless/CI mode

## `cuora put`

Store a secret. The value comes from stdin (`printf … | cuora put cuora://ns/name`) or a hidden terminal prompt — never args, which land in shell history and `/proc`

**Usage:** `cuora put <RREF>`

###### **Arguments:**

* `<RREF>` — `cuora://` ref, e.g. cuora://billing/stripe

## `cuora get`

Print a secret's value to stdout — the escape hatch. Prefer `cuora run` so values never enter agent context; guardrails can deny this op

**Usage:** `cuora get <RREF>`

###### **Arguments:**

* `<RREF>` — `cuora://` ref

## `cuora list`

List the refs stored in the selected env — names only, never values

**Usage:** `cuora list`

## `cuora rm`

Remove a secret from the selected env

**Usage:** `cuora rm <RREF>`

###### **Arguments:**

* `<RREF>` — `cuora://` ref

## `cuora rotate`

Rotate a secret to a new value (stdin or hidden prompt, like `put`); bumps version + rotated_at

**Usage:** `cuora rotate <RREF>`

###### **Arguments:**

* `<RREF>` — `cuora://` ref

## `cuora run`

Run a command with declared secrets injected as env vars.

Injection surface = `[secrets]` in cuora.toml plus `-s VAR=cuora://ref` ad-hoc pairs; nothing else is injectable. Two exec modes after `--`:

`cuora run -- 'pnpm test | tee log'` — one string → `bash -c`

`cuora run -- pnpm test --filter x` — argv → direct exec

argv mode needs no shell quoting — prefer it when the command has arguments. Exit code is the child's.

**Usage:** `cuora run [OPTIONS] -- <CMD>...`

###### **Arguments:**

* `<CMD>` — Command to run (after `--`). One arg → `bash -c`; several → `argv[0]` exec'd directly with the rest

###### **Options:**

* `-s`, `--secret <VAR=CUORA_REF>` — Extra `VAR=cuora://ref` pairs to inject (refused when `[guardrails] deny_adhoc` is set)

## `cuora env`

Print `export VAR='…'` lines for `eval "$(cuora env)"` — the human path (`run` is the agent path). Values print to stdout: `--json` emits a `VAR → value` map

**Usage:** `cuora env [OPTIONS]`

###### **Options:**

* `-s`, `--secret <VAR=CUORA_REF>` — Extra `VAR=cuora://ref` pairs to resolve

## `cuora check`

Validate config + the declared surface: `[envs]` routes point at declared vaults, vaults decrypt, every `[secrets]` ref resolves in its routed env. Exit 1 on findings

**Usage:** `cuora check`

## `cuora migrate`

Rewrite stored records for a wire/config upgrade. Currently one transform: `--private-refs` re-keys every plaintext-named row in each env routed to a cf vault with `[vaults.*] private_refs = true` — ciphertext moves verbatim, old keys are deleted after the new rows land

**Usage:** `cuora migrate [OPTIONS]`

###### **Options:**

* `--private-refs` — Re-key cf-vault rows under HMAC-derived storage keys, attaching the encrypted `name` blob (P-004)

## `cuora deploy`

Push a stored secret to a write-only deploy target (`[deploy.NAME]` in cuora.toml, e.g. a CF Secrets Store). The vault is the source of truth; the sink never gives the value back. With one configured target, `--target` may be omitted

**Usage:** `cuora deploy [OPTIONS] <RREF>`

###### **Arguments:**

* `<RREF>` — `cuora://` ref to push

###### **Options:**

* `--target <NAME>` — Deploy target name (`[deploy.NAME]`); required when several are configured

## `cuora serve`

Serve the Vault port over ndjson — one JSON request per stdin line, one response per stdout line, audit events on stderr unchanged. For orchestrators embedding cuora (peregrin); with `[serve.pdp]` configured, a policy decision point is consulted before every vault-touching op. `{"op":"manifest"}` on the wire describes the protocol

**Usage:** `cuora serve`

## `cuora ui`

Interactive vault browser (requires the `tui` feature)

**Usage:** `cuora ui`

## `cuora manifest`

Capabilities, JSON shapes, env vars, audit contract — agent self-discovery (always JSON)

**Usage:** `cuora manifest`

## `cuora schema`

JSON Schema for `cuora.toml`

**Usage:** `cuora schema`

&#xA;This document was generated automatically by
clap-markdown.&#xA;

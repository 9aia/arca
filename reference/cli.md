# Command-Line Help for `arca`

This document contains the help content for the `arca` command-line program.

**Command Overview:**

* [`arca`↴](#arca)
* [`arca init`↴](#arca-init)
* [`arca put`↴](#arca-put)
* [`arca get`↴](#arca-get)
* [`arca list`↴](#arca-list)
* [`arca rm`↴](#arca-rm)
* [`arca rotate`↴](#arca-rotate)
* [`arca run`↴](#arca-run)
* [`arca env`↴](#arca-env)
* [`arca check`↴](#arca-check)
* [`arca migrate`↴](#arca-migrate)
* [`arca deploy`↴](#arca-deploy)
* [`arca serve`↴](#arca-serve)
* [`arca ui`↴](#arca-ui)
* [`arca manifest`↴](#arca-manifest)
* [`arca schema`↴](#arca-schema)

## `arca`

arca — secrets broker for agents: `arca://` refs, redaction, env-injection instead of plaintext.

Refs name logical secrets (`arca://billing/stripe`); `--env` picks the resolution context. `[secrets]` in arca.toml bounds what `run`/`env` may inject. `manifest`/`schema` describe the surface for machines; every verb honors `--json`.

**Usage:** `arca [OPTIONS] <COMMAND>`

###### **Subcommands:**

* `init` — Bootstrap: store the age identity in the system keyring when a session secret store is reachable — importing an existing identity.txt rather than orphaning it — else the `~/.config/arca/identity.txt` keyfile (0600). Verifies the vault opens
* `put` — Store a secret. The value comes from stdin (`printf … | arca put arca://ns/name`) or a hidden terminal prompt — never args, which land in shell history and `/proc`
* `get` — Print a secret's value to stdout — the escape hatch. Prefer `arca run` so values never enter agent context; guardrails can deny this op
* `list` — List the refs stored in the selected env — names only, never values
* `rm` — Remove a secret from the selected env
* `rotate` — Rotate a secret to a new value (stdin or hidden prompt, like `put`); bumps version + rotated_at
* `run` — Run a command with declared secrets injected as env vars
* `env` — Print `export VAR='…'` lines for `eval "$(arca env)"` — the human path (`run` is the agent path). Values print to stdout: `--json` emits a `VAR → value` map
* `check` — Validate config + the declared surface: `[envs]` routes point at declared vaults, vaults decrypt, every `[secrets]` ref resolves in its routed env. Exit 1 on findings
* `migrate` — Rewrite stored records for a wire/config upgrade. Currently one transform: `--private-refs` re-keys every plaintext-named row in each env routed to a cf vault with `[vaults.*] private_refs = true` — ciphertext moves verbatim, old keys are deleted after the new rows land
* `deploy` — Push a stored secret to a write-only deploy target (`[deploy.NAME]` in arca.toml, e.g. a CF Secrets Store). The vault is the source of truth; the sink never gives the value back. With one configured target, `--target` may be omitted
* `serve` — Serve the Vault port over ndjson — one JSON request per stdin line, one response per stdout line, audit events on stderr unchanged. For orchestrators embedding arca (peregrin); with `[serve.pdp]` configured, a policy decision point is consulted before every vault-touching op. `{"op":"manifest"}` on the wire describes the protocol
* `ui` — Interactive vault browser (requires the `tui` feature)
* `manifest` — Capabilities, JSON shapes, env vars, audit contract — agent self-discovery (always JSON)
* `schema` — JSON Schema for `arca.toml`

###### **Options:**

* `--json` — Emit JSON — stable machine shapes on every verb; errors are `{"error": "…"}` on stderr with a non-zero exit
* `--env <ENV>` — Env context — `dev`/`staging`/`prod`/custom. Overrides `ARCA_ENV`; default `dev`
* `--config <PATH>` — Config file, overriding the search ($ARCA_CONFIG → ./arca.toml → ~/.config/arca/arca.toml)
* `--actor <ACTOR>` — Actor attribution for audit events (default $ARCA_ACTOR, else $USER)



## `arca init`

Bootstrap: store the age identity in the system keyring when a session secret store is reachable — importing an existing identity.txt rather than orphaning it — else the `~/.config/arca/identity.txt` keyfile (0600). Verifies the vault opens

**Usage:** `arca init [OPTIONS]`

###### **Options:**

* `--with-file` — Force the identity keyfile even when a keyring is available — the headless/CI mode



## `arca put`

Store a secret. The value comes from stdin (`printf … | arca put arca://ns/name`) or a hidden terminal prompt — never args, which land in shell history and `/proc`

**Usage:** `arca put <RREF>`

###### **Arguments:**

* `<RREF>` — `arca://` ref, e.g. arca://billing/stripe



## `arca get`

Print a secret's value to stdout — the escape hatch. Prefer `arca run` so values never enter agent context; guardrails can deny this op

**Usage:** `arca get <RREF>`

###### **Arguments:**

* `<RREF>` — `arca://` ref



## `arca list`

List the refs stored in the selected env — names only, never values

**Usage:** `arca list`



## `arca rm`

Remove a secret from the selected env

**Usage:** `arca rm <RREF>`

###### **Arguments:**

* `<RREF>` — `arca://` ref



## `arca rotate`

Rotate a secret to a new value (stdin or hidden prompt, like `put`); bumps version + rotated_at

**Usage:** `arca rotate <RREF>`

###### **Arguments:**

* `<RREF>` — `arca://` ref



## `arca run`

Run a command with declared secrets injected as env vars.

Injection surface = `[secrets]` in arca.toml plus `-s VAR=arca://ref` ad-hoc pairs; nothing else is injectable. Two exec modes after `--`:

`arca run -- 'pnpm test | tee log'` — one string → `bash -c`

`arca run -- pnpm test --filter x` — argv → direct exec

argv mode needs no shell quoting — prefer it when the command has arguments. Exit code is the child's.

**Usage:** `arca run [OPTIONS] -- <CMD>...`

###### **Arguments:**

* `<CMD>` — Command to run (after `--`). One arg → `bash -c`; several → `argv[0]` exec'd directly with the rest

###### **Options:**

* `-s`, `--secret <VAR=ARCA_REF>` — Extra `VAR=arca://ref` pairs to inject (refused when `[guardrails] deny_adhoc` is set)



## `arca env`

Print `export VAR='…'` lines for `eval "$(arca env)"` — the human path (`run` is the agent path). Values print to stdout: `--json` emits a `VAR → value` map

**Usage:** `arca env [OPTIONS]`

###### **Options:**

* `-s`, `--secret <VAR=ARCA_REF>` — Extra `VAR=arca://ref` pairs to resolve



## `arca check`

Validate config + the declared surface: `[envs]` routes point at declared vaults, vaults decrypt, every `[secrets]` ref resolves in its routed env. Exit 1 on findings

**Usage:** `arca check`



## `arca migrate`

Rewrite stored records for a wire/config upgrade. Currently one transform: `--private-refs` re-keys every plaintext-named row in each env routed to a cf vault with `[vaults.*] private_refs = true` — ciphertext moves verbatim, old keys are deleted after the new rows land

**Usage:** `arca migrate [OPTIONS]`

###### **Options:**

* `--private-refs` — Re-key cf-vault rows under HMAC-derived storage keys, attaching the encrypted `name` blob (P-004)



## `arca deploy`

Push a stored secret to a write-only deploy target (`[deploy.NAME]` in arca.toml, e.g. a CF Secrets Store). The vault is the source of truth; the sink never gives the value back. With one configured target, `--target` may be omitted

**Usage:** `arca deploy [OPTIONS] <RREF>`

###### **Arguments:**

* `<RREF>` — `arca://` ref to push

###### **Options:**

* `--target <NAME>` — Deploy target name (`[deploy.NAME]`); required when several are configured



## `arca serve`

Serve the Vault port over ndjson — one JSON request per stdin line, one response per stdout line, audit events on stderr unchanged. For orchestrators embedding arca (peregrin); with `[serve.pdp]` configured, a policy decision point is consulted before every vault-touching op. `{"op":"manifest"}` on the wire describes the protocol

**Usage:** `arca serve`



## `arca ui`

Interactive vault browser (requires the `tui` feature)

**Usage:** `arca ui`



## `arca manifest`

Capabilities, JSON shapes, env vars, audit contract — agent self-discovery (always JSON)

**Usage:** `arca manifest`



## `arca schema`

JSON Schema for `arca.toml`

**Usage:** `arca schema`



<hr/>

<small><i>
    This document was generated automatically by
    <a href="https://crates.io/crates/clap-markdown"><code>clap-markdown</code></a>.
</i></small>


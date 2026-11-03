---
url: /arca/explanation/architecture.md
---
# Architecture

Ports-and-adapters over a flat `crates/` workspace. `arca-core`
is pure domain — `arca://` refs, the redacting `Secret` newtype,
the env axis, actor attribution — with no I/O. Ports are traits
(`arca-config`, `arca-vault`, `arca-deploy`); impls live in
separate crates; `crates/arca-cli` (package `arca`) is the
composition root that wires impls to verbs.

Design rules:

* **No `unsafe`, no async** — the binary stays dependency-light
  and synchronous; vault impls that speak HTTP use a blocking
  client (`ureq`), so no runtime embeds anywhere.
* **`Secret` never exposes plaintext** — no `Display`/`Debug` of
  the value; `.expose()` is the only way out, called at edges
  only (child env injection, `arca env` exports, `get`).
* **Stores hold ciphertext only** — every vault impl encrypts
  client-side with age (ADR-0001); a remote vault is ciphertext
  over the wire *and* at rest, never a plaintext service.
* **Ports are conformance-tested** — `arca-testkit` runs the same
  contract suite against every vault impl (`arca-vault-age`,
  `arca-vault-cf`): upsert/rotate/list/env-isolation semantics
  can't drift per backend.
* **One ops shell** — `arca-ops` owns config load, env→vault
  routing, the env-pin rule, and audit emission; the CLI and the
  TUI are both thin front-ends over it and cannot diverge.

| crate | layer | purpose |
|---|---|---|
| `arca-core` | domain | arca domain core: arca:// refs, redacting Secret, env axis, actor, config model — pure, no I/O |
| `arca-config` | port | Config port: where arca.toml-shaped config comes from |
| `arca-deploy` | port | Deploy port: write-only push targets (sinks) for secret values — never a source |
| `arca-vault` | port | Vault port: readable secret stores — get/put/rm/list/rotate, per-env; every impl stores opaque ciphertext |
| `arca-ops` | ops | arca ops I/O: config load, env/ref resolution, vault open, verb plumbing, audit emission |
| `arca-config-toml` | adapter | Config impl: arca.toml file (explicit path, env, ./, ~/.config) |
| `arca-deploy-cf` | adapter | Deploy impl: push secret values to a Cloudflare account Secrets Store (write-only sink) via the CF API v4 |
| `arca-identity` | adapter | Identity port: where the age identity (master key) comes from — file, keyring, later sealed variants |
| `arca-identity-file` | adapter | Identity impl: the ~/.config/arca/identity.txt age keyfile (0600) — the headless/CI default |
| `arca-identity-keyring` | adapter | Identity impl: the platform session secret store (secret-service / Keychain / Credential Manager) via keyring-core |
| `arca-serve` | adapter | arca serve — the ndjson request/response IPC surface orchestrators embed, with the external policy-decision consult (P-005) |
| `arca-shell` | adapter | Run a command in a working directory via bash -c — the generic shell-out helper |
| `arca-testkit` | adapter | Vault conformance suite — every arca-vault impl passes the same contract tests (fake values only) |
| `arca-tui` | adapter | arca — interactive vault TUI; a thin front-end over arca-ops (phase 5 polish) |
| `arca-vault-age` | adapter | Local vault impl: one age-x25519-encrypted JSON doc — the v1 default backend |
| `arca-vault-cached` | adapter | Composite vault impl: read-through age-encrypted local cache in front of any backend (P-007) |
| `arca-vault-cf` | adapter | Remote vault impl: self-hosted Cloudflare Worker + D1 holding per-secret age ciphertext — the Worker never sees plaintext |
| `arca` | binary | arca — secrets broker for agents: reference URIs, redaction, env-injection CLI |

## Front-ends

* **`arca` (CLI)** — clap verbs + `--json`/`manifest`/`schema` for
  machines; the agent interface.
* **`arca ui` (TUI)** — a read-only ratatui browser over the same
  ops; feature-gated (`--no-default-features` for a lean binary).
* **`arca serve`** — ndjson IPC over the Vault port with an
  exec-driven `[serve.pdp]` policy-decision hook (ADR-0003);
  peregrin's embedding point.

## Boundary — secret materialization only

arca owns refs, vaults, env injection, redaction, and the audit
wire. Neighbors own theirs — see
[boundaries](boundaries.md) for the full map.

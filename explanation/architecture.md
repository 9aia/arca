# Architecture

Ports-and-adapters over a flat `crates/` workspace. `cuora-core`
is pure domain — `cuora://` refs, the redacting `Secret` newtype,
the env axis, actor attribution — with no I/O. Ports are traits
(`cuora-config`, `cuora-vault`, `cuora-deploy`); impls live in
separate crates; `crates/cuora-cli` (package `cuora`) is the
composition root that wires impls to verbs.

Design rules:

- **No `unsafe`, no async** — the binary stays dependency-light
  and synchronous; vault impls that speak HTTP use a blocking
  client (`ureq`), so no runtime embeds anywhere.
- **`Secret` never exposes plaintext** — no `Display`/`Debug` of
  the value; `.expose()` is the only way out, called at edges
  only (child env injection, `cuora env` exports, `get`).
- **Stores hold ciphertext only** — every vault impl encrypts
  client-side with age (ADR-0001); a remote vault is ciphertext
  over the wire *and* at rest, never a plaintext service.
- **Ports are conformance-tested** — `cuora-testkit` runs the same
  contract suite against every vault impl (`cuora-vault-age`,
  `cuora-vault-cf`): upsert/rotate/list/env-isolation semantics
  can't drift per backend.
- **One ops shell** — `cuora-ops` owns config load, env→vault
  routing, the env-pin rule, and audit emission; the CLI and the
  TUI are both thin front-ends over it and cannot diverge.

<!-- crate-map -->
| crate | layer | purpose |
|---|---|---|
| `cuora-core` | domain | cuora domain core: cuora:// refs, redacting Secret, env axis, actor, config model — pure, no I/O |
| `cuora-config` | port | Config port: where cuora.toml-shaped config comes from |
| `cuora-deploy` | port | Deploy port: write-only push targets (sinks) for secret values — never a source |
| `cuora-vault` | port | Vault port: readable secret stores — get/put/rm/list/rotate, per-env; every impl stores opaque ciphertext |
| `cuora-ops` | ops | cuora ops I/O: config load, env/ref resolution, vault open, verb plumbing, audit emission |
| `cuora-config-toml` | adapter | Config impl: cuora.toml file (explicit path, env, ./, ~/.config) |
| `cuora-deploy-cf` | adapter | Deploy impl: push secret values to a Cloudflare account Secrets Store (write-only sink) via the CF API v4 |
| `cuora-driver` | adapter | tamias-driver-cuora — pavo view driver over cuora vault metadata (read-only; never renders secret values) |
| `cuora-identity` | adapter | Identity port: where the age identity (master key) comes from — file, keyring, later sealed variants |
| `cuora-identity-file` | adapter | Identity impl: the ~/.config/cuora/identity.txt age keyfile (0600) — the headless/CI default |
| `cuora-identity-keyring` | adapter | Identity impl: the platform session secret store (secret-service / Keychain / Credential Manager) via keyring-core |
| `cuora-notice` | adapter | Process-wide UX notices — cuora:-prefixed stderr notes, mutable for terminal-owning surfaces |
| `cuora-serve` | adapter | cuora serve — the ndjson request/response IPC surface orchestrators embed, with the external policy-decision consult (P-005) |
| `cuora-shell` | adapter | Run a command in a working directory via bash -c — the generic shell-out helper |
| `cuora-testkit` | adapter | Vault conformance suite — every cuora-vault impl passes the same contract tests (fake values only) |
| `cuora-vault-age` | adapter | Local vault impl: one age-x25519-encrypted JSON doc — the v1 default backend |
| `cuora-vault-cached` | adapter | Composite vault impl: read-through age-encrypted local cache in front of any backend (P-007) |
| `cuora-vault-cf` | adapter | Remote vault impl: self-hosted Cloudflare Worker + D1 holding per-secret age ciphertext — the Worker never sees plaintext |
| `cuora` | binary | cuora — secrets broker for agents: reference URIs, redaction, env-injection CLI |

## Front-ends

- **`cuora` (CLI)** — clap verbs + `--json`/`manifest`/`schema` for
  machines; the agent interface.
- **`cuora ui` (TUI)** — a read-only ratatui browser over the same
  ops; feature-gated (`--no-default-features` for a lean binary).
- **`cuora serve`** — ndjson IPC over the Vault port with an
  exec-driven `[serve.pdp]` policy-decision hook (ADR-0003);
  peregrin's embedding point.

## Boundary — secret materialization only

cuora owns refs, vaults, env injection, redaction, and the audit
wire. Neighbors own theirs — see
[boundaries](boundaries.md) for the full map.

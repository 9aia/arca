+++
id = "ADR-0001"
title = "resolution model, wire surface, and crypto port boundaries"
status = "open"
created = "2026-10-31"
priority = "medium"
+++

Status: accepted (v1); revisitable per spike results. Amended
after owner review — audit ownership moved to peregrin, `serve`
confirmed as port + adapter.

## Context

P-001/PL-001 left several boundaries needing a decision: how env
participates in references, whether a `serve` wire exists,
whether crypto is a port or a fixed choice, who owns audit, and
how a project declares its secret surface.

## Decision

- **Env is resolution context, not ref name.** `arca://billing/stripe`
  names the logical secret; `dev`/`staging`/`prod` is selected by
  `arca run --env` / `ARCA_ENV` / `[envs]` config. Explicit pin
  `arca://prod/billing/stripe` overrides for cross-env access.
- **Env is also the routing axis.** `[envs] prod = {vault="cf"}`
  associates environments to backends — same ref, different store
  per env. Local (`arca-vault-age`) is the default vault forever.
- **Env vars are the consumption interface.** `arca run -- cmd`
  resolves declared refs and injects them into the child env —
  values live only in that process; `arca env` prints exports for
  humans (`eval`). No SDK. `run` is the agent path; `env` is the
  human path.
- **`[secrets]` declares a project's surface.** `VAR = "arca://ref"`
  entries in `arca.toml` do three jobs: map ref → env-var name,
  bound what `run` may inject (least privilege — no ambient vault
  dump), and make the surface reviewable/`arca check`-able. Ad-hoc
  `-s VAR=ref` covers one-offs without a file.
- **Audit belongs to peregrin.** No local `audit.jsonl`. arca
  emits materialization events on the `serve` wire (and peregrin
  records its own invocations when orchestrating); aggregation,
  retention, and compliance views are peregrin's — integrating
  the battle-tested ledger beats a weaker local one.
- **`serve` is a port + adapters.** `Serve` = verb dispatch over a
  wire; **ndjson JSON-RPC 2.0 on stdio is the first/default impl**
  (sepia/myco protocol family); unix socket / HTTP are later
  adapters. Still gated: per-request actor attribution + policy
  must exist first — whoever connects inherits vault access.
- **Crypto is fixed: age.** All vault impls store opaque age
  ciphertext; stores never see plaintext. Whether crypto graduates
  to a second port (`seal`/`open` with age|kms impls) is decided
  by the AWS spike, not on main.
- **Values never enter via CLI args.** stdin/prompt/keychain only.

## Consequences

Refs stay portable across environments; backend swap is a config
change. Audit has no local implementation to maintain — it
arrives free once `serve` lands. `serve`, KMS, and envelope refs
remain cheap to add later because vault, wire, and crypto
boundaries already isolate them. Actor+policy (in `arca-core`,
reviewed in P-001) is the hard dependency for `serve`.

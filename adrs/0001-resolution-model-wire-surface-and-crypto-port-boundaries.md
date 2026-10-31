+++
id = "ADR-0001"
title = "resolution model, wire surface, and crypto port boundaries"
status = "open"
created = "2026-10-31"
priority = "medium"
+++

Status: accepted (v1); revisitable per spike results.

## Context

P-001/PL-001 left three boundaries needing a decision: how env
participates in references, whether a `serve` wire exists, and
whether crypto is a port or a fixed choice.

## Decision

- **Env is resolution context, not ref name.** `arca://billing/stripe`
  names the logical secret; `dev`/`staging`/`prod` is selected by
  `arca run --env` / `ARCA_ENV` / `[envs]` config. Explicit pin
  `arca://prod/billing/stripe` overrides for cross-env access.
- **Env is also the routing axis.** `[envs] prod = {vault="cf"}`
  associates environments to backends — same ref, different store
  per env. Local (`arca-vault-age`) is the default vault forever.
- **No `arca serve` in v1.** ndjson/JSON-RPC on stdio (sepia/myco
  pattern) requires per-request actor attribution + the policy
  model; whoever connects inherits vault access. Revisit once
  policy lands and peregrin needs embedding.
- **Crypto is fixed: age.** All vault impls store opaque age
  ciphertext; stores never see plaintext. Whether crypto graduates
  to a second port (`seal`/`open` with age|kms impls) is decided
  by the AWS spike, not on main.
- **Env vars are the only consumption interface.** `arca run`
  injects; `arca env` prints exports. No SDK.
- **Values never enter via CLI args.** stdin/prompt/keychain only.

## Consequences

Refs stay portable across environments; backend swap is a config
change; serve and KMS remain cheap to add later because vault and
(future) crypto boundaries already isolate them.

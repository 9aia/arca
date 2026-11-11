+++
id = "P-003"
title = "scoped tokens and HMAC request signing for cf vault"
status = "done"
created = "2026-11-02"
priority = "med"
labels = ["security", "vault-cf"]
+++

Status: **proposal**. Upgrades the cf vault wire from one static
bearer token to scoped credentials, then a signed-request scheme
where the token never travels.

## Problem

`ARCA_VAULT_TOKEN` is a single static bearer for all envs and
ops. Whoever holds it gets read access to every ciphertext and
ref name, in every env, forever — no scoping, no rotation story,
and the secret travels on every request.

## Proposal

Phase 1 — scoped tokens (worker + config):

- Worker config maps token → `{env → {read, write}}` scopes;
  multiple tokens supported. A leaked `dev` token can't read
  `prod`.
- `arca.toml` `[vaults.cf]` gains per-env token refs
  (`arca://…` refs, naturally — not values).
- Token rotation doc + `PUT /v1/admin/token` left out — rotation
  is a `wrangler secret put` away; document it.

Phase 2 — signed requests (wire `arca-vault-cf/2`):

- `Authorization: ArcaHmac token_id, ts, sig` where
  `sig = HMAC-SHA256(token, method+path+ts+body-sha)`.
  Token never on the wire; replay window ~60s; worker rejects
  stale ts. Bearer mode stays accepted during migration.

## Non-goals

- mTLS / client certs — disproportionate for self-hosted D1.
- OAuth/dynamic auth — out of the single-binary scope.

## Effort

Phase 1 small (worker scope map + config arm). Phase 2 medium
(client signer, worker verifier, protocol version bump).

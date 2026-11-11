+++
id = "P-004"
title = "private ref metadata via HMAC-derived keys"
status = "done"
created = "2026-11-02"
priority = "med"
labels = ["security", "vault-cf"]
+++

Status: **proposal**. Option to stop leaking ref names and env
names to the cf store — ciphertext-only is the goal, metadata
included.

## Problem

Values are age ciphertext end-to-end, but `ref` and `env` are
plaintext D1 columns. A store/worker compromise reveals *which*
secrets exist — namespaces map to project structure
(`billing/stripe`, `ci/deploy-key`), which is real metadata.

## Proposal

Opt-in private-refs mode, `arca-vault-cf/2` wire:

- `[vaults.cf] private_refs = true` — client derives
  `ref_key = HMAC(identity-derived-key, env + "/" + ref)` and
  uses it as the storage key. Deterministic → lookup unchanged;
  unlinkable → the store sees opaque hex.
- Display names kept client-side: each record gains an
  encrypted `name` blob (age ciphertext) holding the real ref,
  decrypted only when listing.
- `list` returns ref_keys; client maps back via the name blob.
  Migration: `arca migrate --private-refs` rewrites existing
  records; mixed-mode vault rejected by `check` (fail closed).

## Trade-offs / non-goals

- Costs: opaque-at-server debugging gets harder (D1 console
  shows hex); rotation of the derived key = full rewrite.
- Not hiding *access patterns* (timing/frequency) — out of
  scope without an oblivious design.
- Default stays `private_refs = false` — plaintext names are
  acceptable for many users and keep ops simple.

## Effort

Medium — client derivation + name blob, worker stores opaque
keys verbatim (already does), protocol v2 flag, migrate verb.

+++
id = "P-007"
title = "cached remote vault — read-through encrypted local cache"
status = "open"
created = "2026-11-03"
priority = "medium"
labels = ["security", "ux", "vault-cf"]
+++

Status: **proposal**. Dev secrets live in the cloud vault; a local
age-encrypted cache makes materialization offline-capable and
onboarding a two-command affair.

## Problem

Keeping dev secrets in the shared cf vault is the right call for
teams (one place to onboard, one place to rotate), but a pure
remote vault means: every `arca run` does a network round-trip,
offline = broken, and per-secret latency sits on the hot path of
every command.

## Proposal

Read-through encrypted cache — the cache is itself an
`arca-vault` impl (composite), not a bolt-on:

- `[vaults.*] cache = true` (or new impl `arca-vault-cached`
  wrapping any backend) — `get`/`list` hit the local
  age-encrypted cache first; miss → fetch remote → write-through
  to cache. All cache rows carry the remote `version`/
  `rotated_at` the worker already serves.
- **Freshness:** cheap metadata probe (version/rotated_at GET)
  or TTL; `put`/`rotate`/`rm` always write remote-first then
  invalidate/update the cache entry — writes never serve stale.
- **Env-scoped policy:** dev may cache aggressively; prod
  defaults to live-fetch (`cache = "dev"` scope, or per-env
  flags) — a stale prod secret is a footgun we don't ship.
- **Onboarding payoff:** `arca init` + a scoped `dev` token ref
  → the whole dev surface materializes on first use. No local
  `put`, no `.env` shared over chat.
- Cache location: `~/.cache/arca/` (XDG cache dir —
  disposable-by-convention data, honest placement).

## Simpler alternative

`arca sync` — explicit pull of remote→local mirror. No
invalidation logic at all, but the read-through version is the
better UX; sync can ship later as the explicit refresh escape
hatch either way.

## Dependencies

- The cf vault must actually be deployed (database_id is a
  placeholder today; needs CF creds) — this proposal is shaped
  for after that lands.
- P-004 `private_refs` composes cleanly: cache stores ref_key +
  name blob verbatim, still opaque.

## Effort

Medium — composite impl or cf flag, metadata freshness probe,
write-through invalidation, env-scoped policy arm in config.

+++
id = "P-005"
title = "arca serve — ndjson policy-decision wire for peregrin"
status = "in-progress"
created = "2026-11-02"
priority = "low"
labels = ["architecture", "peregrin"]
+++

Status: **proposal (later ADR)**. Formalizes the seam for real
policy: peregrin becomes the decision point, arca stays the
enforcement-thin emitter.

## Problem

`ARCA_ACTOR` is self-claimed and `[guardrails]` are advisory —
accident prevention, not adversary defense (by design). Real
enforcement needs an external policy engine, and that engine
needs a wire into arca's materialization path.

## Proposal

`arca serve` — an ndjson/JSON-RPC IPC surface (stdin/stdout or
local socket):

- Each materialization request arrives as an event; before
  resolving, arca consults a configured policy endpoint (the
  PDP — peregrin) with `{actor_attestation, ref, env, op}`.
- PDP returns `allow | deny | allow-with-conditions` (e.g.
  redact, session-bound); arca enforces the verdict and emits
  the usual audit event plus `policy:` fields.
- Actor attestation: PDP-issued short-lived tokens (signed) —
  replaces self-claimed `ARCA_ACTOR` *on the serve path only*;
  standalone CLI keeps advisory mode explicitly.

## Non-goals

- arca never *becomes* the policy engine — PDP is always
  external (peregrin's job).
- No daemon by default — `serve` is opt-in; single-shot CLI
  stays the primary surface.

## Effort

Large — wire protocol, attestation format, peregrin-side PDP
impl. Correctly sequenced after peregrin exists; this proposal
only fixes the boundary contract so peregrin can build against it.

## Note — local serving needs no cf

`arca serve` exposes the `Vault` *port* itself over ndjson — any
impl behind it. A local instance is the age vault served in
process, no HTTP/D1 semantics involved; the cf wire is only for
remote vaults.

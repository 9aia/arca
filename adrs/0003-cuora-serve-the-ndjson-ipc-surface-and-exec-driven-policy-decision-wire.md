+++
id = "ADR-0003"
title = "cuora serve — the ndjson IPC surface and exec-driven policy-decision wire"
status = "open"
created = "2026-11-03"
priority = "med"
+++

# arca serve — the ndjson IPC surface and exec-driven policy-decision wire

Status: **accepted** · Created: 2026-11-03

Implements P-005.

## Context

`ARCA_ACTOR` is self-claimed and `[guardrails]` are advisory —
accident prevention by design, not enforcement. Real policy needs
an external decision point, and that point needs a wire into
arca's materialization path. Peregrin is that point: it
orchestrates campaigns and arbitrates between actors, so it needs
to embed arca in-process, gate each materialization, and see the
same audit trail. P-005 fixed the boundary contract; this ADR
fixes the wire shapes.

## Decision

- **`arca serve` — the Vault port over ndjson.** One JSON request
  per stdin line, one JSON response per stdout line; the audit
  ndjson on stderr is unchanged. Ops: `get`/`put`/`rm`/`rotate`
  (the vault-touching verbs, mirroring the `Vault` port), `list`,
  `check`, `manifest` (protocol self-description), `shutdown`.
  `run`/`env`/`deploy`/`init`/`ui`/`migrate`/`schema` stay
  single-shot CLI verbs — serve covers the surface an embedder
  needs in-process. A malformed line is a `bad_request` response,
  never a crash, never the end of the loop; EOF is a clean exit.
  `arca-serve` is a thin front over `arca-ops` like `arca-tui`:
  env pins, `[envs]` routing, and audit events are identical.
- **Request envelope.** `{id, op, ref?, env?, value?, actor?,
  attestation?}`. `id` echoes verbatim. `env` overrides the loaded
  context per request (a pin still wins). `value` carries
  plaintext for `put`/`rotate` — the wire is the channel, the
  parent owns it exactly like it owns `arca get`'s stdout.
  Responses: `{id, ok: true, result}` or `{id, ok: false,
  error: {code, message}}` — codes: `bad_request`, `unknown_op`,
  `denied`, `pdp_unreachable`, `not_found`, `op_error`.
- **Policy consult: `[serve.pdp] exec`.** Before every
  vault-touching op — *before the vault is opened* — serve runs
  the configured command via `sh -c`, sends
  `{actor, attestation?, op, env, ref}` as one JSON line on
  stdin, and reads `{allow, reason?, obligations?}` on stdout.
  The exec driver was chosen over sockets/HTTP: zero new
  dependencies, sync-only, no daemon lifecycle — a PDP wanting a
  persistent endpoint can put `socat`/`nc` in its exec line. A
  deny is a `denied` response + `denied` audit event and no vault
  access.
- **Fail closed.** Spawn failure, non-zero exit, or an
  unparseable verdict is *unreachable* → the op is denied
  (`pdp_unreachable`) unless `[serve.pdp] allow_on_error = true`,
  which runs the op and records the degraded verdict. An
  unanswered policy engine must not silently mean "allowed".
- **Verdicts ride the audit wire.** `AuditEvent` gains an
  optional `policy` object `{allow, reason?, obligations?}`,
  attached by `Ops::scoped`/`set_policy` on the serve path only;
  standalone CLI events stay unchanged. The same verdict echoes
  on the response's `policy` field.
- **Attestation is advisory.** `actor` is the self-claimed
  attribution id (validated); `attestation` is an opaque blob
  passed to the PDP verbatim — arca verifies neither. Signed
  short-lived tokens are peregrin's side, a later decision; this
  ADR only fixes the pass-through seam.
- **Per-request scoping.** `Ops::scoped(actor, env, policy)`
  clones the loaded handle per request — no config reload — so
  audit events carry the request's actor and the consult's
  verdict.
- **Guardrails still apply** on the serve path (advisory layer,
  checked before the PDP consult); `manifest`/`shutdown` are
  protocol meta-ops and skip it.

## Amendment — adversarial-hardening pass (T-009)

The original claim that `list`/`check` "skip the consult —
nothing materializes" was wrong: `list` decrypts every record's
metadata, and `check` probes vaults including scoped-token
`get`s (real materializations, plus outbound signed HTTP on cf
vaults). Corrected:

- **`list`/`check` consult the PDP too.** The consult carries
  `op: "list"|"check"` and omits `ref` (now optional in the
  request shape). The verdict attaches to the scoped `Ops`
  handle before the op runs, so the incidental materializations
  inside — scoped-token `get`s, vault probes — emit
  `policy`-bearing audit events.
- **The consult is bounded.** The exec child is killed at
  `[serve.pdp] timeout_ms` (new field, default 5000); the
  verdict read is capped at 64 KiB; the request is written on a
  helper thread so a >64 KiB request can't deadlock against a
  child that never reads. A hung or flooding PDP can no longer
  freeze the broker — the "deliberate v1 trade-off" below is
  superseded.
- **The PDP child's env is scrubbed** to the `arca-shell`
  allowlist — parent credentials (`ARCA_CF_TOKEN`,
  `CLOUDFLARE_API_TOKEN`, …) never cross into the decider.
- **Request bounds.** One line ≤ 4 MiB (`bad_request` +
  resync); `actor` ≤ 256 B, `attestation`/`value` ≤ 64 KiB,
  `ref` ≤ 1 KiB, `id` ≤ 4 KiB serialized.
- **Validation precedes the consult** — a doomed request never
  spawns a PDP.
- **`[serve.pdp] exec` is trust-root config.** `arca serve`
  warns on stderr when it loads one from an *implicit*
  `./arca.toml` — pin with `--config`/`$ARCA_CONFIG`.
- **`check` audits `error` when it finds problems** (mirrors
  the CLI exit code), on both the serve wire and the CLI.

## Consequences

- Peregrin can build a PDP today against the documented request/
  verdict shapes — peregrin-side implementation is its own repo's
  work.
- `serve` is opt-in: the single-shot CLI stays the primary
  surface and its posture is unchanged (no PDP consult, advisory
  guardrails only).
- ~~A hung PDP blocks the decision~~ — superseded by the
  amendment above: `timeout_ms` (default 5 s) bounds the exec
  spawn, and the bounded verdict read keeps a flooding PDP from
  sinking memory. A sandboxed decider can still put `timeout` in
  its own exec line for tighter control.
- Audit consumers see a new optional `policy` field — additive,
  backwards-compatible.

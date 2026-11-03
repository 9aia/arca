# The `arca serve` protocol

`arca serve` is the Vault port over newline-delimited JSON — the
surface orchestrators (peregrin) embed. One JSON request per
**stdin** line, one JSON response per **stdout** line, and the
[audit wire](audit-events.md) stays on **stderr**, unchanged.
Protocol version: `arca-serve/1` — send `{"op":"manifest"}` on
the wire for the machine-readable self-description.

Serve is a thin front over the same `arca-ops` plumbing the CLI
uses: `[envs]` routing, `arca://<env>/…` pins, and audit events
are identical. `run`/`env`/`deploy`/`init`/`ui`/`migrate`/`schema`
stay single-shot CLI verbs — serve covers what an embedder needs
in-process.

## Requests

```json
{"id": 1, "op": "get", "ref": "arca://billing/stripe",
 "env": "prod", "actor": "agent:peregrin", "attestation": "…"}
```

| field | notes |
| --- | --- |
| `id` | any JSON value — echoed verbatim (absent → `null`) |
| `op` | `get` \| `put` \| `rm` \| `rotate` \| `list` \| `check` \| `manifest` \| `shutdown` |
| `ref` | `arca://ns/name` — required by the four vault ops; an `arca://<env>/…` pin overrides `env` |
| `env` | per-request env override — else the loaded context (`--env`/`ARCA_ENV`) |
| `value` | plaintext — required by `put`/`rotate`. Values cross this wire deliberately: the parent owns the channel exactly like it owns `arca get`'s stdout |
| `actor` | claimed actor id — advisory attribution recorded on audit events; validated, never verified |
| `attestation` | opaque identity claim — passed to the PDP verbatim, never interpreted by arca |

## Responses

```json
{"id": 1, "ok": true, "result": {"ref": "arca://billing/stripe", "env": "prod",
 "value": "…", "version": 3, "created_at": "…", "rotated_at": "…"},
 "policy": {"allow": true, "reason": "…", "obligations": ["…"]}}
```

```json
{"id": 2, "ok": false, "error": {"code": "denied", "message": "denied by policy: no prod reads"}}
```

`policy` appears on a response whenever a PDP was consulted for
that op — the same verdict the audit event records.

| error code | meaning |
| --- | --- |
| `bad_request` | malformed line, missing `ref`/`value`, invalid ref/env/actor — the loop keeps serving |
| `unknown_op` | op not in the table (a real CLI verb reports "not served — use the single-shot CLI") |
| `denied` | `[guardrails]` or PDP refusal |
| `pdp_unreachable` | the PDP couldn't answer and `allow_on_error` is false (fail-closed) |
| `not_found` | the ref isn't stored in that env |
| `op_error` | vault/config/io failure |

`shutdown` replies then exits cleanly; EOF on stdin exits too. A
non-UTF-8 line is a `bad_request`, not a crash.

## The policy decision point — `[serve.pdp]`

Configured, serve consults the PDP **before every vault-touching
op** (`get`/`put`/`rm`/`rotate`) — before the vault is even
opened. `list`/`check`/`manifest`/`shutdown` don't consult:
nothing materializes. `[guardrails]` still apply on the serve
path (the advisory layer, checked before the consult).

```toml
[serve.pdp]
exec = "peregrin policy decide"   # sh -c per decision
allow_on_error = false            # default: fail closed
```

The `exec` command runs once per decision — request JSON on
stdin, verdict JSON on stdout, stderr discarded:

```json
{"actor": "agent:peregrin", "attestation": "…", "op": "get",
 "env": "prod", "ref": "billing/stripe"}
```

```json
{"allow": true, "reason": "…", "obligations": ["redact", "session-bound"]}
```

`allow` is required; `reason` and `obligations` are free-form
context the PDP wants on record (allow-with-conditions — arca
records them; enforcing them is the requester's job). A deny is
a `denied` response plus a `denied` audit event — no vault
access.

Spawn failure, non-zero exit, or an unparseable verdict is
**unreachable** → the op is denied (`pdp_unreachable`) unless
`allow_on_error = true`, which runs the op anyway and records
the degraded verdict (`policy.reason` says why). Fail-closed is
the default because an unanswered policy engine must not
silently mean "allowed".

Why `exec` and not a socket or HTTP: zero new dependencies, sync
only, no daemon lifecycle. A PDP that wants a persistent
endpoint can put `socat`/`nc`/`curl` in its own exec line — the
wire stays the same.

## Actor claims are advisory

`actor` and `attestation` are *claims*, not proof — arca
validates the actor string's shape and passes both through to
audit and the PDP. Signed, short-lived attestation is the PDP's
side (peregrin's design, a later decision); ADR-0003 only fixes
the pass-through seam.

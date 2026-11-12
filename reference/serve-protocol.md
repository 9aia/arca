---
url: /cuora/reference/serve-protocol.md
---
# The `cuora serve` protocol

`cuora serve` is the Vault port over newline-delimited JSON — the
surface orchestrators (peregrin) embed. One JSON request per
**stdin** line, one JSON response per **stdout** line, and the
[audit wire](audit-events.md) stays on **stderr**, unchanged —
three separate fds, so a reply never rides the event stream and
an event never answers a request. Protocol version:
`cuora-serve/1` — send `{"op":"manifest"}` on the wire for the
machine-readable self-description.

Serve is a thin front over the same `cuora-ops` plumbing the CLI
uses: `[envs]` routing, `cuora://<env>/…` pins, and audit events
are identical. `run`/`env`/`deploy`/`init`/`migrate`/`schema`/
`gen-cli-docs` stay single-shot CLI verbs — serve covers what an
embedder needs in-process.

## Requests

```json
{"id": 1, "op": "get", "ref": "cuora://billing/stripe",
 "env": "prod", "actor": "agent:peregrin", "attestation": "…"}
```

| field | notes |
| --- | --- |
| `id` | any JSON value — echoed verbatim (absent → `null`), ≤ 4 KiB serialized |
| `op` | `get` | `put` | `rm` | `rotate` | `list` | `check` | `manifest` | `shutdown` |
| `ref` | `cuora://ns/name` ≤ 1 KiB — required by the four vault ops; an `cuora://<env>/…` pin overrides `env` |
| `env` | per-request env override — else the loaded context (`--env`/`CUORA_ENV`) |
| `value` | plaintext ≤ 64 KiB — required by `put`/`rotate`. Values cross this wire deliberately: the parent owns the channel exactly like it owns `cuora get`'s stdout |
| `actor` | claimed actor id ≤ 256 B — advisory attribution recorded on audit events; validated, never verified |
| `attestation` | opaque identity claim ≤ 64 KiB — passed to the PDP verbatim, never interpreted by cuora |

Hard bounds on the wire: a request line may be at most **4 MiB**
— an oversized line answers `bad_request` and the reader resyncs
to the next line, so a broken embedder can't exhaust memory.
Blank or whitespace-only lines get **no reply** — legal
keep-alive, unanswered. A non-UTF-8 line is a `bad_request`, not
a crash.

## Responses

```json
{"id": 1, "ok": true, "result": {"ref": "cuora://billing/stripe", "env": "prod",
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
| `bad_request` | malformed/oversized/non-UTF-8 line, missing `ref`/`value`, invalid ref/env/actor, field over cap — the loop keeps serving |
| `unknown_op` | op not in the table (a real CLI verb reports "not served — use the single-shot CLI") |
| `denied` | `[guardrails]` or PDP refusal |
| `pdp_unreachable` | the PDP couldn't answer (spawn, non-zero exit, timeout, flooded/oversized/unparseable verdict) and `allow_on_error` is false — fail-closed |
| `not_found` | the ref isn't stored in that env |
| `op_error` | vault/config/io failure |

`shutdown` replies then exits cleanly; EOF on stdin exits too.
`manifest` and `shutdown` are protocol meta-ops: they skip
guardrails and the PDP consult by design — gating session
lifecycle is the embedder's spawn decision, not a
materialization.

## The policy decision point — `[serve.pdp]`

Configured, serve consults the PDP **before every op that can
materialize** — `get`/`put`/`rm`/`rotate`, and `list`/`check`
too: `list` decrypts every record's metadata and `check` probes
vaults, including scoped-token `get`s and outbound signed HTTP
against cf vaults. Nothing crosses the vault boundary without a
verdict. `manifest`/`shutdown` are meta-ops and don't consult.
`[guardrails]` still apply on the serve path (the advisory
layer, checked before the consult), and request validation runs
before the consult too — a doomed request never spawns a PDP.

```toml
[serve.pdp]
exec = "peregrin policy decide"   # sh -c per decision
allow_on_error = false            # default: fail closed
timeout_ms = 5000                 # default: 5s per consult
```

A `[serve.pdp] exec` line is **trust-root config**: it
authorizes shell execution. When `cuora serve` loads it from an
*implicit* `./cuora.toml` (the search path, not a pinned
`--config`/`$CUORA_CONFIG` or `~/.config/cuora/`), it warns on
stderr — the working directory decides what code runs.

The `exec` command runs once per decision — request JSON on
stdin, verdict JSON on stdout, stderr discarded. The child
inherits only the `cuora-shell` env allowlist (same scrub
`cuora run` applies): `CUORA_CF_TOKEN`, `CLOUDFLARE_API_TOKEN`,
and every other parent credential stay out of the decider.

```json
{"actor": "agent:peregrin", "attestation": "…", "op": "get",
 "env": "prod", "ref": "billing/stripe"}
```

`ref` is the resolved path with the env pin stripped — and is
**absent** on `list`/`check` consults, which carry no ref.

```json
{"allow": true, "reason": "…", "obligations": ["redact", "session-bound"]}
```

`allow` is required; `reason` and `obligations` are free-form
context the PDP wants on record (allow-with-conditions — cuora
records them; enforcing them is the requester's job). A deny is
a `denied` response plus a `denied` audit event — no vault
access.

A consult is hard-bounded so a misbehaving PDP can't take the
broker with it: the child is killed at `timeout_ms`, the verdict
is capped at 64 KiB (a flooding child blocks on its own pipe and
dies on the deadline — it cannot sink memory), and the request
write can't deadlock against a child that never reads stdin.

Spawn failure, non-zero exit, timeout, an oversized or
unparseable verdict is **unreachable** → the op is denied
(`pdp_unreachable`) unless `allow_on_error = true`, which runs
the op anyway and records the degraded verdict (`policy.reason`
says why). Fail-closed is the default because an unanswered
policy engine must not silently mean "allowed".

Why `exec` and not a socket or HTTP: zero new dependencies, sync
only, no daemon lifecycle. A PDP that wants a persistent
endpoint can put `socat`/`nc`/`curl` in its own exec line — the
wire stays the same. Note the per-op cost: every consult spawns
a fresh `sh -c`, `list` decrypts each record, and `check` probes
every vault — batch reads rather than looping `list`.

## Actor claims are advisory

`actor` and `attestation` are *claims*, not proof — cuora
validates the actor string's shape and passes both through to
audit and the PDP. Signed, short-lived attestation is the PDP's
side (peregrin's design, a later decision); ADR-0003 only fixes
the pass-through seam.

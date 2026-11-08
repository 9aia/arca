# Audit events

The audit wire: one ndjson line on **stderr** per op outcome,
plus an OTLP span per event when `OTEL_EXPORTER_OTLP_ENDPOINT`
is set. arca keeps no local ledger (ADR-0001 — aggregation is
peregrin's); the stderr stream *is* the human contract and the
OTLP stream the machine one, so a wrapper — peregrin, sepia,
your own harness — can capture either, and a human sees a
breadcrumb.

## Shape

```json
{"ts":"2026-01-01T00:00:00Z","actor":"devin","op":"run","rref":"billing/stripe","env":"dev","outcome":"ok"}
```

| field | notes |
| --- | --- |
| `ts` | RFC 3339, UTC |
| `actor` | `--actor` → `ARCA_ACTOR` → `$USER`/`$LOGNAME` → `anon`. Self-claimed — attribution, not identity |
| `op` | the verb: `init put get list rm rotate run env check deploy ui serve manifest schema` |
| `rref` | the resolved ref path, env pin stripped — same on `denied`/`error` when the arg parses, else the raw arg verbatim. **Absent** on op-scoped events |
| `env` | the env the op ran under |
| `outcome` | `ok` \| `denied` \| `error` |
| `policy` | **serve-path only, optional** — the PDP verdict `{allow, reason?, obligations?}` behind the outcome; absent on every other path |

## When events fire

| event | meaning |
| --- | --- |
| `ok`, ref-scoped | one **per ref** materialized or mutated — `put`, `get`, `rotate`, `rm`, `deploy`, and per resolved ref inside `run`/`env` |
| `ok`, op-scoped | ref-less verb completed — `init`, `list`, `check`, `ui`, `serve` |
| `denied` | `[guardrails]` — or a serve-path PDP verdict of `allow: false` (including fail-closed on an unreachable PDP) — refused the op before it touched the vault; `rref` is the requested ref (normalized when parseable) |
| `error` | the op ran and failed — missing secret, vault i/o, bad ref; a `check` with findings also audits `error` (it exits 1); also emitted when config load itself fails (best-effort actor/env from flags + process env) |

Two deliberate silences:

- `manifest`, `schema`, `gen-cli-docs` run before config load —
  pure introspection, no event.
- A `run` child's non-zero exit is the *child's* outcome — no
  `error` event for it. arca's own exit code is the child's.

## `serve` and the `policy` field

`arca serve` emits the same events — `get`/`put`/`rm`/`rotate`
requests audit exactly like the CLI verbs, under the request's
claimed `actor` when it carries one. The PDP consult covers the
ops that can materialize — `get`/`put`/`rm`/`rotate` *and*
`list`/`check` (record decrypts, vault probes, scoped-token
gets) — so a `check`'s incidental token `get`s ride the same
`policy`-bearing events. On the serve path only, every event a
consulted `[serve.pdp]` gated gains a `policy` object —
`{allow, reason?, obligations?}` — so the stream shows the
verdict behind the outcome: a PDP allow, a PDP deny
(`outcome: "denied"`, `policy.allow: false`), or a degraded
allow recorded when `allow_on_error` overrode an unreachable
PDP. The `serve` verb itself closes with one op-scoped `ok`.
See [the serve protocol](serve-protocol.md).

## `ui` and the TUI

`arca ui` emits one op-scoped `ok` when the browser exits cleanly.
Inside it, browsing is silent — listing refs and metadata is not
a materialization, and **live stderr audit is muted for the whole
session** so events can't splatter over the alternate screen;
everything held replays to stderr after the terminal is restored.
The `r`→`r` reveal goes through the audited `get` path: a real
`{"op":"get","rref":…,"outcome":"ok"}` event per value shown —
held like the rest, replayed on exit.

## OTLP export

Set `OTEL_EXPORTER_OTLP_ENDPOINT` (e.g. `http://localhost:4318`)
and every audit event is additionally buffered as an
`arca.<op>` span — `ExportTraceServiceRequest` JSON posted to
`<endpoint>/v1/traces` on flush. Span attributes mirror the
ndjson fields: `arca.op`, `arca.rref` (when present), `arca.env`,
`arca.actor`, `arca.outcome`, and `arca.policy.*` on serve-path
events; `Outcome::Error` marks the span `STATUS_CODE_ERROR`.
`service.name` comes from `OTEL_SERVICE_NAME` (default `arca`).

Flush timing: CLI verbs export once at exit; `arca serve` flushes
per request (it's long-running); the TUI flushes at exit like
any verb.

Related env vars: `OTEL_EXPORTER_OTLP_HEADERS` (`k=v` pairs,
percent-decoded), `OTEL_EXPORTER_OTLP_TIMEOUT` (ms, default
10000), `OTEL_SDK_DISABLED=true` turns export off entirely, and
`ARCA_AUDIT_STDERR=0` silences the stderr wire (OTLP-only mode).

## Failure posture

Audit emission is best-effort on both wires: a closed stderr
(broken pipe) never takes the broker down — the write error is
swallowed by design — and a failed OTLP export warns once on
stderr, never fails the op.

Note: stderr can also carry non-ndjson `arca:` *notice* lines
(permission warnings, keyring fallback notes) — UX chatter, not
audit wire. Consumers parsing stderr as pure ndjson should
tolerate lines that don't start with `{`. During `arca ui` both
audit events and notices are held and replayed after the
terminal is restored.

## Granularity

Per-ref events fire when a value crosses the store boundary
toward a caller: `get`, `put`, `rotate`, `rm`, `deploy`, `run`,
`env`, and serve `get`/`put`/`rotate`/`rm`. Bulk reads — `list`,
`check`, the TUI reload, serve `list` — emit **op-scoped** events
only: they decrypt records internally to verify or enumerate, but
no value is returned to the caller, so there is no per-ref
materialization to attribute. If your threat model counts
"decrypted in-process" as materialization, treat bulk ops as
single events covering N refs — the op name tells you which verb
ran, not which refs it touched.

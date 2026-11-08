# Rotate a secret

Replace a stored value deliberately — version bump plus a
`rotated_at` timestamp, and an audit event.

```sh
printf '%s' "$NEW_VALUE" | cuora rotate cuora://billing/stripe
# rotated cuora://billing/stripe (v3)
```

Like `put`, the value arrives on stdin (or a hidden `/dev/tty`
prompt) — never an argument.

## `rotate` vs re-`put`

| | `put` (upsert) | `rotate` |
| --- | --- | --- |
| Missing ref | creates it (v1) | **fails** — `NotFound` |
| `version` | +1 | +1 |
| `rotated_at` | untouched | stamped |

Rotation is intentional: a missing ref failing loudly catches a
typo'd ref or wrong env that `put` would silently create.

## Rotate in a specific env

```sh
printf '%s' "$PROD_VALUE" | cuora rotate cuora://prod/billing/stripe
# or: --env prod
```

## Verify

```sh
cuora list --json --env prod
# … "version": 3, "rotated_at": "2026-…"
```

Downstream consumers pick the new value up on their next
`cuora run` — resolution happens at exec time, nothing caches it.

If the value was pushed to a deploy sink, re-push it:
[`cuora deploy cuora://billing/stripe`](deploy-to-a-sink.md) — the
sink is write-only and doesn't follow the vault on its own.

---
url: /cuora/how-to/guardrails-for-agents.md
---
# Guardrails for agents

`[guardrails]` is an advisory op table — the owner declaring
"agents get these verbs, not those". It runs before the op
touches the vault, on every verb.

```toml
[guardrails]
deny = ["get", "env", "put", "rm", "rotate", "deploy"]
```

```toml
[guardrails]
default = "deny"               # unlisted ops refused
allow = ["run", "list", "check", "manifest", "schema"]
```

* `deny` wins over `allow`; `default` covers the rest
  (default `"allow"`).
* Op names match `cuora manifest`'s `ops` list:
  `init, put, get, list, rm, rotate, run, env, check, deploy,
  ui, serve, manifest, schema`.
* `manifest`/`schema`/`gen-cli-docs` run before config load —
  introspection is never gated.

## What a denial looks like

```text
cuora: op 'get' denied by guardrails in ./cuora.toml
       (advisory — remove/relax the rule to allow)
```

…plus an `outcome: "denied"` audit event carrying the requested
ref. Exit is non-zero; `--json` emits `{"error": "…"}`.

## Honest scope: advisory, not policy

Guardrails are accident prevention, not enforcement:

* `CUORA_ACTOR` (and `--actor`) is self-claimed — attribution, not
  identity.
* Nothing stops a determined process from editing `cuora.toml` or
  running the binary differently. Real policy enforcement is
  peregrin's job (cuora's orchestrator neighbor); cuora's contract
  is to *honor* the declared rules and *record* every refusal.

If a guardrail denies your op as an agent: don't work around it.
Use an allowed verb (`run` instead of `get`), or surface the
denial to the human — it's the owner telling you the boundary.

## Typical agent posture

Deny every value-egress and mutation verb; allow the
inject-and-run path:

```toml
[guardrails]
default = "deny"
allow = ["run", "list", "check", "ui"]
```

The agent can `cuora run` (values materialize inside the child,
never in its context) and can list refs — but `get`/`env`, the
plaintext printers, are off.

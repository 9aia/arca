# Environment variables

Process env vars cuora itself reads. (The vars in `[secrets]` are
yours — this is cuora's own surface; `cuora manifest` reports the
same table.)

| var | default | used for |
| --- | --- | --- |
| `CUORA_CONFIG` | — | config file override, between `--config` and `./cuora.toml` in the search order |
| `CUORA_ENV` | `dev` | env context — resolution + routing (`--env` wins) |
| `CUORA_ACTOR` | `$USER` → `anon` | audit attribution (`--actor` wins). Self-claimed |
| `CUORA_IDENTITY_FILE` | `~/.config/cuora/identity.txt` | age identity keyfile for every vault impl — pins file mode outright |
| `CUORA_IDENTITY_BACKEND` | `keyring` | `keyring` (default — fail closed, no file fallback) \| `auto` (keyring → keyfile fallback, for headless) \| `file` (never probe the keyring); wins over `[identity] backend` |
| `CUORA_CF_TOKEN` | — | bearer token for `kind = "cf"` vaults — *default* name; `[vaults.*].token_env` picks another |
| `CLOUDFLARE_API_TOKEN` | — | API token for `cf-secrets-store` deploy sinks — *default* name; `[deploy.*].token_env` picks another |
| `CUORA_AUDIT_STDERR` | on | `0`/`false`/`off`/`no` silences the stderr ndjson audit wire — OTLP-only mode |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | — | OTLP/HTTP collector base — audit events export as `cuora.<op>` spans to `<endpoint>/v1/traces`; `http://` is accepted only for loopback collectors (`localhost`, `127.0.0.1`, `::1`) — same policy as cf vault URLs |
| `OTEL_EXPORTER_OTLP_HEADERS` | — | `k=v,k2=v2` auth headers for the OTLP endpoint (values percent-decoded) |
| `OTEL_EXPORTER_OTLP_TIMEOUT` | `10000` | export timeout in ms |
| `OTEL_SERVICE_NAME` | `cuora` | `service.name` resource attribute on exported spans |
| `OTEL_SDK_DISABLED` | — | `true`/`1` disables OTLP export entirely |

Honored XDG paths (when the matching override is unset):

| var | falls back to |
| --- | --- |
| `XDG_CONFIG_HOME` | `~/.config` — identity + user-level `cuora.toml` |
| `XDG_DATA_HOME` | `~/.local/share` — the default `vault.age` |

Two rules of thumb:

- `token_env` config fields name env vars — the token *value*
  only ever lives in the process env.
- `CUORA_ACTOR` is a self-claimed string for the audit trail, not
  authentication; see the [threat model](../explanation/threat-model.md).

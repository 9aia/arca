# Environment variables

Process env vars arca itself reads. (The vars in `[secrets]` are
yours — this is arca's own surface; `arca manifest` reports the
same table.)

| var | default | used for |
| --- | --- | --- |
| `ARCA_CONFIG` | — | config file override, between `--config` and `./arca.toml` in the search order |
| `ARCA_ENV` | `dev` | env context — resolution + routing (`--env` wins) |
| `ARCA_ACTOR` | `$USER` → `anon` | audit attribution (`--actor` wins). Self-claimed |
| `ARCA_IDENTITY_FILE` | `~/.config/arca/identity.txt` | age identity keyfile for every vault impl |
| `ARCA_CF_TOKEN` | — | bearer token for `kind = "cf"` vaults — *default* name; `[vaults.*].token_env` picks another |
| `CLOUDFLARE_API_TOKEN` | — | API token for `cf-secrets-store` deploy sinks — *default* name; `[deploy.*].token_env` picks another |

Honored XDG paths (when the matching override is unset):

| var | falls back to |
| --- | --- |
| `XDG_CONFIG_HOME` | `~/.config` — identity + user-level `arca.toml` |
| `XDG_DATA_HOME` | `~/.local/share` — the default `vault.age` |

Two rules of thumb:

- `token_env` config fields name env vars — the token *value*
  only ever lives in the process env.
- `ARCA_ACTOR` is a self-claimed string for the audit trail, not
  authentication; see the [threat model](../explanation/threat-model.md).

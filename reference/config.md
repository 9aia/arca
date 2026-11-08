---
url: /cuora/reference/config.md
---
# Config — `cuora.toml`

## Top level

| field | type | required | notes |
|---|---|---|---|
| `vaults` | `{name: vault}` | optional | Named vault backends. `local` always exists implicitly (age, default paths); an entry only overrides it or adds more. |
| `envs` | `{env: vault|{vault}}` | optional | Env → vault routing. Envs not listed route to `local`; `dev`/`staging`/`prod` always exist. |
| `secrets` | `{VAR: cuora://ref}` | optional | The declared secret surface: env-var name → `cuora://` ref. Bounds what `run`/`env` may inject — least privilege, no ambient vault dump. |
| `deploy` | `{name: target}` | optional | Named deploy targets — write-only sinks (`cuora deploy` pushes a stored value in; nothing ever reads one back). |
| `guardrails` | `table` | optional | Advisory op guardrails — accident prevention for standalone use, honestly *not* a policy boundary (a locally claimed actor is spoofable by construction). |
| `serve` | `table` | optional | The `cuora serve` surface — ndjson request/response on stdio plus the optional policy decision point. Absent, serve runs with no PDP — same advisory posture as the single-shot CLI. |
| `identity` | `IdentityConfig` | optional | Which backend holds the age identity — `[identity]`. Absent → `keyring` (fail closed; `auto`/`file` opt out). |

## `[vaults.NAME]` — a named vault backend

| field | type | required | notes |
|---|---|---|---|
| `kind` | `string` | optional | Backend kind — `"age"` (built-in, local file) or `"cf"` (self-hosted Cloudflare Worker + D1, `cuora-vault-cf`). |
| `path` | `string` | optional | Vault file path override (age impl; `~` is *not* expanded — write the path in full). |
| `identity` | `string` | optional | Age identity file override (`CUORA_IDENTITY_FILE` wins over this; see `cuora-ops`). `cf` vaults decrypt with the same keyfile — all vault impls store age ciphertext (ADR-0001). |
| `recipients` | `list` | optional | Extra age recipients to encrypt to, beyond the identity's public key (`age1…` strings). |
| `url` | `string` | optional | Worker base URL (`cf` impl), e.g. `https://cuora-vault.you.workers.dev`. |
| `token_env` | `string` | optional | Name of the env var holding the bearer token (`cf` impl) — the token *name* lives in config, the token *value* only in the process env. |
| `tokens` | `BTreeMap<Env, Ref>` | optional | Per-env scoped credentials (`cf` impl, P-003): env → the `cuora://` ref of a token stored in the `local` vault. Each resolved token authenticates as `CuoraHmac &lt;ref-path&gt;, ts, nonce, sig` — the ref path is the wire `token_id`. Token *refs* live in config, never token values. |
| `private_refs` | `bool` | optional | Opt-in private ref metadata (`cf` impl, P-004): storage keys become HMAC-derived hex and every record carries an age-encrypted `name` blob, so the remote store can't see which secrets exist. Default false; `cuora migrate --private-refs` rewrites existing rows. |
| `cache` | `CacheScope` | optional | Read-through encrypted local cache in front of this vault (P-007): `true` caches every env; a name or list scopes it — `cache = "dev"` or `cache = ["dev", "staging"]`. Rows land at `$XDG_CACHE_HOME/cuora/&lt;name&gt;.age` as ordinary age ciphertext carrying the remote `version`/`rotated_at` verbatim; freshness is TTL-bounded (`cache_ttl_secs`) and writes are always remote-first. Meant for remote backends — `dev` may cache, `prod` should live-fetch. |
| `cache_ttl_secs` | `int` | optional | Cache freshness window in seconds — a cached row or env listing younger than this answers without a remote round-trip; past it the remote is re-fetched and the cache updated. A remote *failure* falls back to the stale copy — the offline mode. Absent → 300. |

## `[envs]` — env → vault routing (table form)

| field | type | required | notes |
|---|---|---|---|
| `vault` | `string` | required | Vault name in `[vaults]` (or the implicit `local`). |

## `[deploy.NAME]` — a write-only push target

| field | type | required | notes |
|---|---|---|---|
| `kind` | `string` | optional | Target kind — `"cf-secrets-store"` (Cloudflare account Secrets Store, `cuora-deploy-cf`). |
| `account_id` | `string` | optional | Cloudflare account id (`cf-secrets-store`). |
| `store_id` | `string` | optional | Secrets Store id (`cf-secrets-store`). |
| `token_env` | `string` | optional | Name of the env var holding the API token — never the token itself (`cf-secrets-store` defaults to `CLOUDFLARE_API_TOKEN`). |
| `base_url` | `string` | optional | API base URL override — for tests, gateways, and self-hosted proxies. Defaults to `https://api.cloudflare.com/client/v4`. |

## `[guardrails]` — advisory op table

| field | type | required | notes |
|---|---|---|---|
| `default` | `"allow"|"deny"` | optional | Fallback for ops in neither list. |
| `allow` | `op list` | optional | Ops always allowed. |
| `deny` | `op list` | optional | Ops always refused — deny wins over allow. |
| `deny_adhoc` | `bool` | optional | Refuse `-s VAR=cuora://ref` ad-hoc injection on `run`/`env` (MED-5). Without it, a posture like `deny = ["get"]` is sidestepped: `cuora run -s V=cuora://x -- printenv V` *is* a get. Declared `[secrets]` injection is unaffected — the declared surface is auditable config, not an ad-hoc grab. |

## `[serve]` — the `cuora serve` surface

| field | type | required | notes |
|---|---|---|---|
| `pdp` | `table` | optional | The external policy decision point serve consults before every op that can materialize (`get`/`put`/`rm`/`rotate`/ `list`/`check`). Absent, no consult happens — the advisory posture only. |

## `[serve.pdp]` — the external policy decision point

| field | type | required | notes |
|---|---|---|---|
| `exec` | `string` | required | The exec driver command — run via `sh -c` once per decision, request JSON on stdin, verdict JSON on stdout. **Trust root**: the line authorizes shell exec; the child inherits only the `cuora-shell` env allowlist. `cuora serve` warns when this arrives via an implicit `./cuora.toml` — pin config with `--config`/`$CUORA_CONFIG`. |
| `allow_on_error` | `bool` | optional | What an unreachable PDP means. `false` (default) fails **closed** — the op is denied; a policy engine that can't answer must not silently mean "allowed". `true` fails open — the op runs anyway and the audit event records the degraded verdict. |
| `timeout_ms` | `int` | optional | Per-decision deadline in milliseconds — the child is killed and the consult counts as *unreachable* past it. A hung or flooding PDP must never freeze the serve loop; absent → 5000. |

## `[envs]` route forms

Either a bare vault name or the table above:

```toml
[envs]
dev = "local"
prod = { vault = "cloud" }
```

## Constraints

* `deny_unknown_fields`: typos in field names fail the load.
* Search order: `--config` → `$CUORA_CONFIG` → `./cuora.toml` →
  `~/.config/cuora/cuora.toml`. The file is optional — no
  `cuora.toml` anywhere just means defaults (local age vault,
  `dev` env).
* Token values never live in the file — `token_env` fields name
  the env var that carries them.
* `cuora schema` emits the JSON Schema for this shape;
  `cuora check` validates a real file end-to-end.

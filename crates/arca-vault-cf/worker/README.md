# arca-vault-cf worker

The server half of the `cf` vault kind: a Cloudflare Worker fronting
D1 that stores **opaque age ciphertext only**. All encryption and
decryption happens client-side in `arca-vault-cf`; this worker has no
key material and cannot read the values it serves — plaintext never
crosses its boundary.

## Wire protocol (`arca-vault-cf/1`)

Bearer auth on every request (`Authorization: Bearer <token>`).

| Verb | Path | Body → Response |
| --- | --- | --- |
| `GET` | `/v1/{env}` | `{"refs": ["a/b", …]}` sorted |
| `GET` | `/v1/{env}/{ref}` | `{"ciphertext","version","created_at","rotated_at"}` |
| `PUT` | `/v1/{env}/{ref}` | `{"ciphertext","rotate"}` → record metadata |
| `DELETE` | `/v1/{env}/{ref}` | `{"removed": bool}` |

`PUT` with `"rotate": false` upserts (new → `version=1`; existing →
`version+1`, `created_at` preserved, `rotated_at` untouched).
`"rotate": true` additionally stamps `rotated_at` and 404s on a
missing ref. Bookkeeping is server-side so writers can't race
client-computed versions. Errors are `{"error": "…"}` + non-2xx.

The reference implementation of these semantics for tests lives in
`../tests/common/mod.rs` — keep the two in sync.

## Deploy

```bash
cd crates/arca-vault-cf/worker

# 1. create + migrate the database
wrangler d1 create arca-vault          # prints a database_id
#    → paste it into wrangler.toml [[d1_databases]]
wrangler d1 execute arca-vault --file=schema.sql

# 2. set the bearer token (Workers secret, never in the repo)
wrangler secret put ARCA_VAULT_TOKEN   # paste a strong token

# 3. ship it
wrangler deploy
# → https://arca-vault.<you>.workers.dev
```

## Point arca at it

```toml
# arca.toml
[vaults.cloud]
kind = "cf"
url = "https://arca-vault.<you>.workers.dev"
token_env = "ARCA_CF_TOKEN"        # env var name, not the token
identity = "~/.config/arca/identity.txt"   # optional; same default as local
recipients = ["age1…"]             # optional extra encrypt-to keys

[envs]
prod = { vault = "cloud" }
```

```bash
export ARCA_CF_TOKEN=<the same token>
arca put arca://billing/stripe --env prod   # encrypts, PUTs ciphertext
arca list --env prod                        # reads ciphertext rows back
```

`arca check` probes the vault with a `list` call — an unreachable or
mis-tokened worker reports `ok: false`, never a panic.

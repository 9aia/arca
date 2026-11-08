---
url: /cuora/how-to/cloudflare-worker-setup.md
---
# Cloudflare Worker setup

The `cf` vault kind is two halves: `cuora-vault-cf` in the CLI
(client-side age encryption + blocking HTTP) and a Worker you
self-host — a thin store fronting D1 that holds **opaque
ciphertext only**. The worker has no key material and cannot read
what it serves.

## What you're deploying

The worker source, schema, and the authoritative deploy
walkthrough live in the repo:

* [`crates/cuora-vault-cf/worker/README.md`](https://github.com/9aia/cuora/blob/main/crates/cuora-vault-cf/worker/README.md)
  — wire protocol (`cuora-vault-cf/2`), `wrangler` steps
* `worker/index.js` — the Worker (constant-time auth, scoped
  tokens, CuoraHmac verification + REST verbs)
* `worker/schema.sql` — the D1 table (`name` column added in v2 —
  `ALTER TABLE secrets ADD COLUMN name TEXT` on an existing db)
* `worker/wrangler.toml` — binding config

The short version:

```sh
cd crates/cuora-vault-cf/worker
wrangler d1 create cuora-vault                  # → paste database_id into wrangler.toml
wrangler d1 execute cuora-vault --file=schema.sql
wrangler secret put CUORA_VAULT_TOKEN           # legacy single bearer, or:
wrangler secret put CUORA_VAULT_TOKENS          # scoped token map (JSON) — below
wrangler deploy
```

Then point a vault at it in `cuora.toml`:

```toml
[vaults.cloud]
kind = "cf"
url = "https://cuora-vault.you.workers.dev"
token_env = "CUORA_CF_TOKEN"

[envs]
prod = { vault = "cloud" }
```

and export the bearer token under the name `token_env` chose
(`export CUORA_CF_TOKEN=…`).

## Scoped tokens (wire v2)

One static bearer for every env is a big blast radius. Wire v2
scopes tokens per env: the worker reads `CUORA_VAULT_TOKENS`, a JSON
map of `token_id → {token, scopes}`, and enforces
`{env|"*" → {read, write}}` after auth — a leaked `dev` token can't
read `prod`.

```json
{
  "ops/cf-token-dev": {
    "token": "<dev-token-secret>",
    "scopes": { "dev": { "read": true, "write": true } }
  },
  "ops/cf-token-viewer": {
    "token": "<viewer-secret>",
    "scopes": { "*": { "read": true, "write": false } }
  }
}
```

On the client side, each scoped credential is an `cuora://` ref —
the token itself lives as a secret in the **`local` vault** under
the env it scopes; the ref path is the wire `token_id`:

```toml
[vaults.cloud.tokens]
dev = "cuora://ops/cf-token-dev"
prod = "cuora://ops/cf-token-prod"
```

```sh
printf '%s' '<dev-token-secret>' | cuora put cuora://ops/cf-token-dev --env dev
printf '%s' '<prod-token-secret>' | cuora put cuora://ops/cf-token-prod --env prod
```

With a scoped token configured the client signs every request —
`Authorization: CuoraHmac &lt;token_id&gt;, &lt;ts&gt;, &lt;sig&gt;` where
`sig = hex HMAC-SHA256(token, method+path+ts+sha256(body))` and `ts`
must be within the worker's 60-second replay window. **The token
never travels.** `Bearer` stays accepted during migration, so a
mixed fleet upgrades gracefully.

**Rotation is `wrangler secret put`** — replace the value inside
`CUORA_VAULT_TOKENS`, then `cuora put` the new secret at the token
ref. There is deliberately no admin route.

## Private refs (opt-in)

By default `ref` and `env` are visible to the store — values are
ciphertext, but *which secrets exist* is metadata. With
`private_refs = true` the client derives
`ref_key = HMAC(identity-derived-key, env + "/" + ref)` and uses it
as the storage key; each record also carries a `name` blob — age
ciphertext of the real ref — so `list` maps keys back while the
store sees only hex:

```toml
[vaults.cloud]
kind = "cf"
url = "…"
private_refs = true
```

Existing rows aren't rewritten by the flag alone — run:

```sh
cuora migrate --private-refs
```

which re-keys every plaintext-named row (ciphertext moves verbatim,
old keys deleted after new ones land). A vault mixing plain and
private rows is **mixed-mode**: `list`/`check` fail closed until
you migrate — deliberately, since reading the wrong interpretation
silently is worse than failing. Turning `private_refs` off again on
a private vault fails closed the other way; migrating back is a
manual re-put.

## Client-side crypto boundary

`put` seals the value with age **before** it leaves the machine;
`get` unseals after it arrives. The wire — and the D1 row —
carries `{"ciphertext", "name", "version", created_at, rotated_at}`.
A compromised worker yields ciphertext and, at most, opaque keys —
not values, not ref names (with `private_refs`). Your local
`identity.txt` is the only key, same file the local age vault uses;
the private-refs derivation is keyed by it, so rotating the
identity means a full re-key + migrate.

## Verify

```sh
cuora check --env prod
# vault "cloud" (cf): readable     ← probes the worker over HTTPS
```

An unreachable worker or a bad token shows up as a finding —
`check` fails open in the report, never panics.

## Deploy sinks vs the vault

Don't confuse the two CF surfaces:

* `[vaults.cloud] kind = "cf"` — a *readable* ciphertext store
  (your Worker + D1). Source of truth.
* `[deploy.prod] kind = "cf-secrets-store"` — a *write-only*
  Cloudflare Secrets Store sink `cuora deploy` pushes plaintext
  into for Workers to consume via bindings. See
  [deploy to a sink](deploy-to-a-sink.md).

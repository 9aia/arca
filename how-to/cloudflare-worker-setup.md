# Cloudflare Worker setup

The `cf` vault kind is two halves: `arca-vault-cf` in the CLI
(client-side age encryption + blocking HTTP) and a Worker you
self-host — a thin store fronting D1 that holds **opaque
ciphertext only**. The worker has no key material and cannot read
what it serves.

## What you're deploying

The worker source, schema, and the authoritative deploy
walkthrough live in the repo:

- [`crates/arca-vault-cf/worker/README.md`](https://github.com/9aia/arca/blob/main/crates/arca-vault-cf/worker/README.md)
  — wire protocol (`arca-vault-cf/1`), `wrangler` steps
- `worker/index.js` — the Worker ( bearer auth + REST verbs )
- `worker/schema.sql` — the D1 table
- `worker/wrangler.toml` — binding config

The short version:

```sh
cd crates/arca-vault-cf/worker
wrangler d1 create arca-vault                  # → paste database_id into wrangler.toml
wrangler d1 execute arca-vault --file=schema.sql
wrangler secret put ARCA_VAULT_TOKEN           # the bearer token
wrangler deploy
```

Then point a vault at it in `arca.toml`:

```toml
[vaults.cloud]
kind = "cf"
url = "https://arca-vault.you.workers.dev"
token_env = "ARCA_CF_TOKEN"

[envs]
prod = { vault = "cloud" }
```

and export the bearer token under the name `token_env` chose
(`export ARCA_CF_TOKEN=…`).

## Client-side crypto boundary

`put` seals the value with age **before** it leaves the machine;
`get` unseals after it arrives. The wire — and the D1 row —
carries `{"ciphertext", "version", created_at, rotated_at}`.
A compromised worker yields ciphertext and ref names, not values.
Your local `identity.txt` is the only key, same file the local
age vault uses.

## Verify

```sh
arca check --env prod
# vault "cloud" (cf): readable     ← probes the worker over HTTPS
```

An unreachable worker or a bad token shows up as a finding —
`check` fails open in the report, never panics.

## Deploy sinks vs the vault

Don't confuse the two CF surfaces:

- `[vaults.cloud] kind = "cf"` — a *readable* ciphertext store
  (your Worker + D1). Source of truth.
- `[deploy.prod] kind = "cf-secrets-store"` — a *write-only*
  Cloudflare Secrets Store sink `arca deploy` pushes plaintext
  into for Workers to consume via bindings. See
  [deploy to a sink](deploy-to-a-sink.md).

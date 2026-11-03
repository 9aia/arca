---
url: /arca/how-to/deploy-to-a-sink.md
---
# Deploy a secret to a sink

`arca deploy` pushes a vault-stored value to a write-only target —
a `[deploy.NAME]` table in `arca.toml`. The vault is the source of
truth; the sink serves your runtimes directly (e.g. a Worker
reading a Secrets Store binding).

## Configure a target

```toml
[deploy.prod]
kind = "cf-secrets-store"      # Cloudflare account Secrets Store
account_id = "00000000000000000000000000000000"
store_id   = "00000000000000000000000000000000"
# token_env = "CLOUDFLARE_API_TOKEN"   # default; env var NAME only
```

The API token comes from the env var named by `token_env`
(default `CLOUDFLARE_API_TOKEN`) — never from the file.

## Push

```sh
arca deploy arca://billing/stripe --target prod
# deployed arca://billing/stripe → "prod" as dev_billing_stripe (created)
```

* `--target` may be omitted when exactly one `[deploy.*]` table is
  configured; several targets make it required.
* The sink-side name is derived and stable —
  `env + ref-path` underscored (`dev_billing_stripe`).
* Re-deploying updates in place (`updated`, not `created`).

## The direction is one-way

Deploy targets are **sinks**, not sources: the plaintext crosses
to the sink exactly once, at push time, and nothing ever reads it
back through arca. A deploy emits a `deploy` audit event for the
ref.

## Rotate → re-deploy

`arca rotate` changes the vault; the sink doesn't follow on its
own. Re-push after rotation:

```sh
printf '%s' "$NEW_VALUE" | arca rotate arca://prod/billing/stripe
arca deploy arca://prod/billing/stripe --target prod
```

## Verify

`arca check` validates deploy tables (kind known, `account_id` +
`store_id` present) as findings — it doesn't push anything.

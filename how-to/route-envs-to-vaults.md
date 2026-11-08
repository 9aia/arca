---
url: /cuora/how-to/route-envs-to-vaults.md
---
# Route envs to vaults

Envs are the routing axis: `[envs]` maps each env name to a
`[vaults]` backend, so `dev` can stay on the local age file while
`prod` reads from a remote ciphertext store.

## The tables

```toml
[vaults.local]                 # the built-in default — entry optional
kind = "age"
# path / identity / recipients  # optional overrides

[vaults.cloud]
kind = "cf"                    # self-hosted Worker + D1, age ciphertext
url = "https://cuora-vault.you.workers.dev"
token_env = "CUORA_CF_TOKEN"    # env var NAME — token value stays in env

[envs]
dev = "local"                  # bare-name form
prod = { vault = "cloud" }     # table form
```

Rules:

* `local` always exists implicitly (age, default paths) — a
  `[vaults.local]` entry only overrides it.
* Envs not listed route to `local`. `dev`/`staging`/`prod` always
  exist; `[envs]` keys add custom envs (`ci`, `qa`, …).
* `token_env` names the env var carrying the credential — the
  *name* is in config, the *value* only in the process env.
* `CUORA_IDENTITY_FILE` overrides the age identity path globally.

## What changes at run time

```sh
cuora list --env prod          # opens the "cloud" vault over HTTPS
cuora put cuora://prod/x/y      # pin → writes to "cloud"
cuora run --env prod -- ./app  # [secrets] resolve under prod → cloud
```

The ref spelling doesn't change — `cuora://ns/name` is env-agnostic;
routing decides which backend serves it.

## The `cf` backend

`kind = "cf"` talks to your own Cloudflare Worker fronting D1.
All encryption is client-side: the worker stores opaque age
ciphertext and has no key material. Setup is a how-to of its own —
see [Cloudflare Worker setup](cloudflare-worker-setup.md).

## Verify routing

```sh
cuora check
# env: dev → vault "local"
# vault "local" (age): readable
# vault "cloud" (cf): readable      ← actually probed over the wire
```

`check` reports every configured and routed vault — including
ones no env currently routes to — and fails on routes pointing at
undeclared vault names.

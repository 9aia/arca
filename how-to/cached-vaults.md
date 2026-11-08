---
url: /cuora/how-to/cached-vaults.md
---
# Cache a remote vault locally

A remote vault (`kind = "cf"`) costs a network round-trip per
read and goes hard-down offline. `[vaults.*] cache` fronts it
with a read-through, age-encrypted local mirror — `get`/`list`
serve the cache while it's fresh, misses fetch remote and
write-through, and a remote failure falls back to the stale copy.

## Turn it on

```toml
[vaults.cloud]
kind = "cf"
url = "https://cuora-vault.you.workers.dev"
token_env = "CUORA_CF_TOKEN"
cache = ["dev"]              # cache only the dev env
# cache = true              # or: cache every env
# cache_ttl_secs = 600      # freshness window, default 300
```

Three scope forms — `true` for every env, `"dev"` for one,
`["dev", "staging"]` for a set. `cache = false` and `cache = []`
are the same as leaving it out.

## What lands on disk

Rows mirror into `$XDG_CACHE_HOME/cuora/<vault>.age` (else
`~/.cache/cuora/`) — an ordinary age vault encrypted to the same
identity and recipients as the vault itself, so the cache needs
no new key material and no new threat surface. Each cached row
carries the remote `version`/`rotated_at` verbatim plus a
`fetched_at` stamp; a tiny `cloud.listed.json` sidecar holds
per-env list markers (env names and timestamps, never refs or
values). It's a cache: delete the directory and it rebuilds.

## Freshness model

* **Within the TTL** a cached `get`/`list` answers with no
  network at all — a burst of `cuora run`s costs one round-trip.
* **Past the TTL** the read goes remote and rewrites the row.
* **Remote unreachable** — the stale copy answers. That's the
  offline mode the cache exists for, not an error.
* **Writes never serve stale**: `put`/`rotate`/`rm` always hit
  the remote first, then update or invalidate the mirror. A
  remote `rm` propagates into the cache on the next `list`.

The tradeoff to know: within the TTL another writer's change
isn't visible — the remote's own change is. Shorten
`cache_ttl_secs` if your env rotates often.

## Scope it — prod stays live

```toml
[envs]
dev = { vault = "cloud" }    # cached — fast, offline-capable
prod = { vault = "cloud" }   # out of scope → every read live-fetches
```

`cache = ["dev"]` means prod rows *never touch the disk mirror* —
every prod verb is a straight remote call. A stale prod secret
is a footgun; the scope is the safety rail.

## Verify

```sh
cuora get cuora://a/b       # first call: remote fetch + write-through
cuora get cuora://a/b       # second call: cache hit, no round-trip
ls ~/.cache/cuora/         # cloud.age + cloud.listed.json
cuora check                # probes the remote like before
```

A cache file that won't decrypt is moved aside to
`<vault>.corrupt` and rebuilt — corruption degrades to
remote-only reads, never to a wrong answer.

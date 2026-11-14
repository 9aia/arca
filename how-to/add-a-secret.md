# Add a secret

Store a value under an `cuora://` ref in the vault backing an env.

## The pipe path (scripts and agents)

```sh
printf '%s' "$VALUE" | cuora put cuora://ns/name
```

- stdin is read to EOF; **one** trailing newline is stripped, the
  rest is verbatim (multi-line values work).
- Empty input fails — you can't store a blank secret.
- The value is never an argument: args land in shell history and
  `/proc`.

Other sources that already hold the value pipe the same way:

```sh
pass show stripe/test | cuora put cuora://billing/stripe
op read "op://vault/stripe/test" | cuora put cuora://billing/stripe --env dev
```

## The interactive path (humans)

```sh
cuora put cuora://ns/name          # stdin is a tty → hidden /dev/tty prompt
```

The prompt doesn't echo — paste the value, hit enter.

## Pick the env

Env is resolution *context*, not part of the name — the same
`cuora://ns/name` ref can hold different values per env:

```sh
printf dev-token   | cuora put cuora://billing/stripe --env dev
printf prod-token  | cuora put cuora://prod/billing/stripe   # env pin
```

`cuora://prod/…` pins the env regardless of `--env`/`CUORA_ENV`;
the bare form resolves under the selected env.

## Env-agnostic secrets — pick a home first

Some secrets have one value regardless of environment (registry
tokens, signing material, machine credentials). Don't replicate
them across envs by default — choose deliberately:

| Kind | Home |
| ---- | ---- |
| env-agnostic, **read-only** for consumers (tokens, keys) | a sealed pack (`cuora://<pack>/<name>` — no env axis at all; see [sealed packs](sealed-packs.md)) |
| env-agnostic, needs `rotate`/`deploy`/vault writes | vault **`prod` only** — convention: publisher/operator credentials resolve under `--env prod`; consumers that need them run in prod context |
| genuinely per-env values | every env that needs it, as above |

There is no `put --env all`: replication is a deliberate act
(`get --env prod … | put --env <e> …` per extra env), which keeps
env drift visible instead of silently fanning out.

## Upsert semantics

`put` is an upsert: a new ref starts at `version = 1`; re-putting
an existing ref replaces the value and bumps the version but is
*not* a rotation (`rotated_at` stays untouched — use
[`cuora rotate`](rotate-a-secret.md) for deliberate rotations).

## Remove

```sh
cuora rm cuora://ns/name
```

## Verify

```sh
cuora list [--env prod]      # refs only — never values
cuora check                  # declared [secrets] entries resolve?
```

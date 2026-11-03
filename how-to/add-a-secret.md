---
url: /arca/how-to/add-a-secret.md
---
# Add a secret

Store a value under an `arca://` ref in the vault backing an env.

## The pipe path (scripts and agents)

```sh
printf '%s' "$VALUE" | arca put arca://ns/name
```

* stdin is read to EOF; **one** trailing newline is stripped, the
  rest is verbatim (multi-line values work).
* Empty input fails — you can't store a blank secret.
* The value is never an argument: args land in shell history and
  `/proc`.

Other sources that already hold the value pipe the same way:

```sh
pass show stripe/test | arca put arca://billing/stripe
op read "op://vault/stripe/test" | arca put arca://billing/stripe --env dev
```

## The interactive path (humans)

```sh
arca put arca://ns/name          # stdin is a tty → hidden /dev/tty prompt
```

The prompt doesn't echo — paste the value, hit enter.

## Pick the env

Env is resolution *context*, not part of the name — the same
`arca://ns/name` ref can hold different values per env:

```sh
printf dev-token   | arca put arca://billing/stripe --env dev
printf prod-token  | arca put arca://prod/billing/stripe   # env pin
```

`arca://prod/…` pins the env regardless of `--env`/`ARCA_ENV`;
the bare form resolves under the selected env.

## Upsert semantics

`put` is an upsert: a new ref starts at `version = 1`; re-putting
an existing ref replaces the value and bumps the version but is
*not* a rotation (`rotated_at` stays untouched — use
[`arca rotate`](rotate-a-secret.md) for deliberate rotations).

## Remove

```sh
arca rm arca://ns/name
```

## Verify

```sh
arca list [--env prod]      # refs only — never values
arca check                  # declared [secrets] entries resolve?
```

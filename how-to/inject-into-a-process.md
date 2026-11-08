# Inject secrets into a process

`cuora run` is the agent path — resolved values become env vars in
the child and nowhere else. `cuora env` is the human path — export
lines for `eval`.

## Declare the surface

`[secrets]` in `cuora.toml` bounds what `run`/`env` may inject:

```toml
[secrets]
STRIPE_KEY   = "cuora://billing/stripe"
DATABASE_URL = "cuora://prod/db/url"     # env pin: prod, always
```

Nothing outside this table (plus `-s` ad-hoc pairs) is injectable
— least privilege, and `cuora check` verifies every entry resolves.

## `cuora run` — two exec modes

```sh
# one arg → bash -c — pipes, redirects, $VAR expansion
cuora run -- 'psql "$DATABASE_URL" -c "select 1" | tee out.log'

# several args → argv[0] exec'd directly, NO shell —
# the program reads its own environment
cuora run -- stripe charges list --limit 3
```

Prefer argv mode when the command has arguments — no quoting
puzzles. One caveat: with no shell in the child there is no
`$VAR` expansion — write `$STRIPE_KEY` in argv and *your* outer
shell eats it (to an empty string) before cuora runs. Use `bash
-c` mode when the command line itself needs the value.

Exit code is the child's; a child's non-zero exit is not an cuora
failure (no `error` audit event).

## Ad-hoc pairs: `-s VAR=cuora://ref`

Repeatable, merged over `[secrets]` (same `VAR` — the `-s` pair
wins):

```sh
cuora run -s TEMP_TOKEN=cuora://ci/token -- ./deploy.sh
```

## Missing secrets fail closed

If any declared or `-s` ref isn't stored in its env, `run` fails
before spawning — partial injection is worse than none.

## `cuora env` — the human path

```sh
eval "$(cuora env)"                              # export every declared VAR
eval "$(cuora env -s EXTRA=cuora://misc/extra)"   # plus ad-hoc pairs
cuora env --json                                 # {"VAR": "value"} map
```

Output is `export VAR='…'` with shell-safe quoting for any value.
This prints plaintext into your shell environment — that's why
it's the *human* path: agents should `run` instead. A
`[guardrails]` deny on `env` is the owner's hint.

## The audit trail

Each resolved ref emits one materialization event on stderr —
`op` is `run` or `env`, `rref` the ref path, `outcome: "ok"`. See
[the audit wire](../reference/audit-events.md).

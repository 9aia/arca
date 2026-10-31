# Getting started

Install the `arca` CLI, seal your first secret in the local
age-encrypted vault, and run a command that uses it — without the
value ever touching your terminal, a file, or an agent's context.

## Install

The crates.io name `arca` belongs to an unrelated project —
install from the repo:

```sh
cargo install --git https://github.com/9aia/arca arca
```

From a checkout of this repo: `cargo xtask install` builds and
installs the release binary to `~/.cargo/bin/arca`. For a lean
binary without the TUI:
`cargo install --git https://github.com/9aia/arca arca --no-default-features`.

## Bootstrap

```sh
arca init
```

`init` generates your age identity at
`~/.config/arca/identity.txt` (mode `0600`) if it's missing and
verifies the vault at `~/.local/share/arca/vault.age` opens. It's
idempotent — run it any time.

## Seal a secret

Values never enter via CLI arguments — they would land in shell
history and `/proc`. Pipe them instead:

```sh
printf '%s' "$STRIPE_KEY" | arca put arca://billing/stripe
```

Interactive humans get a hidden `/dev/tty` prompt instead of
stdin. The value is encrypted to your identity and stored under
the `dev` env (the default context).

## Declare the surface

`[secrets]` in `arca.toml` bounds what `run`/`env` may inject —
least privilege, no ambient vault dump:

```toml
[secrets]
STRIPE_KEY = "arca://billing/stripe"
```

Config search: `./arca.toml` → `~/.config/arca/arca.toml`
(`--config`/`$ARCA_CONFIG` override). No file at all is fine —
defaults are the local vault and the `dev` env.

## Run with secrets

```sh
arca run -- 'curl -H "Authorization: Bearer $STRIPE_KEY" \
    https://api.stripe.com/v1/charges'
```

`run` resolves the declared refs, injects them as env vars into
the child only, and exits with the child's code. One arg after
`--` runs via `bash -c` (shell syntax, as above); several args
exec `argv[0]` directly with no shell. The value lives only in
that process — agents never see it.

## Check the setup

```sh
arca check
```

Validates the config, that the routed vaults decrypt, and that
every `[secrets]` ref resolves — exit `1` on findings.

## Agent self-discovery

```sh
arca manifest          # verbs, JSON shapes, env vars, audit contract (JSON)
arca schema            # JSON Schema for arca.toml
arca <verb> --help     # every verb, self-documenting
```

`--json` is honored on every verb; errors are `{"error": "…"}`
on stderr with a non-zero exit. The
[arca skill](https://github.com/9aia/arca/tree/main/.agents/skills/arca)
teaches agents the verbs.

## Where next

- [Your first secret](tutorials/first-secret.md) — a guided
  end-to-end in a throwaway HOME.
- [Inject into a process](how-to/inject-into-a-process.md) — run
  modes, ad-hoc `-s` pairs, the `eval "$(arca env)"` human path.
- [The threat model](explanation/threat-model.md) — what arca
  does and does not protect against.
- [CLI reference](reference/cli.md) — the full verb surface.

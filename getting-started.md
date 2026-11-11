# Getting started

Install the `cuora` CLI, seal your first secret in the local
age-encrypted vault, and run a command that uses it — without the
value ever touching your terminal, a file, or an agent's context.

## Install

The `cuora` command ships as a tamias pack (`libraries/cuora`) — a
generated `bins[]` shim over `tamias-driver-cuora`, not a
cargo-installed binary. From a checkout of this repo:

```sh
cargo xtask pack                            # stage the driver payload
tamias install --allow-drivers ./libraries/cuora   # deploys `cuora`
```

`cargo xtask install` additionally puts `tamias-driver-cuora`
itself on `PATH` (`~/.cargo/bin`).

## Bootstrap

```sh
cuora init
```

`init` stores your age identity where it's safest available and
verifies the vault at `~/.local/share/cuora/vault.age` opens. It's
idempotent — run it any time.

On a desktop session (GNOME Keyring, KeePassXC, KDE Wallet, macOS
Keychain, Windows Credential Manager) the identity lands in the
session keyring — it unlocks with login, no prompt. The default
backend is `keyring`: **fail closed** — if no session store is
reachable, `init` (and every other verb) errors rather than
silently downgrading to a file. Headless boxes (servers, CI,
containers, plain SSH) have no session bus, so there you opt out
explicitly — `[identity] backend = "auto"` in `cuora.toml` (probe
the keyring, fall back to the keyfile with a note), `"file"`
(never probe), or the `CUORA_IDENTITY_BACKEND` env var. The
fallback writes `~/.config/cuora/identity.txt` (mode `0600`) —
raw age key material, protect it like a private SSH key.
`cuora init --with-file` forces the keyfile; pinning
`CUORA_IDENTITY_FILE` or `[vaults.*].identity` in config does the
same — that's the deliberate headless/CI path.

Already have an `identity.txt`? `cuora init` imports it into the
keyring rather than orphaning it, and leaves the file as a backup
you can delete when ready. (On a brand-new keyring with no
collections yet, cuora claims the `default` collection alias
automatically — no manual step.)

## Seal a secret

Values never enter via CLI arguments — they would land in shell
history and `/proc`. Pipe them instead:

```sh
printf '%s' "$STRIPE_KEY" | cuora put cuora://billing/stripe
```

Interactive humans get a hidden `/dev/tty` prompt instead of
stdin. The value is encrypted to your identity and stored under
the `dev` env (the default context).

## Declare the surface

`[secrets]` in `cuora.toml` bounds what `run`/`env` may inject —
least privilege, no ambient vault dump:

```toml
[secrets]
STRIPE_KEY = "cuora://billing/stripe"
```

Config search: `./cuora.toml` → `~/.config/cuora/cuora.toml`
(`--config`/`$CUORA_CONFIG` override). No file at all is fine —
defaults are the local vault and the `dev` env.

## Run with secrets

```sh
cuora run -- 'curl -H "Authorization: Bearer $STRIPE_KEY" \
    https://api.stripe.com/v1/charges'
```

`run` resolves the declared refs, injects them as env vars into
the child only, and exits with the child's code. One arg after
`--` runs via `bash -c` (shell syntax, as above); several args
exec `argv[0]` directly with no shell. The value lives only in
that process — agents never see it.

## Check the setup

```sh
cuora check
```

Validates the config, that the routed vaults decrypt, and that
every `[secrets]` ref resolves — exit `1` on findings.

## Agent self-discovery

```sh
cuora manifest          # verbs, JSON shapes, env vars, audit contract (JSON)
cuora schema            # JSON Schema for cuora.toml
cuora <verb> --help     # every verb, self-documenting
```

`--json` is honored on every verb; errors are `{"error": "…"}`
on stderr with a non-zero exit. The
[cuora skill](https://github.com/9aia/cuora/tree/main/.agents/skills/cuora)
teaches agents the verbs.

## Where next

- [Your first secret](tutorials/first-secret.md) — a guided
  end-to-end in a throwaway HOME.
- [Inject into a process](how-to/inject-into-a-process.md) — run
  modes, ad-hoc `-s` pairs, the `eval "$(cuora env)"` human path.
- [The threat model](explanation/threat-model.md) — what cuora
  does and does not protect against.
- [CLI reference](reference/cli.md) — the full verb surface.

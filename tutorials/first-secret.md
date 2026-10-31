# Your first secret

A full arca lifecycle — bootstrap, seal, declare, inject, audit —
in a throwaway `HOME` so nothing touches your real config. All
values below are fake.

## Setup

```sh
export HOME="$(mktemp -d)"        # scratch HOME for this tutorial
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_DATA_HOME="$HOME/.local/share"

arca init
```

```text
identity: /tmp/tmp.…/.config/arca/identity.txt (generated)
recipient: age1…
vault:     /tmp/tmp.…/.local/share/arca/vault.age
```

On stderr you'll also see the first audit event — every arca op
emits one ndjson line there:

```json
{"ts":"2026-01-01T00:00:00Z","actor":"you","op":"init","env":"dev","outcome":"ok"}
```

## Seal a value

```sh
printf '%s' 'sk_test_FAKE000000000000000000' | arca put arca://billing/stripe
```

```text
stored arca://billing/stripe (v1)
```

The plaintext went in on stdin — never an argument. What's on
disk is age ciphertext:

```sh
file "$XDG_DATA_HOME/arca/vault.age"   # data — not readable
```

## Declare and inject

```sh
cat > arca.toml <<'EOF'
[secrets]
STRIPE_KEY = "arca://billing/stripe"
EOF

arca run -- 'echo "key starts with: ${STRIPE_KEY:0:7}…"'
```

```text
key starts with: sk_test…
```

The child saw `$STRIPE_KEY` as a normal env var; nothing else —
not your shell, not a file — ever held it. And the audit wire
recorded the materialization:

```json
{"ts":"…","actor":"you","op":"run","rref":"billing/stripe","env":"dev","outcome":"ok"}
```

## List without spilling

```sh
arca list
# arca://billing/stripe

arca list --json
# [{"ref":"arca://billing/stripe","env":"dev","version":1,"created_at":"…","rotated_at":null}]
```

`list` is names plus metadata — never values. The only surfaces
that print a value are `get` (the escape hatch) and `env`
(exports for `eval`).

## Rotate and verify

```sh
printf '%s' 'sk_test_FAKE111111111111111111' | arca rotate arca://billing/stripe
# rotated arca://billing/stripe (v2)

arca check
# config: /tmp/tmp.…/arca.toml
# env: dev → vault "local"
# vault "local" (age): readable
# secret STRIPE_KEY → arca://billing/stripe (env dev): resolves
# ok
```

`rotate` requires the ref to exist (it's deliberate rotation,
not upsert), bumps the version, and stamps `rotated_at`.

## Clean up

```sh
rm -rf "$HOME"      # the scratch vault and identity go with it
```

## Where next

- [Add a secret](../how-to/add-a-secret.md) — multi-env and
  pinning.
- [Route envs to vaults](../how-to/route-envs-to-vaults.md) —
  `prod` to a remote vault.
- [The audit wire](../reference/audit-events.md) — the ndjson
  contract.

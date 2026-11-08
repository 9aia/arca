+++
id = "ADR-0004"
title = "identity backend selection — fail-closed keyring default, explicit file opt-out"
status = "open"
created = "2026-11-08"
owner = "devin"
priority = "high"
labels = ["identity", "security"]
+++

## Context

ADR-0002 made the session keyring preferred with an automatic
fallback to the `0600` keyfile. That fallback is a *silent*
security downgrade: a desktop session that loses its store (daemon
crash, locked collection, headless SSH) quietly reads raw age key
material from disk while the operator believes keyring protection
is active. For a secrets broker, secure-by-default wins.

## Decision

`[identity] backend` in `arca.toml` / `ARCA_IDENTITY_BACKEND`
selects the identity source, resolved env > config > default:

- **`keyring` (default)** — fail closed. Unreachable store or
  missing identity is an error naming the opt-out
  (`ARCA_IDENTITY_BACKEND=auto|file`); the keyfile is never read.
- **`auto`** — ADR-0002's behavior preserved as an explicit
  opt-out: probe the store, fall back to the keyfile with a
  one-time stderr note. For headless/CI, which have no session
  bus (verified: no Secret Service provider exists without one;
  workarounds like `dbus-run-session` + `gnome-keyring-daemon
  --unlock` are fragile, especially containers).
- **`file`** — never probe the store; equivalent to
  `arca init --with-file`.

Explicit pins still win over everything: `ARCA_IDENTITY_FILE`
then `[vaults.*].identity` force file mode and now emit a
one-time note when they override `backend=keyring`.

Unrecognized `ARCA_IDENTITY_BACKEND` values are ignored with a
stderr note — a typo'd opt-out must not silently fail closed.

## Consequences

- Breaking for unconfigured headless setups: they must set
  `backend = "auto"` or `"file"` once. Accepted — explicit beats
  implicit for key material.
- `arca init` on a reachable-but-empty store still bootstraps
  (I-006 collection aliasing unchanged); migration of an existing
  keyfile into the keyring is unchanged.
- The `arca-identity` port, impls, and ciphertext format from
  ADR-0002 are untouched — this amends the *selection policy*,
  not the port.

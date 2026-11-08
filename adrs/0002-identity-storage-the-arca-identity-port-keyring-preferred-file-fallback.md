+++
id = "ADR-0002"
title = "identity storage — the arca-identity port (keyring-preferred, file fallback)"
status = "done"
created = "2026-11-02"
owner = "devin"
priority = "high"
labels = ["identity", "security"]
superseded_by = "ADR-0004"
+++

## Context

P-002 closed the largest gap in the v1 threat model:
`~/.config/arca/identity.txt` is the master key for every vault
impl — raw age key material at `0600`, like an unencrypted SSH
key. Host-read access to that file means every secret in every
vault. At the same time, headless and CI environments have no
session secret store at all, so the file cannot simply go away.

## Decision

- **Identity is a port.** `arca-identity` defines `IdentitySource`
  — `load`, `store`, `kind`, `describe` — speaking parsed
  `x25519::Identity`s (ADR-0001 already fixed crypto = age), plus
  `ensure()` for load-or-generate bootstrap. `Ok(None)` from
  `load` means "this source holds nothing" and is the
  fall-through signal; unreadable or corrupt material is always
  `Err`, never `None`, so a good identity can't be silently
  clobbered.
- **Two impls at merge.** `arca-identity-file` is today's keyfile
  behavior extracted unchanged (`0600`, `#`-comment format,
  `ARCA_IDENTITY_FILE` → XDG → `~/.config` resolution) — it stays
  the default forever for headless/CI. `arca-identity-keyring`
  stores the `AGE-SECRET-KEY-…` string in the platform session
  store via `keyring-core`'s synchronous `CredentialStore` model:
  secret-service over libdbus on Linux/*nix (vendored — no system
  -dev packages), the login keychain on macOS, Credential Manager
  on Windows. No async runtime, per the workspace rules.
- **Store construction is the availability probe.**
  `KeyringIdentity::platform()` returns `IdentityError::Unavailable`
  when no session store is reachable (headless, no dbus session) —
  callers fall back to the file with a one-line stderr note, never
  a crash. A reachable-but-failing store degrades the same way.
- **Resolution order** (in `arca-ops`, applied at every vault
  open): an explicit file override — `ARCA_IDENTITY_FILE`, then
  `[vaults.*].identity` — pins file mode outright; else the
  session keyring wins when it holds an `arca/age-identity` entry;
  else the default keyfile.
- **`arca init` prefers the keyring** when a session store is
  reachable. An existing default-path keyfile is *imported* into
  the keyring, not orphaned — the ciphertext it protects must keep
  decrypting — and left on disk as a backup the human deletes when
  ready. `--with-file` forces the keyfile path.
- **Values stay out of the port's text.** `store()`/`load()` carry
  `x25519::Identity`, whose `SecretString` form is the only
  serialization; front-ends print locators (`describe()`), never
  key material.

## Consequences

On a desktop session the master key no longer sits as a bare file
— it unlocks with login and no `0600` file exists unless the user
migrates (the migration *keeps* the file as backup). Headless and
CI see zero behavior change: `ARCA_IDENTITY_FILE` or
`--with-file` pins the file path and no keyring probe runs. The
passphrase-wrapped-identity arm (P-002's "later") and
TPM/hardware sealing are new impls of the same port, not new
ports. A reachable but *locked* or dead store degrades to the
file path — arca degrades, it does not crash.

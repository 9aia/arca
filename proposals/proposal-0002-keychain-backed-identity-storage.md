+++
id = "P-002"
title = "keychain-backed identity storage"
status = "open"
created = "2026-11-02"
priority = "high"
labels = ["identity", "security"]
+++

Status: **proposal**. Closes the single largest gap in the v1
threat model: `identity.txt` is an unencrypted master key at
0600 — compromise unlocks every vault impl.

## Problem

The age keypair at `~/.config/arca/identity.txt` is raw key
material, like an unencrypted SSH key. Host-read access to that
file = all secrets, all vaults. Threat model currently says
"protect it like a private SSH key" — we can do better.

## Proposal

- New port `arca-identity` (where the identity comes from) with
  impls: `arca-identity-file` (today's, stays default for
  headless/CI via `ARCA_IDENTITY_FILE`) and
  `arca-identity-keyring` — key stored in the platform secret
  store (Linux secret-service, macOS Keychain, Windows DPAPI)
  via the `keyring` crate. Unlocks with the login session, no
  prompt.
- `arca init` prefers keyring when a session secret store is
  available; `--with-file` keeps file mode. Migration: file
  detected → offer one-command import into keyring.
- Later arm (same port): passphrase-wrapped identity file for
  portable identities.

## Non-goals

- TPM/hardware sealing — later ADR.
- No removal of the file path — CI and headless need it.

## Effort

Medium. New port + crate, init/migration UX, ADR-#### to ratify.

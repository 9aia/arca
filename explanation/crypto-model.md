# Crypto model

One crypto choice everywhere, made once in ADR-0001: **age**.
Every vault impl stores age ciphertext; the same identity
unlocks the local file vault and the remote `cf` vault alike.

## The pieces

- **Identity** — one X25519 keypair, held by an `arca-identity`
  impl (ADR-0002): the **session keyring** when one is reachable
  (secret-service, macOS Keychain, Credential Manager), else the
  `~/.config/arca/identity.txt` keyfile (mode `0600`). An explicit
  file — `ARCA_IDENTITY_FILE` or `[vaults.*].identity` — pins the
  keyfile outright; that is the headless/CI path. `arca init`
  bootstraps whichever backend applies and prints the public
  `age1…` recipient.
- **Recipients** — `[vaults.*].recipients` encrypts to extra
  public keys beyond the identity's own — a team recipient list
  without sharing the private key.
- **Ciphertext at rest** — the local vault is a single
  age-encrypted file (`vault.age`); the `cf` vault stores
  per-secret age ciphertext in D1. Neither store — local disk,
  remote worker — holds plaintext or key material.
- **Seal/open at the edge** — values are plaintext only inside
  the arca process, and only for as long as the op needs them.
  `Secret` is a `Zeroizing` newtype: memory is scrubbed on drop,
  and nothing but `.expose()` reads it.

## Why age (and not KMS / libsodium / SSH keys)

- **One identity, every backend** — a remote vault that stores
  client-side ciphertext needs an offline asymmetric primitive;
  age gives recipient-style encryption with no service to trust.
- **Dependency posture** — the `age` crate (rage's Rust impl) is
  pulled with `default-features = false`: binary ciphertext, sync
  API, no async/armor/plugin machinery — the binary stays light
  per the workspace rules.
- **Interop for free** — the vault file decrypts with stock
  `rage`/`age` tooling, so a human is never locked into the CLI
  for disaster recovery.

## What is deliberately not here

- **No KMS port yet** — managed-key backends (AWS KMS/Secrets
  Manager) were explored in the `arca-vault-aws` spike to prove
  the `Vault` port generalizes to non-age stores; a KMS-backed
  *crypto* port is a later ADR, not v1.
- **No passphrase-wrapped identity file** — when the keyring is
  unavailable the keyfile is still raw age key material at
  `0600`, like an unencrypted SSH key. A passphrase-wrapped arm
  of the identity port is a later impl, not a new port.
- **No signing / integrity metadata** beyond what age's AEAD
  gives — tampered ciphertext fails decryption loudly, which is
  the correct failure.

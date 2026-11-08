# Crypto model

One crypto choice everywhere, made once in ADR-0001: **age**.
Every vault impl stores age ciphertext; the same identity
unlocks the local file vault and the remote `cf` vault alike.

## The pieces

- **Identity** — one X25519 keypair, held by an `cuora-identity`
  impl (ADR-0002): the **session keyring** (secret-service,
  macOS Keychain, Credential Manager) by default — fail closed —
  or the `~/.config/cuora/identity.txt` keyfile (mode `0600`)
  under `auto`/`file` modes or an explicit pin. An explicit
  file — `CUORA_IDENTITY_FILE` or `[vaults.*].identity` — pins the
  keyfile outright; that is the headless/CI path. `[identity]
  backend` (or `CUORA_IDENTITY_BACKEND`) selects the mode:
  `keyring` (default) fails closed — no reachable store or
  stored identity is an error, the keyfile is never read;
  `auto` is the fallback described above — convenient on
  headless/CI, but a *silent* downgrade if a desktop session
  loses its store, so it's opt-in; `file` skips the store
  probe entirely. `cuora init`
  bootstraps whichever backend applies and prints the public
  `age1…` recipient. On a reachable-but-empty Secret Service (fresh
  daemon, no collections yet) the keyring impl claims the `default`
  collection alias on first use — `dbus-secret-service-keyring-store`'s
  create path can't bootstrap one itself (I-006).
- **Recipients** — `[vaults.*].recipients` encrypts to extra
  public keys beyond the identity's own — a team recipient list
  without sharing the private key.
- **Ciphertext at rest** — the local vault is a single
  age-encrypted file (`vault.age`); the `cf` vault stores
  per-secret age ciphertext in D1. Neither store — local disk,
  remote worker — holds plaintext or key material.
- **Seal/open at the edge** — values are plaintext only inside
  the cuora process, and only for as long as the op needs them.
  `Secret` is a `Zeroizing` newtype: memory is scrubbed on drop,
  and nothing but `.expose()` reads it.
- **HMAC alongside age** — the `cf` wire uses HMAC-SHA256 twice
  (P-003, P-004), keyed but unkeyed-payload, complementary to the
  age envelope: request *signing* so tokens never travel, and
  `ref_key` *derivation* so storage keys are unlinkable. Neither
  is a second encryption primitive — it's authentication and
  deterministic naming.

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

## The cf wire (`cuora-vault-cf/2`)

- **Request signing** — `Authorization: CuoraHmac &lt;token_id&gt;, &lt;ts&gt;,
  &lt;sig&gt;` with `sig = hex HMAC-SHA256(token, method+path+ts+
  sha256hex(body))`. The token is the HMAC key and never crosses
  the wire; a ±60s timestamp window bounds replay. Worker-side,
  `CUORA_VAULT_TOKENS` maps `token_id → {env → {read, write}}`
  scopes — per-env credentials instead of one static bearer
  (which stays accepted during migration).
- **Private refs** — with `[vaults.*] private_refs = true`, the
  storage key is `ref_key = hex HMAC-SHA256(k, env + "/" + ref)`
  where `k = HMAC-SHA256(identity, "cuora-cf/private-refs/v1")`.
  Derivation is deterministic (lookup is a plain keyed read) and
  identity-bound (rotating the identity re-keys everything, via
  `cuora migrate --private-refs`). The real ref travels inside a
  `name` blob — age ciphertext — so `list` maps keys back while
  the store sees only hex.
- **Constant-time compares** — every token/signature comparison
  on the worker is a byte-wise XOR-fold over the longer input
  with length folded in; no early exit, no prefix oracle.

## What is deliberately not here

- **No KMS port yet** — managed-key backends (AWS KMS/Secrets
  Manager) were explored in the `cuora-vault-aws` spike to prove
  the `Vault` port generalizes to non-age stores; a KMS-backed
  *crypto* port is a later ADR, not v1.
- **No passphrase-wrapped identity file** — when the keyring is
  unavailable the keyfile is still raw age key material at
  `0600`, like an unencrypted SSH key. A passphrase-wrapped arm
  of the identity port is a later impl, not a new port.
- **No signing / integrity metadata** beyond what age's AEAD
  gives — tampered ciphertext fails decryption loudly, which is
  the correct failure.

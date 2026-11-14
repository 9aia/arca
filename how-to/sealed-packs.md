# Ship secrets as a sealed pack

A **sealed pack** is a tamias package whose payload is age
ciphertext only (tamias ADR-0004). Tamias carries, digests, verifies,
and signs the bytes opaquely — it has no decrypt path. Opening is
cuora's job, consumer-side, with the recipient private key
(`~/.config/cuora/identity.txt`, mode 0600).

Use this when a secret should distribute like code — versioned,
registry-published, installed on every machine — instead of living
in one vault. `cuora://<pack>/<secret>` refs then resolve through
the installed pack everywhere vault refs do.

Sealed packs are the right home for **env-agnostic, read-only**
secrets — they carry no env axis, so one sealed copy serves every
env. Secrets that need vault writes (`rotate`/`deploy`) or are
genuinely per-env stay in the vault; see the
[env-agnostic convention](add-a-secret.md#env-agnostic-secrets--pick-a-home-first).

## Author: seal a payload

Plaintext files in, `sealed/` ciphertext out:

```sh
tamias-driver-cuora seal \
  --in ./secrets-plain \
  --out ./secrets-9aia \
  --recipients recipients.txt \
  --manifest ./secrets-9aia/tamias.yaml
```

- `--in <dir>` seals every file (logical name = file stem); or pass
  `NAME=PATH` pairs as operands for explicit names.
- Recipients come from `--recipient age1…` (repeatable),
  `--recipients <file>` (one `age1…` per line), or `--keyfile`
  (adds the local identity's public key).
- The `sealed:` manifest block prints on stdout. `--manifest
  <tamias.yaml>` merges it into the pack manifest in place —
  comments and unrelated keys are preserved (surgical text merge,
  validated as a manifest before it lands). Without it, paste the
  block into `tamias.yaml` by hand.

The result: `sealed/recipients` + `sealed/<name>.age` ciphertext
and a `sealed:` block mapping logical names to files. Never commit
plaintext — tamias's publish/install ciphertext gate rejects
non-`.age` payloads in a sealed pack.

## Publish and install

```sh
tamias pack ./secrets-9aia          # or: tamias publish .
tamias install secrets-9aia@1.0.0
```

`tamias-driver-cuora install|deploy|uninstall` are dispatched for
`type: secrets` packs and are deliberate no-ops — ciphertext
installs verbatim.

## Consume

Once installed, refs resolve with **vault-first** ordering: a vault
hit always wins; packs answer only on a vault miss.

```toml
# cuora.toml
[secrets]
TAMIAS_REGISTRY_TOKEN = "cuora://secrets-9aia/registry-token"
```

```sh
cuora run -s TOK=cuora://secrets-9aia/registry-token -- cmd
cuora env -s TOK=cuora://secrets-9aia/registry-token
```

Or the explicit driver path:

```sh
tamias-driver-cuora open secrets-9aia registry-token   # → stdout
tamias-driver-cuora materialize secrets-9aia --out <dir>
tamias-driver-cuora recipients secrets-9aia            # public keys
tamias-driver-cuora packs                              # installed packs
```

Every open/materialize emits a `cuora` audit event (`op: get`,
`env: sealed`) — the materialization trail covers pack opens too.

## Pin a version when several are installed

Refs address packs by name, and the ref grammar mirrors tamias's
`name@version` label spelling for pins:

```sh
cuora get cuora://secrets-9aia@1.0.0/registry-token
```

An unpinned `cuora://secrets-9aia/registry-token` with two versions
installed is an **ambiguity error** — never a silent pick; the
message lists the candidate labels and the pin syntax. Scoped
packs (`@scope/name`) aren't addressable in refs — the two-segment
shape has no room for the scope; publish unscoped or pin by the
bare name.

## Migrate a vault ref onto a pack

When a secret moves from the vault into a sealed pack, consumers
keep their old refs working through `[aliases]` — a config-level
remap applied during resolution, before vault and pack lookup:

```toml
# cuora.toml
[aliases]
"tamias" = "cuora://secrets-9aia"   # namespace prefix: every
                                    # cuora://tamias/* ref lands on
                                    # the pack's keyspace
```

Keys are bare ref paths (no `cuora://`); the longest key that
equals the path or prefixes it at a `/` boundary wins, the target
path replaces the matched segments, and any tail is appended —
`cuora://tamias/registry-token` resolves as
`cuora://secrets-9aia/registry-token`. Aliases chain transitively;
self-aliases and cycles fail config load. Env pins strip first, so
`cuora://prod/tamias/x` still resolves under `prod`. Materializations
through an alias audit normally, with `alias_from` recording the
requested path. See [config](../reference/config.md).

## Rotate a sealed secret

Sealed packs are immutable ciphertext — rotation means re-sealing:

1. Re-run `seal` with the full recipient set (adding a member later
   can't retroactively open older versions — always seal to
   *everyone* who should read it).
2. Bump `version` in `tamias.yaml`, republish, and have consumers
   `tamias install secrets-9aia@<new>` — or update their pinned
   refs.

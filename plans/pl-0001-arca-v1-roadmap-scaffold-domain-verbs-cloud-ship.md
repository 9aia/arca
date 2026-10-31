+++
id = "PL-001"
title = "arca v1 roadmap — scaffold → domain → verbs → cloud → ship"
status = "open"
created = "2026-10-31"
priority = "high"
+++

Roadmap per P-001. Hard constraints from the owner:

- **Local vault is the default, always** — `arca-vault-age` is v1
  *and* the fallback; CF is just another impl, never the default.
- **`arca-vault-aws` is a spike** — lives on a local throwaway
  branch to prove the port generalizes; never merges to main,
  deleted once proven.
- **Publish = `cargo install --git https://github.com/9aia/arca`** —
  no crates.io publish, no git tags.

## Phases

- [x] **P1 — scaffold**: myco anatomy copy → rename → compile →
  commit → `myco init` → P-001.
- [x] **P2 — domain core**: `arca-core` (arca:// ref, Secret
  newtype, env axis, actor/policy) · `arca-vault` port ·
  `arca-vault-age` impl · `arca-testkit` conformance ·
  re-map/drop vestigial crates (arca-git, arca-index*,
  arca-store*).
- [x] **P3 — verbs + surfaces**: `put/get/list/rm/rotate` (stdin
  only) · `run`/`env` injection · `arca.toml` env→vault
  association + ref→env-name map · audit ledger ·
  `--json`/`manifest`/`schema` · `.agents/skills/arca`.
- [x] **P4 — cloud**: `arca-vault-cf` (Worker + D1, client-side
  age ciphertext) · `arca-deploy` port + `arca-deploy-cf`
  (push-only targets) · *spike*: `arca-vault-aws` on a local
  branch → delete.
  Spike verdict (SPIKE.md, branch deleted after review): the
  `Vault` port fits a hosted secrets manager **unchanged** — a
  Secrets Manager-shaped transport carries the full contract.
  Stores without per-record fields need a client-side JSON
  envelope (`ct`+version+timestamps inside SecretString); age
  stays fixed — **no crypto port needed** (KMS would be a new
  ADR, not a blocker). Still needed for a real impl: SigV4-signed
  blocking transport, pagination, delete-recovery semantics,
  `kind = "aws"` config arm.
- [ ] **P5 — polish + ship**: `arca ui` TUI · docs site →
  `9aia.com/arca` · mimus key-resolution migration.
  Landed: TUI shipped (audited two-step reveal),
  `gen-cli-docs`, docs site rebuilt on the `docs` orphan
  worktree (builds green), `cargo install --git` verified.
  Open/external: docs push → Pages deploy (9aia.com/arca),
  real CF vault deploy + deploy-sink test (credentials),
  mimus migration (mimus repo), `spike/aws-vault` branch
  deletion.

## Later ADRs

- `arca serve` — ndjson/JSON-RPC IPC surface for peregrin
- envelope refs (inline ciphertext vs store refs)
- KMS-backed crypto port (see AWS spike)

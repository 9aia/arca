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
- [ ] **P2 — domain core**: `arca-core` (arca:// ref, Secret
  newtype, env axis, actor/policy) · `arca-vault` port ·
  `arca-vault-age` impl · `arca-testkit` conformance ·
  re-map/drop vestigial crates (arca-git, arca-index*,
  arca-store*).
- [ ] **P3 — verbs + surfaces**: `put/get/list/rm/rotate` (stdin
  only) · `run`/`env` injection · `arca.toml` env→vault
  association + ref→env-name map · audit ledger ·
  `--json`/`manifest`/`schema` · `.agents/skills/arca`.
- [ ] **P4 — cloud**: `arca-vault-cf` (Worker + D1, client-side
  age ciphertext) · `arca-deploy` port + `arca-deploy-cf`
  (push-only targets) · *spike*: `arca-vault-aws` on a local
  branch → delete.
- [ ] **P5 — polish + ship**: `arca ui` TUI · docs site →
  `9aia.com/arca` · mimus key-resolution migration.

## Later ADRs

- `arca serve` — ndjson/JSON-RPC IPC surface for peregrin
- envelope refs (inline ciphertext vs store refs)
- KMS-backed crypto port (see AWS spike)

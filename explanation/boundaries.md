---
url: /cuora/explanation/boundaries.md
---
# Boundaries

cuora is one organism in a stack — it owns **secret
materialization** and nothing else. If a feature needs another
kind of memory or authority, it belongs to a neighbor.

| neighbor | owns | how it meets cuora |
| --- | --- | --- |
| **myco** | work memory — tasks, issues, proposals, plans, ADRs, handoffs | this repo's `project/` ledger is myco's; cuora has no PM surface |
| **sturn** | ephemeral coordination — live claims/ops/topics | not secrets: sturn state is transient, cuora state is sealed |
| **kuda** | code memory — symbol/index knowledge | cuora refs name *secrets*, not code entities |
| **peregrin** | orchestration — campaigns, multi-actor arbitration, policy | campaigns consume secrets via `cuora run`; audit aggregation + real policy are peregrin's (`CUORA_ACTOR` is self-claimed inside cuora); `cuora serve` — the ndjson IPC wire + `[serve.pdp]` exec consult (ADR-0003) — is its embedding point |
| **bupha / ci-gate** | CI gate | bupha is the CI repo — the org gate is the peregrin `ci-gate` campaign, mid-migration back to bupha (`type: bupha` bundles); it asserted no plaintext secrets land in pipelines, `cuora check` in pipeline steps |
| **mimus** | critic | secrets resolve env → cuora → legacy (keyring, `~/.config/mimus/env`) via `mimus-cuora` shelling `cuora`; vaults are canonical, writes land in them |
| **sepia** | session lifecycle | session wrappers can capture cuora's stderr audit stream |

## What that means in practice

* **A hosted vault service is in scope** — `cuora-vault-cf` is a
  store impl behind the same port, ciphertext-only, self-hosted.
* **A daemon is out of scope** — persistent coordination,
  multi-machine views, arbitration between actors → peregrin.
  cuora stays a sync, embeddable binary: env vars are the
  interface, no SDK.
* **Policy is out of scope** — `[guardrails]` are advisory
  accident prevention. Enforcement, identity, and unforgeable
  attribution are orchestration concerns.
* **The audit wire stops at stderr** — emitting ndjson is cuora's;
  collecting, storing, and alerting on it is peregrin's
  (ADR-0001 deliberately removed the local `audit.jsonl`).

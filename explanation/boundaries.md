---
url: /arca/explanation/boundaries.md
---
# Boundaries

arca is one organism in a stack — it owns **secret
materialization** and nothing else. If a feature needs another
kind of memory or authority, it belongs to a neighbor.

| neighbor | owns | how it meets arca |
| --- | --- | --- |
| **myco** | work memory — tasks, issues, proposals, plans, ADRs, handoffs | this repo's `project/` ledger is myco's; arca has no PM surface |
| **sturn** | ephemeral coordination — live claims/ops/topics | not secrets: sturn state is transient, arca state is sealed |
| **kuda** | code memory — symbol/index knowledge | arca refs name *secrets*, not code entities |
| **peregrin** | orchestration — campaigns, multi-actor arbitration, policy | campaigns consume secrets via `arca run`; audit aggregation + real policy are peregrin's (`ARCA_ACTOR` is self-claimed inside arca); `arca serve` — the ndjson IPC wire + `[serve.pdp]` exec consult (ADR-0003) — is its embedding point |
| **bupha** | CI gate | asserts no plaintext secrets land in pipelines; `arca check` in pipeline steps |
| **mimus** | critic | secrets resolve env → arca → legacy (keyring, `~/.config/mimus/env`) via `mimus-arca` shelling `arca`; vaults are canonical, writes land in them |
| **sepia** | session lifecycle | session wrappers can capture arca's stderr audit stream |

## What that means in practice

* **A hosted vault service is in scope** — `arca-vault-cf` is a
  store impl behind the same port, ciphertext-only, self-hosted.
* **A daemon is out of scope** — persistent coordination,
  multi-machine views, arbitration between actors → peregrin.
  arca stays a sync, embeddable binary: env vars are the
  interface, no SDK.
* **Policy is out of scope** — `[guardrails]` are advisory
  accident prevention. Enforcement, identity, and unforgeable
  attribution are orchestration concerns.
* **The audit wire stops at stderr** — emitting ndjson is arca's;
  collecting, storing, and alerting on it is peregrin's
  (ADR-0001 deliberately removed the local `audit.jsonl`).

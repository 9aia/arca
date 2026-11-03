---
url: /arca/explanation/threat-model.md
---
# Threat model

What arca protects against, honestly — and what it explicitly
does not.

## Protects against

* **Plaintext in the repo** — code and config carry `arca://`
  refs, never values. There is nothing to leak in a commit, a
  diff, a paste, or a prompt context.
* **Plaintext in shell history / process listings** — values
  never enter via CLI arguments (args land in `~/.bash_history`
  and `/proc/*/cmdline`). `put`/`rotate` read stdin or a hidden
  `/dev/tty` prompt.
* **Plaintext in agent context** — `arca run` injects values into
  the child's environment; the agent that spawned it sees the
  child's *output*, never the variable. `[secrets]` bounds the
  injectable surface so a `run` can't dump the whole vault.
* **Accidental printing** — `Secret` has no `Display`/`Debug` of
  the value (`.expose()` only), `list` is names + metadata only,
  and the TUI's posture is browse-never-spill (reveal is a
  deliberate two-step, audited).
* **Ciphertext at rest, everywhere** — every vault impl stores
  age ciphertext. The local file is unreadable without the
  identity; the `cf` remote store holds ciphertext over the wire
  *and* in D1 — a compromised worker yields blobs, not values.
  With `[vaults.*] private_refs = true` it can't even see *which*
  secrets exist: storage keys are HMAC-derived hex and the real
  ref travels in an age-encrypted `name` blob.
* **Token leakage on the cf wire** — scoped tokens
  (`ARCA_VAULT_TOKENS`) bound a leaked credential to its
  `{env, read, write}` scope instead of every env forever, and
  ArcaHmac request signing means the token never travels at all
  (a ±60s window bounds replay). All auth compares are
  constant-time.
* **Silent misuse** — every materialization, denial, and error
  emits an ndjson audit event on stderr, so capture is a wrapper
  away. `check` fails closed on a vault that won't decrypt, and
  on a mixed-mode cf vault (private config + plaintext rows).

## Does NOT protect against

* **A determined local actor** — `ARCA_ACTOR` is self-claimed
  (attribution, not identity) and `[guardrails]` are advisory:
  anyone who can edit `arca.toml` or run the binary can bypass
  them. They prevent *accidents*, not adversaries. Real
  multi-actor policy and enforcement belong to **peregrin**.
* **The identity's host protection** — the master key lives in
  the session keyring when one exists (ADR-0002), which protects
  it about as well as your login session. On headless/CI it is
  still `identity.txt` (0600), raw key material — protect it like
  a private SSH key. Either way, UID-level compromise reaches it:
  the keyring raises the bar, it is not a TPM.
* **The child process** — once `run` injects env vars, the child
  owns them (`/proc/<pid>/environ`, core dumps, the child logging
  its own env). arca's guarantee ends at `exec`.
* **Deploy sinks** — `arca deploy` pushes plaintext into the sink
  by design (CF Secrets Store is write-only); a sink compromise
  is outside arca's boundary — it crosses exactly once, at push.
* **Access patterns** — `private_refs` hides *which* secrets
  exist, not *when* they're touched: request timing and row
  counts are still observable to the store. Oblivious access is
  out of scope.
* **Host compromise in general** — an attacker who can read your
  memory, ptrace the broker, or impersonate your UID already won;
  arca raises the bar for *accidental* and *casual* leaks, not
  root.

## In one line

arca makes the *default* posture "values never appear in repos,
prompts, args, logs, or agent context" — and records every time
one still crosses.

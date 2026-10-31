# Threat model

What arca protects against, honestly — and what it explicitly
does not.

## Protects against

- **Plaintext in the repo** — code and config carry `arca://`
  refs, never values. There is nothing to leak in a commit, a
  diff, a paste, or a prompt context.
- **Plaintext in shell history / process listings** — values
  never enter via CLI arguments (args land in `~/.bash_history`
  and `/proc/*/cmdline`). `put`/`rotate` read stdin or a hidden
  `/dev/tty` prompt.
- **Plaintext in agent context** — `arca run` injects values into
  the child's environment; the agent that spawned it sees the
  child's *output*, never the variable. `[secrets]` bounds the
  injectable surface so a `run` can't dump the whole vault.
- **Accidental printing** — `Secret` has no `Display`/`Debug` of
  the value (`.expose()` only), `list` is names + metadata only,
  and the TUI's posture is browse-never-spill (reveal is a
  deliberate two-step, audited).
- **Ciphertext at rest, everywhere** — every vault impl stores
  age ciphertext. The local file is unreadable without the
  identity; the `cf` remote store holds ciphertext over the wire
  *and* in D1 — a compromised worker yields blobs and ref names,
  not values.
- **Silent misuse** — every materialization, denial, and error
  emits an ndjson audit event on stderr, so capture is a wrapper
  away. `check` fails closed on a vault that won't decrypt.

## Does NOT protect against

- **A determined local actor** — `ARCA_ACTOR` is self-claimed
  (attribution, not identity) and `[guardrails]` are advisory:
  anyone who can edit `arca.toml` or run the binary can bypass
  them. They prevent *accidents*, not adversaries. Real
  multi-actor policy and enforcement belong to **peregrin**.
- **The identity file itself** — `identity.txt` (0600) is the
  master key for every vault impl; arca doesn't wrap it in a
  passphrase or keychain (yet — a later ADR). Protect it like a
  private SSH key.
- **The child process** — once `run` injects env vars, the child
  owns them (`/proc/<pid>/environ`, core dumps, the child logging
  its own env). arca's guarantee ends at `exec`.
- **Deploy sinks** — `arca deploy` pushes plaintext into the sink
  by design (CF Secrets Store is write-only); a sink compromise
  is outside arca's boundary — it crosses exactly once, at push.
- **Host compromise in general** — an attacker who can read your
  memory, ptrace the broker, or impersonate your UID already won;
  arca raises the bar for *accidental* and *casual* leaks, not
  root.

## In one line

arca makes the *default* posture "values never appear in repos,
prompts, args, logs, or agent context" — and records every time
one still crosses.

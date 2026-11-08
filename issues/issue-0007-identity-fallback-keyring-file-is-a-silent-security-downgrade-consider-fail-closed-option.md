+++
id = "I-007"
title = "identity fallback keyring→file is a silent security downgrade — consider fail-closed option"
status = "done"
created = "2026-11-08"
owner = "devin"
priority = "medium"
labels = ["identity", "security"]
+++

Today the identity resolves keyring → file automatically (ADR-0002). When the session secret store is unreachable — daemon not running, locked, headless — ops silently proceed against \`~/.config/arca/identity.txt\` (raw age key material, 0600). There is a one-line stderr note, but the op succeeds: a user who believes their identity is keyring-protected gets file-mode protection without noticing.

Threat-model already admits UID-level compromise reaches either backend, so the marginal risk is *unnoticed downgrade*, not a new attack class — but for a secrets broker, 'protected' vs 'protected-ish' should be a conscious choice, not a default.

Proposal: add a fail-closed mode — e.g. \`ARCA_IDENTITY_BACKEND=keyring\` env or \`[identity] backend = \"keyring\"|\"file\"|\"auto\"\` in arca.toml — where \`keyring\` errors instead of falling back (\`IdentityError::Unavailable\` already exists), and/or make the fallback note louder (once per session is fine, it already is). \`auto\` stays default for CI ergonomics.

Also worth deciding: should \`arca init\`/first-run print which backend won more prominently, and should \`arca check\` report the active identity backend as a finding when it's file-on-a-desktop-session?

## Comments

- 2026-11-08 Implemented in 918f9b9 (main) + dea1b1e (docs). [identity] backend in arca.toml and ARCA_IDENTITY_BACKEND env: auto (default — unchanged keyring→file fallback), keyring (fail closed: strict_keyring probe + keyring_required load; unreachable store or absent identity is an error, keyfile never read), file (skips the store probe). Explicit ARCA_IDENTITY_FILE / [vaults.*].identity pins still win. Manifest + env-vars/config/crypto-model docs updated; schema picks it up via schemars. 310 tests + worker green; also hardened keyring-assuming tests against real session stores (dead DBus socket, ARCA_IDENTITY_BACKEND=file in the TUI seeded-keyfile test).

- 2026-11-08 Follow-up in 389586a: fail-closed flipped to the DEFAULT — absent [identity] backend / ARCA_IDENTITY_BACKEND now means keyring; auto (old fallback) and file are explicit opt-outs. Headless/CI must set backend=auto or file. Also fixed a latent pdp flake (SIGPIPE vs exceeds ordering).

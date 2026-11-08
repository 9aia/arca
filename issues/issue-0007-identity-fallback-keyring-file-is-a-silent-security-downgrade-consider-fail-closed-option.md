+++
id = "I-007"
title = "identity fallback keyring→file is a silent security downgrade — consider fail-closed option"
status = "open"
created = "2026-11-08"
owner = "devin"
priority = "medium"
labels = ["identity", "security"]
+++

Today the identity resolves keyring → file automatically (ADR-0002). When the session secret store is unreachable — daemon not running, locked, headless — ops silently proceed against \`~/.config/arca/identity.txt\` (raw age key material, 0600). There is a one-line stderr note, but the op succeeds: a user who believes their identity is keyring-protected gets file-mode protection without noticing.

Threat-model already admits UID-level compromise reaches either backend, so the marginal risk is *unnoticed downgrade*, not a new attack class — but for a secrets broker, 'protected' vs 'protected-ish' should be a conscious choice, not a default.

Proposal: add a fail-closed mode — e.g. \`ARCA_IDENTITY_BACKEND=keyring\` env or \`[identity] backend = \"keyring\"|\"file\"|\"auto\"\` in arca.toml — where \`keyring\` errors instead of falling back (\`IdentityError::Unavailable\` already exists), and/or make the fallback note louder (once per session is fine, it already is). \`auto\` stays default for CI ergonomics.

Also worth deciding: should \`arca init\`/first-run print which backend won more prominently, and should \`arca check\` report the active identity backend as a finding when it's file-on-a-desktop-session?

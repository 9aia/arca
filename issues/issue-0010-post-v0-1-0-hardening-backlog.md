+++
id = "I-010"
title = "post-v0.1.0 hardening backlog"
status = "done"
created = "2026-11-08"
priority = "med"
+++

# post-v0.1.0 hardening backlog

Status: **open** · Created: 2026-11-08

## Description

What is wrong and steps to reproduce.

## Checklist

- [ ] Action item

## Resolution

crates.io publish verified complete 2026-11-11: all publishable crates live (cuora, cuora-core, config, config-toml, deploy, deploy-cf, identity, identity-file, identity-keyring, notice, ops, serve, shell, testkit, vault, vault-age, vault-cached, vault-cf). cuora-driver intentionally unpublished — ships as tamias pack payload. cuora-vault-aws never published — deleted spike per PL-001.

## Comments

- 2026-11-08 Final pre-release audit done (subagent): verdict tag-ready, no critical/high. Fixed 3 lows pre-tag: empty CUORA_* shadowing, trust-root warning path, manifest legacy aliases; env_var empty=unset semantics unified + tests. 329 tests + 9 worker green. Tagged v0.1.0, pushed; cargo install --git --tag v0.1.0 verified.

- 2026-11-08 crates.io publish in progress: 6/18 live (core, config, config-toml, deploy, identity, notice). crates.io rate-limits new crates ~1/5min — background retry loop publishing remaining 12 in dep order. cuora-testkit flipped to publishable (dev-dep of 7 crates).

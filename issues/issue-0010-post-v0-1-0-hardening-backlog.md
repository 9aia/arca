+++
id = "I-010"
title = "post-v0.1.0 hardening backlog"
status = "open"
created = "2026-11-08"
priority = "medium"
+++

# post-v0.1.0 hardening backlog

Status: **open** · Created: 2026-11-08

## Description

What is wrong and steps to reproduce.

## Checklist

- [ ] Action item

## Resolution

## Comments

- 2026-11-08 Final pre-release audit done (subagent): verdict tag-ready, no critical/high. Fixed 3 lows pre-tag: empty CUORA_* shadowing, trust-root warning path, manifest legacy aliases; env_var empty=unset semantics unified + tests. 329 tests + 9 worker green. Tagged v0.1.0, pushed; cargo install --git --tag v0.1.0 verified.

- 2026-11-08 crates.io publish in progress: 6/18 live (core, config, config-toml, deploy, identity, notice). crates.io rate-limits new crates ~1/5min — background retry loop publishing remaining 12 in dep order. cuora-testkit flipped to publishable (dev-dep of 7 crates).

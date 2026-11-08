+++
id = "I-009"
title = "cuora vault workers deployed — secrets re-put + client URL flip pending"
status = "open"
created = "2026-11-08"
priority = "medium"
+++

# cuora vault workers deployed — secrets re-put + client URL flip pending

Status: **open** · Created: 2026-11-08

## Description

What is wrong and steps to reproduce.

## Checklist

- [ ] Action item

## Resolution

## Comments

- 2026-11-08 Deployed: worker 'cuora' live on both accounts — cuora.luisfuturist.com (Luis Emidio acct, D1 'arca' 4b0fbf31) and cuora.9aia.com (Gaia acct, D1 'arca-vault' 628fc6ce). Both verified 401 fail-closed. Old 'arca' workers still running with secrets — zero downtime. BLOCKER: new workers have empty secrets; user must run: cd crates/cuora-vault-cf/worker && bunx wrangler secret put CUORA_VAULT_TOKENS -c wrangler.toml && bunx wrangler secret put CUORA_VAULT_TOKENS -c wrangler.9aia.toml (value = same scoped-token JSON map as old ARCA_VAULT_TOKENS). After verify: flip ~/.config/arca/arca.toml → ~/.config/cuora/cuora.toml url=https://cuora.luisfuturist.com token_env=CUORA_CF_TOKEN cuora:// refs; thal cuora.toml already points at cuora.luisfuturist.com. ktbhealth worker (arca.ktbhealth.workers.dev) is on a different CF account — cannot redeploy from here; old worker only speaks ArcaHmac/Bearer so scoped tokens break until they redeploy; bearer token_env unaffected. D1 databases keep names 'arca'/'arca-vault' (ids reused, no data migration).

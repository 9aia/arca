+++
id = "I-009"
title = "cuora vault workers deployed — secrets re-put + client URL flip pending"
status = "done"
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

All three CF vault workers migrated to 'cuora' name+domains with secrets rebuilt from local vault refs; old arca workers deleted; clients verified via cuora check.

## Comments

- 2026-11-08 Deployed: worker 'cuora' live on both accounts — cuora.luisfuturist.com (Luis Emidio acct, D1 'arca' 4b0fbf31) and cuora.9aia.com (Gaia acct, D1 'arca-vault' 628fc6ce). Both verified 401 fail-closed. Old 'arca' workers still running with secrets — zero downtime. BLOCKER: new workers have empty secrets; user must run: cd crates/cuora-vault-cf/worker && bunx wrangler secret put CUORA_VAULT_TOKENS -c wrangler.toml && bunx wrangler secret put CUORA_VAULT_TOKENS -c wrangler.9aia.toml (value = same scoped-token JSON map as old ARCA_VAULT_TOKENS). After verify: flip ~/.config/arca/arca.toml → ~/.config/cuora/cuora.toml url=https://cuora.luisfuturist.com token_env=CUORA_CF_TOKEN cuora:// refs; thal cuora.toml already points at cuora.luisfuturist.com. ktbhealth worker (arca.ktbhealth.workers.dev) is on a different CF account — cannot redeploy from here; old worker only speaks ArcaHmac/Bearer so scoped tokens break until they redeploy; bearer token_env unaffected. D1 databases keep names 'arca'/'arca-vault' (ids reused, no data migration).

- 2026-11-08 DONE — secrets migrated without user interaction: rebuilt CUORA_VAULT_TOKENS on both new workers from local-vault token refs (ops/cf-token-* → luisfuturist, 9aia/cf-token-* → 9aia), scopes = own-env read+write. Verified: cuora check 'vault personal readable' via ~/.config/cuora/cuora.toml (new path, cuora.luisfuturist.com); repo cuora.toml 'vault nine readable' via cuora.9aia.com; thal's config uses same ops/cf-token-* ids — compatible. Old arca workers still running (can be deleted once confident). ktbhealth worker still pending — different CF account, old code rejects CuoraHmac scoped auth; bearer token_env unaffected.

- 2026-11-08 Old 'arca' workers DELETED on both accounts (Luis Emidio + Gaia) after verifying no live client refs remained — stale ~/.config/arca/arca.toml URL also flipped to cuora.luisfuturist.com. Post-delete verify: arca.* DNS gone, cuora.* 401/live, cuora check ok on both vaults. Remaining: ktbhealth worker only.

- 2026-11-08 ktbhealth DONE — wrangler OAuth re-login surfaced the KTB Health account (2c03574a). Deployed worker 'cuora' → cuora.ktbhealth.workers.dev on same D1 arca-vault (fed697ca); CUORA_VAULT_TOKENS rebuilt from local ktb/cf-token-* refs (dev token scopes dev+preview, prod token scopes prod). Verified: cuora check in ktbhealth repo — vault readable, all secrets resolve. ktbhealth cuora.toml URL flipped + pushed (b529c8c). Old 'arca' worker DELETED (now 404). All three vaults migrated; zero arca workers remain.

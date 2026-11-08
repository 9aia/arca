+++
id = "I-008"
title = "rename arca → cuora (full, incl. dependents + archived repos)"
status = "done"
created = "2026-11-08"
priority = "medium"
+++

# rename arca → cuora (full, incl. dependents + archived repos)

Status: **open** · Created: 2026-11-08

## Description

What is wrong and steps to reproduce.

## Checklist

- [ ] Action item

## Resolution

Renamed arca → cuora across main repo, all live dependents, and archived repos (thalapp/thal only). GH repo renamed with redirects; docs at 9aia.com/cuora; legacy ARCA_*/arca.toml/arca:// fallbacks preserved.

## Comments

- 2026-11-08 DONE — full rename shipped. Main repo 9aia/cuora (was 9aia/arca, GH redirect live): 20 crates + binary + cuora:// + CUORA_* + cuora.toml + ~/.config/cuora, all with ARCA_*/arca.toml/arca:// legacy fallbacks. 328 tests + 9 worker green. Dependents pushed: mimus (mimus-cuora), peregrin, tamias, myco, kuda, ktbhealth (partial — package.json edits ride user's WIP), 9aia.github.io (projects/cuora.md; index/about ride WIP), vitepress-theme-9aia, gaia-docus (untracked local). Archived: only thalapp/thal referenced arca — unarchived, updated, pushed, re-archived. Docs site live at 9aia.com/cuora. Local dir renamed, worktrees repaired, cuora 0.1.0 release-installed, stale arca binary removed.

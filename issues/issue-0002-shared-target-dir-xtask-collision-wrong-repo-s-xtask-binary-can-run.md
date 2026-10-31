+++
id = "I-002"
title = "shared target-dir xtask collision — wrong repo's xtask binary can run"
status = "open"
created = "2026-10-31"
priority = "med"
+++

# shared target-dir xtask collision — wrong repo's xtask binary can run

Status: **open** · Created: 2026-10-31

## Description

`~/.cargo/config.toml` sets `target-dir = ~/.cargo/shared-target`
for every cargo workspace on this machine. arca and myco both ship
a workspace package named `xtask`, and cargo's fingerprint does
not distinguish them by repo — whichever repo built `xtask` last
owns `shared-target/debug/xtask`. `cargo xtask install` here ran
myco's xtask, which failed on `--path crates/myco-cli`. The
check/test verbs masked it: both xtasks run identical generic
cargo commands, so results were correct by accident.

Repro: `cargo xtask install` in arca after myco's xtask was built
→ "crates/myco-cli is not a directory".

Workaround used: `touch xtask/src/main.rs && cargo build -p xtask`
forces arca's xtask to own the binary; collision ping-pongs back
whenever the other repo rebuilds.

## Checklist

- [ ] Pick a fix: unique bin names per repo (`arca-xtask`), a
      per-repo `CARGO_TARGET_DIR`, or document the touch-rebuild
      workaround in AGENTS.md

## Resolution

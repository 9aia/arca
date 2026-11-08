+++
id = "I-006"
title = "keyring store fails on empty store — 'Secret Service: no result found' (create-item path)"
status = "done"
created = "2026-11-08"
owner = "devin"
priority = "high"
labels = ["identity", "keyring"]
+++

`arca init` fails to write the age identity into a reachable-but-empty Secret Service store.

Repro (Arch, gnome-keyring 51.1, dbus-activated daemon, no prior items):

1. `gnome-keyring` installed; `org.freedesktop.secrets` activatable, daemon running.
2. `identity.txt` present, keyring entry `arca/age-identity` absent.
3. `arca init` → `keyring set: Couldn't access platform storage: Secret Service: no result found` — exit 1, no migration.

Once *any* item matching `service=arca username=age-identity` exists (created via `secret-tool`), the failure mode changes: `k.load()` finds it, parse fails on non-key material, init falls back to file. With a correct item present, reads work fine — only the **create** path is broken.

Suspect: `dbus-secret-service-keyring-store` `set_secret` → no unique item → `create_item("default", …)` → `util::get_collection(&ss, "default")` or `collection.create_item` returns `ServiceError::NoResult` (mapped to `NoStorageAccess` in errors.rs). Empty `SearchItems` via `secret-tool search` exits 0, so the daemon's search itself is fine — need to pinpoint which call emits NoResult (ReadAlias "default"? CreateCollection? CreateItem?).

Impact: first-time `arca init` on a fresh machine can't bootstrap into the keyring; users must hand-write the item with `secret-tool` (what was done as workaround: `secret-tool store --label=… service arca username age-identity`).

Constraints: sync only, no unsafe; fix may need `[patch]`/vendoring if the bug is in `dbus-secret-service(-keyring-store)` rather than arca — evaluate whether an arca-side workaround (pre-create collection, different attr path) is cleaner.

## Resolution

fixed in 1d8ece8 — ensure_default_collection claims the default alias on fresh Secret Services

## Comments

- 2026-11-08 Root cause: get_collection("default") → ReadAlias returns '/' on a service with no collections → dbus-secret-service maps to NoResult → keyring-store decode_error → NoStorageAccess; the Err(NoEntry)→create_collection arm never fires, and util::create_collection("default") re-reads the same alias so the crate can't self-bootstrap. gnome-keyring only honors the 'default' alias (verified: other aliases → NotSupported). Fixed in 1d8ece8: platform_store() calls ensure_default_collection() — claims the default alias via create_collection("arca","default") when missing; best-effort, prompt-timeout 0. Caveat: exact fresh-service repro needs a wiped keyring so wasn't re-run e2e; residual edge — collections existing without a default alias can't be re-aliased via the public API.

- 2026-11-08 Caveat fixed in HEAD: collections-without-default-alias now handled — ensure_default_collection claims 'default' via raw org.freedesktop.Secret.Service.SetAlias (gnome-keyring implements it; dbus-secret-service's proxy is private so one raw dbus call, 5s timeout). Prefers the 'login' collection so secrets stay in the user's real keyring; falls back to creating 'arca' + SetAlias. SetAlias verified live against gnome-keyring 51.1 (alias moved, ReadAlias followed, restored). Adds dbus = 0.9 dep (already in tree).

+++
id = "I-006"
title = "keyring store fails on empty store — 'Secret Service: no result found' (create-item path)"
status = "open"
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

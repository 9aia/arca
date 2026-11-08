+++
id = "I-005"
title = "Audit should be OTel-based; stderr ndjson corrupts arca ui"
status = "open"
created = "2026-11-08"
owner = "devin"
priority = "high"
labels = ["audit", "observability", "tui"]
+++

Every verb emits audit as ndjson on stderr (`{ts, actor, op, rref, env, outcome}` — `arca-ops/src/audit.rs`). Two problems:

1. **`arca ui` corrupts** — TUI draws on stdout's alt screen; stderr writes splatter over the frame. No opt-out exists (only `ARCA_ACTOR`/`ARCA_ENV`/`ARCA_CONFIG`).
2. **stderr isn't observability** — no schema versioning, trace context, or backend routing; consumers merging streams (`2>&1`) get noise to parse-and-discard.

**Proposal:** emit audit as OTel spans/logs (`arca.op`, `arca.rref`, `arca.env`, `arca.outcome`, `arca.actor`, `policy.*`), honoring `OTEL_EXPORTER_OTLP_ENDPOINT`. Keep stderr ndjson as a deprecated env-gated shim (`ARCA_AUDIT_STDERR=1`) for existing peregrin/sepia capture. `arca ui` writes audit only to the exporter — never the tty — optionally surfacing events in a pane.

Fits the machine-first posture: OTel is the standard wire for agent observability; peregrin gets trace correlation it currently reconstructs from flat ndjson. Must stay sync / no async runtime.

(GH issue 9aia/arca#1 was opened in error and closed — this is the canonical record.)

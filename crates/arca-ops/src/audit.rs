//! Audit events — the wire stand-in until `arca serve` lands.
//!
//! ADR-0001 moved audit ownership to peregrin: no local
//! `audit.jsonl`. Instead arca emits one ndjson line per
//! materialization on **stderr** — a peregrin/sepia wrapper can
//! capture it, a human sees a breadcrumb. Keep the struct small and
//! serializable: it's the wire contract.

use std::io::{self, Write};

use serde::Serialize;
use time::OffsetDateTime;

use arca_core::Op;

/// One materialization (or mutation) record.
#[derive(Debug, Clone, Serialize)]
pub struct AuditEvent {
    /// RFC 3339 timestamp.
    #[serde(with = "time::serde::rfc3339")]
    pub ts: OffsetDateTime,
    /// Self-claimed actor (`--actor`/`ARCA_ACTOR`) — attribution,
    /// not identity.
    pub actor: String,
    /// The verb.
    pub op: Op,
    /// The resolved ref path (no env pin — `env` carries that).
    pub rref: String,
    /// The env the secret was materialized under.
    pub env: String,
}

impl AuditEvent {
    /// Stamp `now` and fill the fields.
    pub fn new(actor: &str, op: Op, rref: &str, env: &str) -> Self {
        AuditEvent {
            ts: OffsetDateTime::now_utc(),
            actor: actor.to_string(),
            op,
            rref: rref.to_string(),
            env: env.to_string(),
        }
    }
}

/// Write `event` as one ndjson line to stderr.
pub fn emit(event: &AuditEvent) {
    // stderr is the audit wire — a broken pipe must never take the
    // broker down, so failures are deliberately swallowed.
    let _ = emit_to(&mut io::stderr(), event);
}

/// Write `event` as one ndjson line to `w` — the testable core.
pub fn emit_to(w: &mut impl Write, event: &AuditEvent) -> io::Result<()> {
    let line =
        serde_json::to_string(event).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    writeln!(w, "{line}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn event_is_ndjson() {
        let ev = AuditEvent::new("devin", Op::Run, "billing/stripe", "prod");
        let mut buf = Vec::new();
        emit_to(&mut buf, &ev).unwrap();
        let line = String::from_utf8(buf).unwrap();
        assert!(line.ends_with('\n'));
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["actor"], "devin");
        assert_eq!(v["op"], "run");
        assert_eq!(v["rref"], "billing/stripe");
        assert_eq!(v["env"], "prod");
        assert!(v["ts"].is_string());
        assert!(!line.contains("value"), "audit never carries values");
    }
}

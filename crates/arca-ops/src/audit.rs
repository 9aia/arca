//! Audit events — the observability wire.
//!
//! ADR-0001 moved audit ownership to peregrin: no local
//! `audit.jsonl`. Instead arca emits ndjson lines on **stderr** —
//! a peregrin/sepia wrapper can capture them, a human sees a
//! breadcrumb — and, when `OTEL_EXPORTER_OTLP_ENDPOINT` is set,
//! buffers each event for an OTLP/HTTP JSON span export on
//! [`flush`] (see [`crate::otel`]).
//!
//! stderr stays the default human wire; `ARCA_AUDIT_STDERR=0`
//! silences it (the OTLP-only mode). [`mute_stderr`] suspends
//! live stderr writes for surfaces that own the terminal — the
//! TUI — and [`unmute_stderr`] replays the held events afterward,
//! so nothing is lost, just deferred past the alt-screen.
//!
//! Keep the struct small and serializable: it's the wire
//! contract on both sinks.
//!
//! Every verb invocation leaves a trail, uniformly:
//!
//! - **ok** — one event per ref materialized or mutated
//!   (`put`/`get`/`rotate`/`rm`, and per-ref inside `run`/`env`),
//!   or one op-scoped event for ref-less verbs
//!   (`init`/`list`/`check`/`ui`).
//! - **denied** — the op was refused by `[guardrails]` (advisory).
//! - **error** — the op ran but failed (missing secret, vault
//!   error, bad ref).
//!
//! `manifest`/`schema` run before config load — pure
//! introspection, no event. A `run` child's non-zero exit is the
//! child's outcome, not arca's — no `error` event for it.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use time::OffsetDateTime;

use arca_core::Op;

/// Process-wide audit sink — the OTLP buffer plus the stderr
/// mute the TUI engages. A poisoned lock is recovered rather than
/// panicking: audit must never take the broker down.
struct Sink {
    /// Events held for the next [`flush`] — only populated when
    /// `OTEL_EXPORTER_OTLP_ENDPOINT` is configured.
    otel: Vec<AuditEvent>,
    /// TUI mode — live stderr writes suspended; events collect in
    /// `held` for [`unmute_stderr`] to replay once the terminal is
    /// restored.
    muted: bool,
    held: Vec<AuditEvent>,
}

static SINK: Mutex<Sink> = Mutex::new(Sink {
    otel: Vec::new(),
    muted: false,
    held: Vec::new(),
});

fn sink() -> std::sync::MutexGuard<'static, Sink> {
    SINK.lock().unwrap_or_else(|e| e.into_inner())
}

/// `ARCA_AUDIT_STDERR` — `0`/`false`/`off`/`no` silences the
/// stderr wire; anything else (or unset) keeps it. Resolved once:
/// env doesn't change mid-process.
fn stderr_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        !matches!(
            std::env::var("ARCA_AUDIT_STDERR")
                .ok()
                .map(|v| v.to_ascii_lowercase())
                .as_deref(),
            Some("0" | "false" | "off" | "no")
        )
    })
}

/// Warn-once flag for OTLP export failures — a dead collector
/// shouldn't spam stderr once per flush.
static EXPORT_WARNED: AtomicBool = AtomicBool::new(false);

/// What the policy decision point said — recorded on serve-path
/// audit events so the stream shows the verdict behind the
/// outcome, not just the outcome. `None` on every other path:
/// standalone CLI ops have no PDP, and the field stays absent.
#[derive(Debug, Clone, Serialize)]
pub struct PolicyVerdict {
    /// The verdict — `false` is what a `denied` outcome carries.
    pub allow: bool,
    /// The PDP's reason, when it gave one (a deny's "why", an
    /// allow-on-error's "the PDP was unreachable").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Conditions the PDP attached (`allow-with-conditions` —
    /// e.g. redact, session-bound). Advisory: serve surfaces them
    /// in the response and records them here; enforcing them is
    /// the requester's job.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub obligations: Option<Vec<String>>,
}

/// How an audited op ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// The op did what was asked (a ref materialized/mutated, or a
    /// ref-less verb completed).
    Ok,
    /// Refused by `[guardrails]` — the op never touched the vault.
    Denied,
    /// The op ran and failed (missing secret, vault i/o, bad ref).
    Error,
}

/// One materialization, mutation, or op outcome record.
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
    /// Absent for op-scoped events (`list`, denials before parse).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rref: Option<String>,
    /// The env the op ran under.
    pub env: String,
    /// How it ended.
    pub outcome: Outcome,
    /// The PDP verdict behind this event — serve-path only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy: Option<PolicyVerdict>,
}

impl AuditEvent {
    /// A ref-scoped success — one materialization or mutation.
    pub fn materialized(actor: &str, op: Op, rref: &str, env: &str) -> Self {
        Self::new(actor, op, Some(rref), env, Outcome::Ok)
    }

    /// An op-scoped event — verbs with no single ref, denials, and
    /// failures. `rref` carries the raw requested ref when known
    /// (e.g. a denied `get arca://x/y`).
    pub fn op(actor: &str, op: Op, rref: Option<&str>, env: &str, outcome: Outcome) -> Self {
        Self::new(actor, op, rref, env, outcome)
    }

    fn new(actor: &str, op: Op, rref: Option<&str>, env: &str, outcome: Outcome) -> Self {
        AuditEvent {
            ts: OffsetDateTime::now_utc(),
            actor: actor.to_string(),
            op,
            rref: rref.map(str::to_string),
            env: env.to_string(),
            outcome,
            policy: None,
        }
    }

    /// Attach the PDP verdict that gated this event (serve path).
    pub fn with_policy(mut self, verdict: Option<PolicyVerdict>) -> Self {
        self.policy = verdict;
        self
    }
}

/// Route one event to the live sinks: stderr ndjson (unless muted
/// or `ARCA_AUDIT_STDERR` silences it) and the OTLP buffer (when
/// `OTEL_EXPORTER_OTLP_ENDPOINT` is configured). A broken stderr
/// must never take the broker down — write failures are
/// deliberately swallowed, same for the buffer.
pub fn emit(event: &AuditEvent) {
    let stderr_on = {
        let mut s = sink();
        if crate::otel::enabled() {
            s.otel.push(event.clone());
        }
        if s.muted {
            s.held.push(event.clone());
            false
        } else {
            stderr_enabled()
        }
    };
    if stderr_on {
        let _ = emit_to(&mut io::stderr(), event);
    }
}

/// Suspend live stderr audit writes — for surfaces that own the
/// terminal (the TUI's alternate screen). Events keep buffering
/// for OTLP and collect in `held` for [`unmute_stderr`].
pub fn mute_stderr() {
    sink().muted = true;
}

/// Resume stderr writes and replay every event held while muted —
/// call after the terminal is restored so the breadcrumb lands
/// below the UI, not inside it.
pub fn unmute_stderr() {
    let held = {
        let mut s = sink();
        s.muted = false;
        std::mem::take(&mut s.held)
    };
    if !stderr_enabled() {
        return;
    }
    let mut err = io::stderr();
    for ev in &held {
        let _ = emit_to(&mut err, ev);
    }
}

/// Export every buffered audit event as OTLP spans, then clear the
/// buffer. No-op when `OTEL_EXPORTER_OTLP_ENDPOINT` is unset or
/// the buffer is empty. Export failures warn once on stderr —
/// audit transport must never fail the op that produced it.
pub fn flush() {
    let events = {
        let mut s = sink();
        std::mem::take(&mut s.otel)
    };
    if events.is_empty() {
        return;
    }
    if let Err(e) = crate::otel::export(&events) {
        if !EXPORT_WARNED.swap(true, Ordering::Relaxed) {
            eprintln!("arca: otel audit export failed: {e}");
        }
    }
}

/// Write `event` as one ndjson line to `w` — the testable core.
///
/// Serializing this POD event cannot fail in practice — every
/// field is a string/enum — so the `expect` documents an invariant
/// rather than guards a reachable panic.
#[allow(clippy::unwrap_used)]
pub fn emit_to(w: &mut impl Write, event: &AuditEvent) -> io::Result<()> {
    let line = serde_json::to_string(event).expect("audit event serializes");
    writeln!(w, "{line}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn event_is_ndjson() {
        let ev = AuditEvent::materialized("devin", Op::Run, "billing/stripe", "prod");
        let mut buf = Vec::new();
        emit_to(&mut buf, &ev).unwrap();
        let line = String::from_utf8(buf).unwrap();
        assert!(line.ends_with('\n'));
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["actor"], "devin");
        assert_eq!(v["op"], "run");
        assert_eq!(v["rref"], "billing/stripe");
        assert_eq!(v["env"], "prod");
        assert_eq!(v["outcome"], "ok");
        assert!(v["ts"].is_string());
        assert!(!line.contains("value"), "audit never carries values");
    }

    #[test]
    fn op_scoped_and_outcome_events() {
        let mut buf = Vec::new();
        emit_to(
            &mut buf,
            &AuditEvent::op("devin", Op::List, None, "dev", Outcome::Ok),
        )
        .unwrap();
        emit_to(
            &mut buf,
            &AuditEvent::op(
                "devin",
                Op::Get,
                Some("billing/stripe"),
                "dev",
                Outcome::Denied,
            ),
        )
        .unwrap();
        let text = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert!(v.get("rref").is_none(), "op-scoped events omit rref");
        assert_eq!(v["outcome"], "ok");
        let v: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(v["outcome"], "denied");
        assert_eq!(v["rref"], "billing/stripe");
    }

    #[test]
    fn policy_verdict_serializes_only_when_attached() {
        let mut buf = Vec::new();
        emit_to(
            &mut buf,
            &AuditEvent::op("devin", Op::Get, Some("a/b"), "prod", Outcome::Denied).with_policy(
                Some(PolicyVerdict {
                    allow: false,
                    reason: Some("pdp: no prod reads".into()),
                    obligations: Some(vec!["session-bound".into()]),
                }),
            ),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&String::from_utf8(buf).unwrap()).unwrap();
        assert_eq!(v["policy"]["allow"], false);
        assert_eq!(v["policy"]["reason"], "pdp: no prod reads");
        assert_eq!(v["policy"]["obligations"][0], "session-bound");
        // unattached → the field stays absent (skip_serializing_if)
        let mut buf = Vec::new();
        emit_to(
            &mut buf,
            &AuditEvent::materialized("devin", Op::Get, "a/b", "dev"),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&String::from_utf8(buf).unwrap()).unwrap();
        assert!(v.get("policy").is_none());
    }

    #[test]
    fn a_failing_writer_propagates_the_io_error() {
        struct Dead;
        impl Write for Dead {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "dead"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut dead = Dead;
        // a flush on the dead writer is a no-op — only writes fail
        dead.flush().unwrap();
        let ev = AuditEvent::op("devin", Op::List, None, "dev", Outcome::Ok);
        assert!(emit_to(&mut dead, &ev).is_err());
        // and `emit` swallows the same failure — stderr can be a
        // broken pipe, arca must not crash
        emit(&ev);
    }
}

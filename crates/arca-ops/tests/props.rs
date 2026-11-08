//! Property tests for the audit wire — the ndjson contract must
//! hold for *arbitrary* field content: control chars, newlines,
//! unicode, huge strings. One event, one line, always valid JSON.

#![allow(clippy::unwrap_used)]

use arca_core::Op;
use arca_ops::audit::{emit_to, AuditEvent, Outcome};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// Whatever goes into actor/rref/env, the emitted line is
    /// exactly one `\n`-terminated line of valid JSON carrying the
    /// same values back.
    #[test]
    fn audit_line_is_single_line_json(
        actor in ".*",
        rref in ".*",
        env in ".*",
    ) {
        let ev = AuditEvent::op(&actor, Op::Get, Some(&rref), &env, Outcome::Ok);
        let mut buf = Vec::new();
        emit_to(&mut buf, &ev).unwrap();
        let text = String::from_utf8(buf).unwrap();

        prop_assert!(text.ends_with('\n'));
        prop_assert_eq!(text.lines().count(), 1);

        let v: serde_json::Value =
            serde_json::from_str(text.trim_end()).expect("valid JSON");
        prop_assert!(v["actor"] == serde_json::json!(actor));
        prop_assert!(v["rref"] == serde_json::json!(rref));
        prop_assert!(v["env"] == serde_json::json!(env));
        prop_assert!(v["op"] == serde_json::json!("get"));
        prop_assert!(v["outcome"] == serde_json::json!("ok"));
        // timestamp parses as RFC 3339
        prop_assert!(v["ts"].as_str().unwrap().contains('T'));
    }

    /// `rref: None` omits the field entirely — consumers can rely
    /// on absence, not null.
    #[test]
    fn op_scoped_events_omit_rref(actor in ".*", env in ".*") {
        let ev = AuditEvent::op(&actor, Op::List, None, &env, Outcome::Denied);
        let mut buf = Vec::new();
        emit_to(&mut buf, &ev).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&String::from_utf8(buf).unwrap()).unwrap();
        prop_assert!(v.get("rref").is_none());
        prop_assert!(v["op"] == serde_json::json!("list"));
        prop_assert!(v["outcome"] == serde_json::json!("denied"));
    }
}

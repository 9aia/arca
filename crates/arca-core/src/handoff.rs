//! Handoff entries — the `handoffs/<ID>.md` records that carry
//! resumable state between sessions (`arca handoff` /
//! `arca resume`, sepia on the boundary).
//!
//! One append-only file per item, uniform across all kinds —
//! tasks can't hold this inline in `TODO.md` (done tasks are
//! removed), and uniformity beats special-casing.

use crate::Actor;

/// One `## <date> · <actor>[ · session:<id>]` entry — a dated note
/// from one actor's session. `session` is provenance only: a sepia
/// session id, `cli:<pid>`, whatever wrote the entry — arca records
/// it, never interprets it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct HandoffEntry {
    /// Entry date — `YYYY-MM-DD` in the `## ` heading.
    #[serde(with = "crate::date::req")]
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub date: time::Date,
    /// Who wrote it — `devin`, `human:luis`, `claude`, …
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub actor: Actor,
    /// Session provenance (`session:<id>` in the heading); absent
    /// when the writer didn't supply one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Freeform note body — done/next/open, verbatim.
    pub note: String,
}

/// Can `s` be a `session:<id>` heading token? Non-empty, no `·`
/// (the heading's field separator), no newline.
pub fn is_valid_session(s: &str) -> bool {
    !s.is_empty() && !s.contains('·') && !s.contains('\n')
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::date;

    #[test]
    fn session_validation() {
        for ok in ["cli:abc123", "sepia-01J", "pid-1234", "a b c"] {
            assert!(is_valid_session(ok), "{ok}");
        }
        for bad in ["", "a·b", "two\nlines"] {
            assert!(!is_valid_session(bad), "{bad}");
        }
    }

    #[test]
    fn serde_roundtrip() {
        let e = HandoffEntry {
            date: date::parse("2026-10-29").unwrap(),
            actor: "devin".parse().unwrap(),
            session: Some("cli:abc".into()),
            note: "Done: x. Next: y.".into(),
        };
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["date"], "2026-10-29");
        assert_eq!(json["actor"], "devin");
        assert_eq!(json["session"], "cli:abc");
        let back: HandoffEntry = serde_json::from_value(json).unwrap();
        assert_eq!(back, e);
        // no session → key absent
        let bare = HandoffEntry { session: None, ..e };
        let j = serde_json::to_value(&bare).unwrap();
        assert!(!j.as_object().unwrap().contains_key("session"));
    }

    #[cfg(feature = "schema")]
    #[test]
    fn json_schema_compiles() {
        let schema = schemars::schema_for!(HandoffEntry);
        assert!(serde_json::to_value(&schema).is_ok());
    }
}

//! Item status — the owned state machine.
//!
//! ```text
//! open        → claimed | in-progress | deferred
//! claimed     → in-progress | open | deferred
//! in-progress → blocked | done | deferred | open
//! blocked     → in-progress | open | deferred
//! deferred    → open
//! done        → (terminal)
//! ```
//!
//! Invalid transitions are typed errors, never panics.

use std::fmt;
use std::str::FromStr;

/// Lifecycle status of an [`crate::Item`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Filed, unowned.
    Open,
    /// Owned by an actor, not yet started.
    Claimed,
    /// Actively worked.
    InProgress,
    /// Work started but stuck on a dependency.
    Blocked,
    /// Finished — terminal; done tasks leave `TODO.md`.
    Done,
    /// Parked — lives in `BACKLOG.md`.
    Deferred,
}

impl Status {
    /// All statuses, in declaration order.
    pub const ALL: [Status; 6] = [
        Status::Open,
        Status::Claimed,
        Status::InProgress,
        Status::Blocked,
        Status::Done,
        Status::Deferred,
    ];

    /// The `status:` token / serde value — `in-progress`, `claimed`, …
    pub fn token(self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Claimed => "claimed",
            Status::InProgress => "in-progress",
            Status::Blocked => "blocked",
            Status::Done => "done",
            Status::Deferred => "deferred",
        }
    }

    /// Every status reachable in one step — for `arca schema` and
    /// diagnostics.
    pub fn transitions(self) -> &'static [Status] {
        use Status::*;
        match self {
            Open => &[Claimed, InProgress, Deferred],
            Claimed => &[InProgress, Open, Deferred],
            InProgress => &[Blocked, Done, Deferred, Open],
            Blocked => &[InProgress, Open, Deferred],
            Deferred => &[Open],
            Done => &[],
        }
    }

    /// Is `self → to` a legal transition? Identity is **not** a
    /// transition — `open → open` is false.
    pub fn can_transition_to(self, to: Status) -> bool {
        self.transitions().contains(&to)
    }

    /// Transition or fail with a typed [`InvalidTransition`].
    pub fn transition_to(self, to: Status) -> Result<Status, InvalidTransition> {
        if self.can_transition_to(to) {
            Ok(to)
        } else {
            Err(InvalidTransition { from: self, to })
        }
    }
}

/// An illegal `from → to` step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid status transition {from} → {to}")]
pub struct InvalidTransition {
    /// Source status.
    pub from: Status,
    /// Requested target.
    pub to: Status,
}

/// Failure to parse a [`Status`] token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid status {0:?} — expected open|claimed|in-progress|blocked|done|deferred")]
pub struct ParseStatusError(pub String);

impl FromStr for Status {
    type Err = ParseStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Status::ALL
            .into_iter()
            .find(|st| st.token() == s)
            .ok_or_else(|| ParseStatusError(s.to_string()))
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.token())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use Status::*;

    #[test]
    fn legal_transitions() {
        for (from, to) in [
            (Open, Claimed),
            (Open, InProgress),
            (Open, Deferred),
            (Claimed, InProgress),
            (Claimed, Open),
            (Claimed, Deferred),
            (InProgress, Blocked),
            (InProgress, Done),
            (InProgress, Deferred),
            // un-starting work — a session dies mid-task, the item
            // honestly returns to open (`arca interrupt`)
            (InProgress, Open),
            (Blocked, InProgress),
            (Blocked, Open),
            (Blocked, Deferred),
            (Deferred, Open),
        ] {
            assert!(from.can_transition_to(to), "{from} → {to}");
            assert_eq!(from.transition_to(to).unwrap(), to);
        }
    }

    #[test]
    fn illegal_transitions() {
        for s in Status::ALL {
            // identity is never legal
            assert!(!s.can_transition_to(s));
            // done is terminal
            assert!(!Done.can_transition_to(s));
            // nothing else reaches done except in-progress
            if s != InProgress {
                assert!(!s.can_transition_to(Done), "{s} → done");
            }
        }
        // spot-check the remaining holes
        for (from, to) in [
            (Open, Blocked),
            (Open, Done),
            (Claimed, Blocked),
            (Claimed, Done),
            (Blocked, Claimed),
            (Blocked, Done),
            (Deferred, InProgress),
            (Deferred, Claimed),
            (Deferred, Done),
            (Deferred, Blocked),
        ] {
            assert!(!from.can_transition_to(to), "{from} → {to}");
            assert_eq!(
                from.transition_to(to).unwrap_err(),
                InvalidTransition { from, to }
            );
        }
    }

    #[test]
    fn transitions_list() {
        assert_eq!(Open.transitions(), &[Claimed, InProgress, Deferred]);
        assert_eq!(Done.transitions(), &[]);
        assert_eq!(Deferred.transitions(), &[Open]);
    }

    #[test]
    fn tokens_roundtrip() {
        for s in Status::ALL {
            assert_eq!(s.token().parse::<Status>().unwrap(), s);
            assert_eq!(s.to_string(), s.token());
        }
        assert_eq!("in-progress".parse::<Status>().unwrap(), InProgress);
        for bad in ["", "wip", "In-Progress", "open "] {
            assert!(bad.parse::<Status>().is_err(), "{bad}");
        }
        assert_eq!(
            "wip".parse::<Status>().unwrap_err(),
            ParseStatusError("wip".into())
        );
    }

    #[test]
    fn serde_kebab() {
        assert_eq!(
            serde_json::to_string(&InProgress).unwrap(),
            "\"in-progress\""
        );
        assert_eq!(
            serde_json::from_str::<Status>("\"in-progress\"").unwrap(),
            InProgress
        );
    }

    #[test]
    fn error_display() {
        let e = InvalidTransition {
            from: Done,
            to: Open,
        };
        assert_eq!(e.to_string(), "invalid status transition done → open");
    }
}

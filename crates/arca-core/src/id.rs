//! Typed item ids in one namespace — `T-012` tasks, `I-003` issues,
//! `P-002` proposals, `ADR-0004` ADRs, `PL-001` plans. Per-kind
//! counters; zero-padded to 3 digits (4 for ADR) when emitted.

use std::fmt;
use std::str::FromStr;

/// The kind of work item an [`ItemId`] addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    /// `T-NNN` — tasks live in `TODO.md` / `BACKLOG.md`, no file.
    Task,
    /// `I-NNN` — one file each under `issues/`.
    Issue,
    /// `P-NNN` — one file each under `proposals/`.
    Proposal,
    /// `ADR-NNNN` — one file each under `adrs/`.
    Adr,
    /// `PL-NNN` — one file each under `plans/`.
    Plan,
}

impl ItemKind {
    /// All kinds, in declaration order.
    pub const ALL: [ItemKind; 5] = [
        ItemKind::Task,
        ItemKind::Issue,
        ItemKind::Proposal,
        ItemKind::Adr,
        ItemKind::Plan,
    ];

    /// Id prefix — `T`, `I`, `P`, `ADR`, `PL`.
    pub fn prefix(self) -> &'static str {
        match self {
            ItemKind::Task => "T",
            ItemKind::Issue => "I",
            ItemKind::Proposal => "P",
            ItemKind::Adr => "ADR",
            ItemKind::Plan => "PL",
        }
    }

    /// Minimum zero-padded digit width in the canonical id form.
    pub fn width(self) -> usize {
        match self {
            ItemKind::Adr => 4,
            _ => 3,
        }
    }

    /// `meta.toml` `[next]` key — `t`, `i`, `p`, `adr`, `pl`.
    pub fn counter_key(self) -> &'static str {
        match self {
            ItemKind::Task => "t",
            ItemKind::Issue => "i",
            ItemKind::Proposal => "p",
            ItemKind::Adr => "adr",
            ItemKind::Plan => "pl",
        }
    }

    /// Kind from an id prefix (`T`, `ADR`, …); `None` when unknown.
    pub fn from_prefix(prefix: &str) -> Option<ItemKind> {
        ItemKind::ALL.into_iter().find(|k| k.prefix() == prefix)
    }
}

/// A typed, numbered id: kind + counter. `Ord` sorts by kind, then
/// number — `T-002 < T-010`, and `T-010 < I-001` across kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemId {
    kind: ItemKind,
    number: u32,
}

impl ItemId {
    /// Construct without validation (`number == 0` is allowed for
    /// internal use; `Display` still pads it).
    pub const fn new(kind: ItemKind, number: u32) -> Self {
        ItemId { kind, number }
    }

    /// The kind segment.
    pub fn kind(self) -> ItemKind {
        self.kind
    }

    /// The counter segment.
    pub fn number(self) -> u32 {
        self.number
    }

    /// The id that follows this one within the same kind.
    pub fn next(self) -> ItemId {
        ItemId {
            kind: self.kind,
            number: self.number.saturating_add(1),
        }
    }
}

/// Verbs are kind-locked — `arca issue resolve T-001` is a usage
/// error, not a lookup failure.
pub fn require_kind(id: ItemId, kind: ItemKind, verb: &str) -> Result<(), String> {
    if id.kind() == kind {
        Ok(())
    } else {
        Err(format!(
            "{verb} takes {}-NNN ids — {id} is a {} item",
            kind.prefix(),
            id.kind().prefix()
        ))
    }
}

/// Failure to parse an [`ItemId`] — bad prefix, empty/bad number,
/// or `0`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid item id {0:?} — expected <prefix>-<number> like T-012 or ADR-0004")]
pub struct ParseItemIdError(pub String);

impl FromStr for ItemId {
    type Err = ParseItemIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || ParseItemIdError(s.to_string());
        let (prefix, digits) = s.split_once('-').ok_or_else(bad)?;
        let kind = ItemKind::from_prefix(prefix).ok_or_else(bad)?;
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
        let number: u32 = digits.parse().map_err(|_| bad())?;
        if number == 0 {
            return Err(bad());
        }
        Ok(ItemId { kind, number })
    }
}

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}-{:0width$}",
            self.kind.prefix(),
            self.number,
            width = self.kind.width()
        )
    }
}

impl serde::Serialize for ItemId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for ItemId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for ItemId {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ItemId".into()
    }

    fn json_schema(gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
        String::json_schema(gen)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_canonical() {
        assert_eq!(
            "T-012".parse::<ItemId>().unwrap(),
            ItemId::new(ItemKind::Task, 12)
        );
        assert_eq!(
            "I-003".parse::<ItemId>().unwrap(),
            ItemId::new(ItemKind::Issue, 3)
        );
        assert_eq!(
            "P-002".parse::<ItemId>().unwrap(),
            ItemId::new(ItemKind::Proposal, 2)
        );
        assert_eq!(
            "ADR-0004".parse::<ItemId>().unwrap(),
            ItemId::new(ItemKind::Adr, 4)
        );
        assert_eq!(
            "PL-001".parse::<ItemId>().unwrap(),
            ItemId::new(ItemKind::Plan, 1)
        );
    }

    #[test]
    fn parse_accepts_noncanonical_width() {
        // Longer/shorter digit runs parse fine; emit re-canonicalizes.
        assert_eq!(
            "T-7".parse::<ItemId>().unwrap(),
            ItemId::new(ItemKind::Task, 7)
        );
        assert_eq!(
            "ADR-12".parse::<ItemId>().unwrap(),
            ItemId::new(ItemKind::Adr, 12)
        );
        assert_eq!(
            "T-01234".parse::<ItemId>().unwrap(),
            ItemId::new(ItemKind::Task, 1234)
        );
    }

    #[test]
    fn parse_rejects() {
        for bad in [
            "",
            "T",
            "T-",
            "-001",
            "X-001",
            "t-001",
            "T-abc",
            "T-0",
            "T-00",
            "T-1-2",
            "T-99999999999999999999",
            "ADR-0000",
            "T–001",
        ] {
            assert!(bad.parse::<ItemId>().is_err(), "{bad} should fail");
        }
    }

    #[test]
    fn display_pads() {
        assert_eq!(ItemId::new(ItemKind::Task, 12).to_string(), "T-012");
        assert_eq!(ItemId::new(ItemKind::Adr, 4).to_string(), "ADR-0004");
        assert_eq!(ItemId::new(ItemKind::Task, 1234).to_string(), "T-1234");
        assert_eq!(ItemId::new(ItemKind::Adr, 12345).to_string(), "ADR-12345");
    }

    #[test]
    fn ordering() {
        assert!(ItemId::new(ItemKind::Task, 2) < ItemId::new(ItemKind::Task, 10));
        // kinds sort in declaration order: Task < Issue < Proposal < Adr < Plan
        assert!(ItemId::new(ItemKind::Task, 999) < ItemId::new(ItemKind::Issue, 1));
        assert!(ItemId::new(ItemKind::Adr, 1) < ItemId::new(ItemKind::Plan, 1));
    }

    #[test]
    fn require_kind_message() {
        let t = ItemId::new(ItemKind::Task, 1);
        assert!(require_kind(t, ItemKind::Task, "todo").is_ok());
        let i = ItemId::new(ItemKind::Issue, 1);
        let e = require_kind(i, ItemKind::Task, "todo").unwrap_err();
        assert!(e.contains("T-NNN"), "{e}");
    }

    #[test]
    fn serde_string() {
        let id = ItemId::new(ItemKind::Adr, 4);
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"ADR-0004\"");
        assert_eq!(serde_json::from_str::<ItemId>("\"ADR-0004\"").unwrap(), id);
        assert!(serde_json::from_str::<ItemId>("\"nope\"").is_err());
    }

    #[test]
    fn kind_helpers() {
        assert_eq!(ItemKind::Task.prefix(), "T");
        assert_eq!(ItemKind::Adr.width(), 4);
        assert_eq!(ItemKind::Issue.width(), 3);
        assert_eq!(ItemKind::Task.counter_key(), "t");
        assert_eq!(ItemKind::Issue.counter_key(), "i");
        assert_eq!(ItemKind::Proposal.counter_key(), "p");
        assert_eq!(ItemKind::Adr.counter_key(), "adr");
        assert_eq!(ItemKind::Plan.counter_key(), "pl");
        assert_eq!(ItemKind::from_prefix("PL"), Some(ItemKind::Plan));
        assert_eq!(ItemKind::from_prefix("X"), None);
        assert_eq!(ItemKind::from_prefix(""), None);
        assert_eq!(
            ItemId::new(ItemKind::Task, 9).next(),
            ItemId::new(ItemKind::Task, 10)
        );
    }

    #[cfg(feature = "schema")]
    #[test]
    fn json_schema_compiles() {
        let schema = schemars::schema_for!(ItemId);
        assert!(serde_json::to_value(&schema).is_ok());
    }
}

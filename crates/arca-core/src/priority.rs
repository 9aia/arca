//! Item priority. Serde/frontmatter uses full kebab names
//! (`"medium"`); `pri:` line tokens use the short forms
//! `low|med|high|crit`.

use std::fmt;
use std::str::FromStr;

/// How urgent an item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    /// `pri:low`
    Low,
    /// `pri:med` — the default when no `pri:` token is present.
    #[default]
    Medium,
    /// `pri:high`
    High,
    /// `pri:crit`
    Critical,
}

impl Priority {
    /// All priorities, lowest first.
    pub const ALL: [Priority; 4] = [
        Priority::Low,
        Priority::Medium,
        Priority::High,
        Priority::Critical,
    ];

    /// The `pri:` token — `low`, `med`, `high`, `crit`.
    pub fn token(self) -> &'static str {
        match self {
            Priority::Low => "low",
            Priority::Medium => "med",
            Priority::High => "high",
            Priority::Critical => "crit",
        }
    }

    /// Full name used in serde/frontmatter — `low`, `medium`,
    /// `high`, `critical`.
    pub fn name(self) -> &'static str {
        match self {
            Priority::Low => "low",
            Priority::Medium => "medium",
            Priority::High => "high",
            Priority::Critical => "critical",
        }
    }
}

/// Failure to parse a priority token or name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid priority {0:?} — expected low|med|high|crit (or medium|critical)")]
pub struct ParsePriorityError(pub String);

impl FromStr for Priority {
    type Err = ParsePriorityError;

    /// Accepts both `pri:` tokens (`low|med|high|crit`) and full
    /// names (`low|medium|high|critical`) — lenient read, strict
    /// canonical write.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let p = match s {
            "low" => Priority::Low,
            "med" | "medium" => Priority::Medium,
            "high" => Priority::High,
            "crit" | "critical" => Priority::Critical,
            _ => return Err(ParsePriorityError(s.to_string())),
        };
        Ok(p)
    }
}

impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        assert_eq!(Priority::Low.token(), "low");
        assert_eq!(Priority::Medium.token(), "med");
        assert_eq!(Priority::High.token(), "high");
        assert_eq!(Priority::Critical.token(), "crit");
        assert_eq!(Priority::Medium.name(), "medium");
        assert_eq!(Priority::Critical.name(), "critical");
    }

    #[test]
    fn parse_tokens_and_names() {
        for p in Priority::ALL {
            assert_eq!(p.token().parse::<Priority>().unwrap(), p);
            assert_eq!(p.name().parse::<Priority>().unwrap(), p);
        }
        assert_eq!("med".parse::<Priority>().unwrap(), Priority::Medium);
        assert_eq!("crit".parse::<Priority>().unwrap(), Priority::Critical);
        for bad in ["", "urgent", "MED", "med "] {
            assert!(bad.parse::<Priority>().is_err(), "{bad}");
        }
        assert_eq!(
            "urgent".parse::<Priority>().unwrap_err(),
            ParsePriorityError("urgent".into())
        );
    }

    #[test]
    fn ordering_and_default() {
        assert!(Priority::Low < Priority::Critical);
        assert_eq!(Priority::default(), Priority::Medium);
        assert_eq!(Priority::default().to_string(), "medium");
    }

    #[test]
    fn serde_full_names() {
        assert_eq!(
            serde_json::to_string(&Priority::Medium).unwrap(),
            "\"medium\""
        );
        assert_eq!(
            serde_json::from_str::<Priority>("\"critical\"").unwrap(),
            Priority::Critical
        );
    }
}

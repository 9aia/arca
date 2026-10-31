//! `time::Date` helpers — arca serializes dates as `YYYY-MM-DD`
//! everywhere (TODO lines, frontmatter, JSON).

use time::macros::format_description;
use time::Date;

const FORMAT: &[time::format_description::FormatItem<'static>] =
    format_description!("[year]-[month]-[day]");

/// Parse a `YYYY-MM-DD` date.
pub fn parse(s: &str) -> Result<Date, time::error::Parse> {
    Date::parse(s, FORMAT)
}

/// Render as `YYYY-MM-DD` (`time::Date` Display is already ISO 8601).
pub fn render(d: Date) -> String {
    d.to_string()
}

/// Serde adapter for a required `Date` — `#[serde(with = "date::req")]`.
/// Serializes as `"YYYY-MM-DD"`.
pub mod req {
    use serde::{Deserialize, Deserializer, Serializer};
    use time::Date;

    /// Serialize as `"YYYY-MM-DD"`.
    pub fn serialize<S: Serializer>(d: &Date, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(&d.to_string())
    }

    /// Deserialize a `"YYYY-MM-DD"` string.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Date, D::Error> {
        let s = String::deserialize(d)?;
        super::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// Serde adapter for `Option<Date>` — `#[serde(with = "date::opt")]`.
pub mod opt {
    use serde::{Deserialize, Deserializer, Serializer};
    use time::Date;

    /// Serialize `Some(d)` as `"YYYY-MM-DD"`, `None` as absent/null.
    pub fn serialize<S: Serializer>(d: &Option<Date>, s: S) -> Result<S::Ok, S::Error> {
        match d {
            Some(d) => s.collect_str(&d.to_string()),
            None => s.serialize_none(),
        }
    }

    /// Deserialize a `"YYYY-MM-DD"` string or null.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Date>, D::Error> {
        let s = Option::<String>::deserialize(d)?;
        s.map(|s| super::parse(&s).map_err(serde::de::Error::custom))
            .transpose()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let d = parse("2026-02-09").unwrap();
        assert_eq!(render(d), "2026-02-09");
        assert!(parse("9/2/26").is_err());
        assert!(parse("2026-13-01").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn serde_opt() {
        #[derive(serde::Serialize, serde::Deserialize)]
        struct W {
            #[serde(with = "opt")]
            d: Option<time::Date>,
        }
        let json = serde_json::to_string(&W {
            d: parse("2026-02-09").ok(),
        })
        .unwrap();
        assert_eq!(json, r#"{"d":"2026-02-09"}"#);
        let back: W = serde_json::from_str(&json).unwrap();
        assert_eq!(back.d, parse("2026-02-09").ok());
        let none: W = serde_json::from_str(r#"{"d":null}"#).unwrap();
        assert!(none.d.is_none());
        assert_eq!(
            serde_json::to_string(&W { d: None }).unwrap(),
            r#"{"d":null}"#
        );
        assert!(serde_json::from_str::<W>(r#"{"d":"bogus"}"#).is_err());
    }

    #[test]
    fn serde_req() {
        #[derive(serde::Serialize, serde::Deserialize)]
        struct W {
            #[serde(with = "req")]
            d: time::Date,
        }
        let json = serde_json::to_string(&W {
            d: parse("2026-10-29").unwrap(),
        })
        .unwrap();
        assert_eq!(json, r#"{"d":"2026-10-29"}"#);
        let back: W = serde_json::from_str(&json).unwrap();
        assert_eq!(back.d, parse("2026-10-29").unwrap());
        assert!(serde_json::from_str::<W>(r#"{"d":"nope"}"#).is_err());
        assert!(serde_json::from_str::<W>(r#"{"d":null}"#).is_err());
    }
}

//! `meta.toml` — the one piece of state that isn't derivable:
//! repo name + persistent per-kind `[next]` counters (done tasks
//! leave `TODO.md`; ids are never reused).

use arca_core::ProjectMeta;

use crate::error::{EmitError, ParseError};

const FILE: &str = "meta.toml";

/// Parse `meta.toml`. TOML errors keep their span.
pub fn parse_meta(src: &str) -> Result<ProjectMeta, ParseError> {
    toml::from_str::<ProjectMeta>(src).map_err(|e| {
        let (offset, len) = e
            .span()
            .map(|s| (s.start, s.end.saturating_sub(s.start)))
            .unwrap_or((0, 1));
        ParseError::at(FILE, src.to_string(), offset, len, e.to_string())
    })
}

/// Canonical `meta.toml`.
///
/// ```toml
/// name = "arca"
/// [next]
/// t = 13
/// i = 4
/// p = 3
/// adr = 5
/// pl = 2
/// ```
pub fn emit_meta(meta: &ProjectMeta) -> Result<String, EmitError> {
    if meta.name.trim().is_empty() {
        return Err(EmitError::new(FILE, "empty repo name"));
    }
    toml::to_string(meta).map_err(|e| EmitError::new(FILE, e.to_string()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use arca_core::ItemKind;

    #[test]
    fn roundtrip() {
        let src = "name = \"arca\"\n\n[next]\nt = 13\ni = 4\np = 3\nadr = 5\npl = 2\n";
        let meta = parse_meta(src).unwrap();
        assert_eq!(meta.name, "arca");
        assert_eq!(meta.next.get(ItemKind::Task), 13);
        assert_eq!(meta.next.get(ItemKind::Adr), 5);
        assert_eq!(emit_meta(&meta).unwrap(), src);
    }

    #[test]
    fn partial_next_defaults_to_one() {
        let meta = parse_meta("name = \"x\"\n\n[next]\nt = 9\n").unwrap();
        assert_eq!(meta.next.get(ItemKind::Task), 9);
        assert_eq!(meta.next.get(ItemKind::Issue), 1);
        // no [next] at all
        let meta = parse_meta("name = \"x\"\n").unwrap();
        assert_eq!(meta.next.get(ItemKind::Task), 1);
    }

    #[test]
    fn parse_errors() {
        assert!(parse_meta("").is_err());
        assert!(parse_meta("name = 3\n").is_err());
        assert!(parse_meta("name = \"x\"\n[next]\nt = \"z\"\n").is_err());
        assert!(parse_meta("name = \"x\"\nnext = [").is_err());
        let e = parse_meta("name = \"x\"\n\nnext = [\n").unwrap_err();
        assert!(e.line() >= 1);
    }

    #[test]
    fn emit_rejects_empty_name() {
        assert!(emit_meta(&ProjectMeta::new(" ")).is_err());
        assert!(emit_meta(&ProjectMeta::new("arca")).is_ok());
    }
}

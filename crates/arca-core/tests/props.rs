//! Property tests — invariants that must hold for *every* input,
//! not just the fixtures in the unit tests. The interesting cases
//! here are the ones nobody thought to write down: unicode, control
//! chars, pathological lengths, near-miss strings.

#![allow(clippy::unwrap_used)]

use arca_core::{
    is_valid_actor, is_valid_env, is_valid_env_var, is_valid_segment, Actor, Env, Ref, Secret,
};
use proptest::prelude::*;

/// A valid ref segment: 1–24 chars from `[A-Za-z0-9._-]`, never
/// `.` or `..` — the exact [`is_valid_segment`] contract.
fn arb_segment() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![prop::sample::select(
            &b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._-"[..]
        )
        .prop_map(char::from)],
        1..=24,
    )
    .prop_map(|chars| chars.into_iter().collect::<String>())
    .prop_filter("segment is . or ..", |s| s != "." && s != "..")
}

proptest! {
    /// Every generated valid path survives `Ref` → display → parse
    /// unchanged, and every emitted segment still satisfies the
    /// segment validator.
    #[test]
    fn ref_path_roundtrips(segments in prop::collection::vec(arb_segment(), 1..=6)) {
        let path = segments.join("/");
        let r = Ref::from_path(&path).expect("generated path is valid");
        prop_assert_eq!(r.path(), path);
        prop_assert_eq!(r.segments(), segments.as_slice());

        let s = r.to_string();
        prop_assert!(s.starts_with("arca://"));
        let r2 = Ref::new(&s).expect("display output reparses");
        for seg in r2.segments() {
            prop_assert!(is_valid_segment(seg));
        }
        prop_assert!(r2 == r);
    }

    /// The inverse direction: whatever `Ref::new` accepts must be
    /// structurally valid — every path segment passes
    /// `is_valid_segment`, and display→parse is the identity.
    #[test]
    fn ref_new_only_accepts_valid_segments(s in ".*") {
        if let Ok(r) = Ref::new(&s) {
            let stripped = s.strip_prefix("arca://").unwrap();
            for seg in stripped.split('/') {
                prop_assert!(is_valid_segment(seg));
            }
            prop_assert!(Ref::new(&r.to_string()).unwrap() == r);
            // serde goes through the same string form
            let j = serde_json::to_string(&r).unwrap();
            prop_assert_eq!(serde_json::from_str::<Ref>(&j).unwrap(), r);
        }
    }

    /// `Secret` redaction is total: no matter the value, Debug and
    /// Display can never contain it. (Values shorter than the mask
    /// are excluded — `"***"` as a secret would trivially "leak".)
    #[test]
    fn secret_never_leaks(v in ".{4,}") {
        let s = Secret::new(v.clone());
        let dbg = format!("{s:?}");
        let disp = format!("{s}");
        prop_assert!(dbg == "Secret(***)");
        prop_assert!(disp == "***");
        prop_assert!(!dbg.contains(&v));
        prop_assert!(!disp.contains(&v));
        prop_assert_eq!(s.expose(), v.as_str());
        prop_assert_eq!(s.len(), v.len());
    }

    /// Env names: `Env::new` accepts iff `is_valid_env` says so,
    /// accepted names roundtrip through serde unchanged.
    #[test]
    fn env_names_roundtrip(s in ".*") {
        prop_assert_eq!(Env::new(&s).is_ok(), is_valid_env(&s));
    }

    #[test]
    fn valid_env_serde_roundtrips(s in "[a-z][a-z0-9_-]{0,63}") {
        let e = Env::new(&s).unwrap();
        prop_assert_eq!(e.as_str(), s.as_str());
        let j = serde_json::to_string(&e).unwrap();
        prop_assert_eq!(serde_json::from_str::<Env>(&j).unwrap(), e);
    }

    /// Env-var names for `[secrets]`/`-s`: accept iff the shell
    /// identifier rule holds — first char alpha/underscore, rest
    /// alphanumeric/underscore.
    #[test]
    fn env_var_names_match_shell_ident(s in ".*") {
        let ok = match s.as_bytes().first() {
            Some(b) if b.is_ascii_alphabetic() || *b == b'_' => s
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            _ => false,
        };
        prop_assert_eq!(is_valid_env_var(&s), ok);
    }

    /// Actor ids: accept iff non-empty, no `::`, no leading/trailing
    /// `:`, and only `[A-Za-z0-9._:-]`.
    #[test]
    fn actor_ids_match_their_contract(s in ".*") {
        let ok = !s.is_empty()
            && !s.contains("::")
            && !s.starts_with(':')
            && !s.ends_with(':')
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':'));
        prop_assert_eq!(is_valid_actor(&s), ok);
        prop_assert_eq!(Actor::new(&s).is_ok(), ok);
    }
}

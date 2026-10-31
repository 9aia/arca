//! Malformed fixtures must produce parse errors with usable
//! locations — the `arca check` contract.
#![allow(clippy::unwrap_used)]

use arca_core::ItemKind;
use arca_store_md::{parse_backlog, parse_handoff_file, parse_item_file, parse_meta, parse_todo};
use arca_testkit::{fixture, fixtures_under};

#[test]
fn todo_invalid() {
    for f in fixtures_under("todo/invalid") {
        let src = fixture(&f);
        let err = parse_todo(&src).unwrap_err();
        assert!(err.line() >= 1 && err.column() >= 1, "{f}: no span");
        insta::assert_snapshot!(f.replace('/', "__").replace('.', "_"), err.to_string());
    }
}

#[test]
fn backlog_invalid() {
    for f in fixtures_under("backlog/invalid") {
        let src = fixture(&f);
        let err = parse_backlog(&src).unwrap_err();
        assert!(err.line() >= 1 && err.column() >= 1, "{f}: no span");
        insta::assert_snapshot!(f.replace('/', "__").replace('.', "_"), err.to_string());
    }
}

#[test]
fn item_invalid() {
    for f in fixtures_under("items/invalid") {
        let src = fixture(&f);
        // kind mismatch is exercised by asking for Issue files
        let err = parse_item_file(&src, ItemKind::Issue, &f).unwrap_err();
        insta::assert_snapshot!(f.replace('/', "__").replace('.', "_"), err.to_string());
    }
}

#[test]
fn handoffs_invalid() {
    for f in fixtures_under("handoffs/invalid") {
        let src = fixture(&f);
        let err = parse_handoff_file(&src, &f).unwrap_err();
        assert!(err.line() >= 1 && err.column() >= 1, "{f}: no span");
        insta::assert_snapshot!(f.replace('/', "__").replace('.', "_"), err.to_string());
    }
}

#[test]
fn meta_invalid() {
    let e = parse_meta("name = [").unwrap_err();
    assert!(!e.message().is_empty());
}

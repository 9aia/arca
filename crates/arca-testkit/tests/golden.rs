//! Golden emit snapshots — `emit(parse(fixture))` is frozen per
//! fixture. Canonical fixtures snapshot byte-identical to
//! themselves; non-canonical ones freeze the canonical form.
#![allow(clippy::unwrap_used)]

use arca_core::ItemKind;
use arca_store_md::{
    emit_backlog, emit_handoff_file, emit_item_file, emit_meta, emit_todo, parse_backlog,
    parse_handoff_file, parse_item_file, parse_meta, parse_todo,
};
use arca_testkit::{fixture, fixtures_under};

fn snap_name(rel: &str) -> String {
    rel.replace('/', "__").replace('.', "_")
}

#[test]
fn todo_emit() {
    for f in fixtures_under("todo") {
        if f.contains("invalid") {
            continue;
        }
        let src = fixture(&f);
        let doc = parse_todo(&src).unwrap_or_else(|e| panic!("{f}: {e}"));
        let out = emit_todo(&doc).unwrap_or_else(|e| panic!("{f}: {e}"));
        insta::assert_snapshot!(snap_name(&f), out);
        // idempotent: re-parse re-emits byte-identical
        assert_eq!(emit_todo(&parse_todo(&out).unwrap()).unwrap(), out, "{f}");
    }
}

#[test]
fn backlog_emit() {
    for f in fixtures_under("backlog") {
        if f.contains("invalid") {
            continue;
        }
        let src = fixture(&f);
        let doc = parse_backlog(&src).unwrap_or_else(|e| panic!("{f}: {e}"));
        let out = emit_backlog(&doc).unwrap_or_else(|e| panic!("{f}: {e}"));
        insta::assert_snapshot!(snap_name(&f), out);
        assert_eq!(
            emit_backlog(&parse_backlog(&out).unwrap()).unwrap(),
            out,
            "{f}"
        );
    }
}

#[test]
fn meta_emit() {
    for f in fixtures_under("meta") {
        let src = fixture(&f);
        let meta = parse_meta(&src).unwrap_or_else(|e| panic!("{f}: {e}"));
        let out = emit_meta(&meta).unwrap_or_else(|e| panic!("{f}: {e}"));
        insta::assert_snapshot!(snap_name(&f), out);
        assert_eq!(emit_meta(&parse_meta(&out).unwrap()).unwrap(), out, "{f}");
    }
}

#[test]
fn item_emit() {
    for (dir, kind) in [
        ("items/issues", ItemKind::Issue),
        ("items/proposals", ItemKind::Proposal),
        ("items/adrs", ItemKind::Adr),
        ("items/plans", ItemKind::Plan),
    ] {
        for f in fixtures_under(dir) {
            let src = fixture(&f);
            let file = parse_item_file(&src, kind, &f).unwrap_or_else(|e| panic!("{f}: {e}"));
            let out = emit_item_file(&file).unwrap_or_else(|e| panic!("{f}: {e}"));
            insta::assert_snapshot!(snap_name(&f), out);
            assert_eq!(
                emit_item_file(&parse_item_file(&out, kind, &f).unwrap()).unwrap(),
                out,
                "{f}"
            );
        }
    }
}

#[test]
fn handoffs_emit() {
    for f in fixtures_under("handoffs") {
        if f.contains("invalid") {
            continue;
        }
        let src = fixture(&f);
        let (id, entries) = parse_handoff_file(&src, &f).unwrap_or_else(|e| panic!("{f}: {e}"));
        let out = emit_handoff_file(&id, &entries).unwrap_or_else(|e| panic!("{f}: {e}"));
        insta::assert_snapshot!(snap_name(&f), out);
        // idempotent: re-parse re-emits byte-identical
        let (id2, e2) = parse_handoff_file(&out, &f).unwrap();
        assert_eq!(emit_handoff_file(&id2, &e2).unwrap(), out, "{f}");
    }
}

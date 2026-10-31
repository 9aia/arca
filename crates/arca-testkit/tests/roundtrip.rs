//! proptest round-trips — `parse(emit(doc)) == doc` and emit is a
//! fixed point (`emit(parse(emit)) == emit`).
#![allow(clippy::unwrap_used)]

use arca_store_md::{
    emit_backlog, emit_handoff_file, emit_item_file, emit_meta, emit_todo, parse_backlog,
    parse_handoff_file, parse_item_file, parse_meta, parse_todo,
};
use arca_testkit::strat;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn todo_roundtrip(doc in strat::todo_doc()) {
        let out = emit_todo(&doc).unwrap();
        let back = parse_todo(&out).unwrap();
        prop_assert_eq!(&back, &doc);
        prop_assert_eq!(emit_todo(&back).unwrap(), out);
    }

    #[test]
    fn backlog_roundtrip(doc in strat::backlog_doc()) {
        let out = emit_backlog(&doc).unwrap();
        let back = parse_backlog(&out).unwrap();
        prop_assert_eq!(&back, &doc);
        prop_assert_eq!(emit_backlog(&back).unwrap(), out);
    }

    #[test]
    fn meta_roundtrip(meta in strat::project_meta()) {
        let out = emit_meta(&meta).unwrap();
        let back = parse_meta(&out).unwrap();
        prop_assert_eq!(&back, &meta);
        prop_assert_eq!(emit_meta(&back).unwrap(), out);
    }

    #[test]
    fn item_file_roundtrip(file in strat::item_file()) {
        let out = emit_item_file(&file).unwrap();
        let back = parse_item_file(&out, file.meta.id.kind(), "x.md").unwrap();
        prop_assert_eq!(&back, &file);
        prop_assert_eq!(emit_item_file(&back).unwrap(), out);
    }

    #[test]
    fn item_id_roundtrip(id in strat::item_kind().prop_flat_map(strat::item_id)) {
        prop_assert_eq!(id.to_string().parse::<arca_core::ItemId>().unwrap(), id);
    }

    #[test]
    fn handoffs_roundtrip(
        id in strat::item_kind().prop_flat_map(strat::item_id),
        entries in prop::collection::vec(strat::handoff_entry(), 0..5),
    ) {
        let out = emit_handoff_file(&id, &entries).unwrap();
        let (back_id, back) = parse_handoff_file(&out, "handoffs/x.md").unwrap();
        prop_assert_eq!(back_id, id);
        prop_assert_eq!(&back, &entries);
        // fixed point: emit(parse(emit)) is byte-identical
        let (id2, e2) =
            parse_handoff_file(&emit_handoff_file(&id, &back).unwrap(), "handoffs/x.md").unwrap();
        prop_assert_eq!(emit_handoff_file(&id2, &e2).unwrap(), out);
    }
}

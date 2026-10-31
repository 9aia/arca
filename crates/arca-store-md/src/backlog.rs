//! `BACKLOG.md` — `# Backlog` plus optional `##` sections of
//! `- T-NNN …` items. No checkboxes: backlog items are `Deferred`
//! (a `status:` token must say `deferred` if present). Items nest
//! like TODO.md — a `-` list indented under an item is its
//! `children` (2 spaces per level, canonical).

use arca_core::{BacklogDoc, Item, ItemKind, Section, Status};
use markdown::mdast::Node;

use crate::error::{EmitError, ParseError};
use crate::line::{self, LineSpec};
use crate::mdx;

const FILE: &str = "BACKLOG.md";
const SPEC: LineSpec = LineSpec {
    kind: ItemKind::Task,
    default_status: Status::Deferred,
    file: FILE,
};

fn err(src: &str, node: &Node, msg: impl Into<String>) -> ParseError {
    match node.position() {
        Some(pos) => ParseError::at_position(FILE, src.to_string(), pos, msg),
        None => ParseError::whole_file(FILE, src.to_string(), msg),
    }
}

/// Parse `BACKLOG.md` source into a [`BacklogDoc`].
pub fn parse_backlog(src: &str) -> Result<BacklogDoc, ParseError> {
    let src = mdx::normalize(src);
    let ast = mdx::parse(&src).map_err(|m| ParseError::whole_file(FILE, src.clone(), m))?;
    // to_mdast always yields Root; children() keeps it non-fatal
    let mut blocks = ast.children().into_iter().flatten();
    let Some(node @ Node::Heading(h1)) = blocks.next() else {
        return Err(ParseError::whole_file(
            FILE,
            src,
            "expected `# Backlog` title",
        ));
    };
    if h1.depth != 1 || mdx::single_text(node).map(str::trim) != Some("Backlog") {
        return Err(err(&src, node, "expected `# Backlog` title"));
    }
    let mut doc = BacklogDoc::default();
    for block in blocks {
        match block {
            Node::Heading(h) if h.depth == 2 => {
                let name = mdx::single_text(block)
                    .ok_or_else(|| err(&src, block, "section heading must be plain text"))?;
                doc.sections.push(Section::named(name.trim()));
            }
            Node::List(list) if !list.ordered => {
                if doc.sections.is_empty() {
                    doc.sections.push(Section::unsectioned());
                }
                let section = doc.sections.last_mut().expect("non-empty");
                parse_list(&src, list, section)?;
            }
            other => {
                return Err(err(
                    &src,
                    other,
                    "unexpected block — BACKLOG.md holds `##` sections and `-` items only",
                ));
            }
        }
    }
    Ok(doc)
}

fn parse_list(
    src: &str,
    list: &markdown::mdast::List,
    section: &mut Section,
) -> Result<(), ParseError> {
    for item in &list.children {
        // list children are ListItem by construction; skip anything else
        if let Node::ListItem(li) = item {
            section.items.push(parse_li(src, item, li)?);
        }
    }
    Ok(())
}

/// One `- T-NNN …` list item → an [`Item`]; a nested `-` list
/// under it parses recursively into `children`.
fn parse_li(src: &str, node: &Node, li: &markdown::mdast::ListItem) -> Result<Item, ParseError> {
    if li.checked.is_some() {
        return Err(err(
            src,
            node,
            "backlog items have no checkbox — they're deferred",
        ));
    }
    let (head, nested) = line::item_parts(li).map_err(|m| err(src, node, m))?;
    let text =
        mdx::single_text(head).ok_or_else(|| err(src, node, "item line must be plain text"))?;
    let mut parsed = line::parse_text(text, &SPEC).map_err(|m| err(src, node, m))?;
    if parsed.status != Status::Deferred {
        return Err(err(
            src,
            node,
            "backlog items are deferred — no other status is allowed",
        ));
    }
    if let Some(list) = nested {
        for child in &list.children {
            if let Node::ListItem(cli) = child {
                parsed.children.push(parse_li(src, child, cli)?);
            }
        }
    }
    Ok(parsed)
}

/// Canonical `BACKLOG.md` — same layout as [`crate::emit_todo`]
/// minus the `[ ]` checkboxes. Fails on non-deferred items and the
/// same line-grammar limits as TODO.md.
pub fn emit_backlog(doc: &BacklogDoc) -> Result<String, EmitError> {
    let err = |m: &str| EmitError::new(FILE, m);
    let mut out = String::from("# Backlog\n");
    for (i, section) in doc.sections.iter().enumerate() {
        for item in &section.items {
            check_subtree(item)?;
        }
        out.push('\n');
        match &section.name {
            Some(name) => {
                if name.trim() != *name || name.is_empty() {
                    return Err(err("empty or untrimmed section name"));
                }
                out.push_str("## ");
                out.push_str(&mdx::inline(name));
                out.push('\n');
                if !section.items.is_empty() {
                    out.push('\n');
                }
            }
            None => {
                if i != 0 {
                    return Err(err("unsectioned items must come before the first `##`"));
                }
                if section.items.is_empty() {
                    return Err(err("empty unsectioned section is unrepresentable"));
                }
            }
        }
        for item in &section.items {
            emit_item(&mut out, item, 0)?;
        }
    }
    Ok(out)
}

/// [`line::check_item`] + the deferred-only rule, applied to a
/// whole subtree.
fn check_subtree(item: &Item) -> Result<(), EmitError> {
    line::check_item(item, FILE)?;
    if item.status != Status::Deferred {
        return Err(EmitError::new(
            FILE,
            format!("non-deferred item {} in BACKLOG.md", item.id),
        ));
    }
    for c in &item.children {
        check_subtree(c)?;
    }
    Ok(())
}

/// One item line plus its nested children — 2-space indent per
/// level under the parent's bullet.
fn emit_item(out: &mut String, item: &Item, depth: usize) -> Result<(), EmitError> {
    for _ in 0..depth {
        out.push_str("  ");
    }
    out.push_str("- ");
    out.push_str(&mdx::inline(&line::text(item, &SPEC)?));
    out.push('\n');
    for c in &item.children {
        emit_item(out, c, depth + 1)?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use arca_core::{Item, ItemId};

    #[test]
    fn roundtrip() {
        let src = "# Backlog\n\n## Someday\n\n- T-034 Old idea · pri:low · #maybe\n- T-040 Another\n\n## Rejected\n\n- T-050 Meh\n";
        let doc = parse_backlog(src).unwrap();
        assert_eq!(doc.sections.len(), 2);
        assert_eq!(doc.items().count(), 3);
        assert!(doc.items().all(|i| i.status == Status::Deferred));
        assert_eq!(emit_backlog(&doc).unwrap(), src);
    }

    #[test]
    fn unsectioned() {
        let src = "# Backlog\n\n- T-001 loose\n";
        let doc = parse_backlog(src).unwrap();
        assert_eq!(doc.sections[0].name, None);
        assert_eq!(emit_backlog(&doc).unwrap(), src);
        assert_eq!(parse_backlog(&emit_backlog(&doc).unwrap()).unwrap(), doc);
    }

    #[test]
    fn explicit_deferred_ok() {
        let src = "# Backlog\n\n- T-001 x · status:deferred\n";
        let doc = parse_backlog(src).unwrap();
        assert_eq!(doc.sections[0].items[0].status, Status::Deferred);
        // canonical emit drops the redundant token
        assert_eq!(emit_backlog(&doc).unwrap(), "# Backlog\n\n- T-001 x\n");
    }

    #[test]
    fn empty_doc() {
        let doc = BacklogDoc::default();
        let out = emit_backlog(&doc).unwrap();
        assert_eq!(out, "# Backlog\n");
        assert_eq!(parse_backlog("# Backlog\n").unwrap(), doc);
    }

    #[test]
    fn parse_errors() {
        for (src, why) in [
            ("# Back\n", "bad title"),
            ("## Backlog\n", "h2 title"),
            ("text\n\n# Backlog\n", "not first"),
            ("# Backlog\n\ntext\n", "stray para"),
            ("# Backlog\n\n### x\n", "h3"),
            ("# Backlog\n\n## s *em*\n", "rich heading"),
            ("# Backlog\n\n- [ ] T-001 x\n", "checkbox"),
            ("# Backlog\n\n- T-001 x · status:open\n", "open status"),
            ("# Backlog\n\n- T-001 x · status:done\n", "done status"),
            ("# Backlog\n\n- I-001 x\n", "wrong kind"),
            ("# Backlog\n\n- T-001\n  cont\n", "multi-line"),
            ("# Backlog\n\n1. T-001 x\n", "ordered"),
        ] {
            assert!(parse_backlog(src).is_err(), "{why}: {src:?}");
        }
    }

    #[test]
    fn err_falls_back_to_file_start_without_position() {
        let n = Node::Text(markdown::mdast::Text {
            value: "x".into(),
            position: None,
        });
        let e = err("src", &n, "m");
        assert_eq!((e.line(), e.column()), (1, 1));
    }

    #[test]
    fn nested_items_parse_into_children() {
        let src = "# Backlog\n\n- T-001 a\n  - T-002 sub\n  - T-003 sub two · status:deferred\n- T-004 sib\n";
        let doc = parse_backlog(src).unwrap();
        assert_eq!(doc.sections[0].items.len(), 2);
        let parent = &doc.sections[0].items[0];
        assert_eq!(parent.children.len(), 2);
        // the redundant token is dropped on canonical emit
        assert_eq!(
            emit_backlog(&doc).unwrap(),
            "# Backlog\n\n- T-001 a\n  - T-002 sub\n  - T-003 sub two\n- T-004 sib\n"
        );
    }

    #[test]
    fn nested_parse_errors() {
        for (src, why) in [
            ("# Backlog\n\n- T-001 a\n  - [ ] T-002 x\n", "checkbox"),
            (
                "# Backlog\n\n- T-001 a\n  - T-002 x · status:open\n",
                "open child",
            ),
        ] {
            assert!(parse_backlog(src).is_err(), "{why}: {src:?}");
        }
        // emit rejects a non-deferred child too
        let mut doc = BacklogDoc::default();
        let child = Item::new(ItemId::new(ItemKind::Task, 2), "sub").status(Status::Open);
        doc.sections.push(Section {
            name: None,
            items: vec![Item::new(ItemId::new(ItemKind::Task, 1), "p")
                .status(Status::Deferred)
                .child(child)],
        });
        assert!(emit_backlog(&doc).is_err());
    }

    #[test]
    fn parse_list_skips_non_listitem_children() {
        let list = markdown::mdast::List {
            ordered: false,
            start: None,
            spread: false,
            children: vec![Node::Paragraph(markdown::mdast::Paragraph {
                children: vec![],
                position: None,
            })],
            position: None,
        };
        let mut section = Section::default();
        parse_list("", &list, &mut section).unwrap();
        assert!(section.items.is_empty());
    }

    #[test]
    fn emit_errors() {
        let mut doc = BacklogDoc::default();
        doc.sections.push(Section {
            name: Some("s".into()),
            items: vec![Item::new(ItemId::new(ItemKind::Task, 1), "x").status(Status::Open)],
        });
        assert!(emit_backlog(&doc).is_err(), "non-deferred");

        let mut doc = BacklogDoc::default();
        doc.sections.push(Section::named("a"));
        doc.sections.push(Section::unsectioned());
        assert!(emit_backlog(&doc).is_err(), "unsectioned at idx>0");

        let mut doc = BacklogDoc::default();
        doc.sections.push(Section::unsectioned());
        assert!(emit_backlog(&doc).is_err(), "empty unsectioned");

        let mut doc = BacklogDoc::default();
        doc.sections.push(Section {
            name: Some(" x".into()),
            items: vec![],
        });
        assert!(emit_backlog(&doc).is_err(), "untrimmed name");
    }

    #[test]
    fn nested_non_line_item_errors() {
        // a nested done item — parse_li's status check rejects it
        // and the error propagates out of the nested-children loop
        let src = "# Backlog\n\n## s\n\n- T-001 a\n  - [x] T-002 done\n";
        assert!(parse_backlog(src).is_err());
        // a nested item with no line head errors too
        let src = "# Backlog\n\n## s\n\n- T-001 a\n  - \n    ```\n    c\n    ```\n";
        assert!(parse_backlog(src).is_err());
    }

    #[test]
    fn nested_list_skips_non_listitem_children() {
        // defensive: mdast never puts non-ListItems in a list, but
        // a hand-built tree may
        let li = markdown::mdast::ListItem {
            checked: None,
            spread: false,
            position: None,
            children: vec![
                Node::Paragraph(markdown::mdast::Paragraph {
                    children: vec![Node::Text(markdown::mdast::Text {
                        value: "T-001 x".into(),
                        position: None,
                    })],
                    position: None,
                }),
                Node::List(markdown::mdast::List {
                    ordered: false,
                    start: None,
                    spread: false,
                    position: None,
                    children: vec![Node::Paragraph(markdown::mdast::Paragraph {
                        children: vec![],
                        position: None,
                    })],
                }),
            ],
        };
        let node = Node::ListItem(li.clone());
        let item = parse_li("", &node, &li).unwrap();
        assert_eq!(item.id.number(), 1);
        assert!(item.children.is_empty());
    }
}

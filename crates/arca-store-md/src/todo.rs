//! `TODO.md` — `# TODO — <repo>` then `## <section>` headings with
//! `- [ ] T-NNN …` task items. Checkbox is always `[ ]` (open-ish
//! statuses); `done` items don't belong — they're removed by
//! `arca todo done`. Items nest: a `- [ ]` list indented under an
//! item is its `children` (2 spaces per level, canonical).
//!
//! Emit is hand-written (canonical lines) because
//! `mdast_util_to_markdown` 0.0.2 can't emit GFM `[ ]` checkboxes —
//! inline text still goes through it for escaping.

use arca_core::{Item, ItemKind, Section, Status, TodoDoc};
use markdown::mdast::Node;

use crate::error::{EmitError, ParseError};
use crate::line::{self, LineSpec};
use crate::mdx;

const FILE: &str = "TODO.md";
const SPEC: LineSpec = LineSpec {
    kind: ItemKind::Task,
    default_status: Status::Open,
    file: FILE,
};

fn err(src: &str, node: &Node, msg: impl Into<String>) -> ParseError {
    match node.position() {
        Some(pos) => ParseError::at_position(FILE, src.to_string(), pos, msg),
        None => ParseError::whole_file(FILE, src.to_string(), msg),
    }
}

/// Parse `TODO.md` source into a [`TodoDoc`]. Strict — any block
/// that isn't the `# TODO —` title, an h2 section, or a task list
/// is an error with a span.
pub fn parse_todo(src: &str) -> Result<TodoDoc, ParseError> {
    let src = mdx::normalize(src);
    let ast = mdx::parse(&src).map_err(|m| ParseError::whole_file(FILE, src.clone(), m))?;
    // to_mdast always yields Root; children() keeps it non-fatal
    let mut blocks = ast.children().into_iter().flatten();
    let title_err =
        || ParseError::whole_file(FILE, src.clone(), "expected `# TODO — <repo>` title");
    let Some(node @ Node::Heading(h1)) = blocks.next() else {
        return Err(title_err());
    };
    if h1.depth != 1 {
        return Err(err(&src, node, "expected `# TODO — <repo>` title"));
    }
    let repo = parse_title(mdx::single_text(node).unwrap_or_default())
        .ok_or_else(|| err(&src, node, "expected `# TODO — <repo>` title"))?;
    if repo.is_empty() {
        return Err(err(&src, node, "empty repo name in `# TODO — <repo>`"));
    }
    let mut doc = TodoDoc::new(repo);
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
                    "unexpected block — TODO.md holds `##` sections and `- [ ]` items only",
                ));
            }
        }
    }
    Ok(doc)
}

/// `# TODO — <repo>` / `# TODO - <repo>` → repo name (`None` when
/// the title isn't in that shape).
fn parse_title(h1: &str) -> Option<String> {
    let rest = h1.strip_prefix("TODO")?.trim_start();
    let rest = rest.strip_prefix('—').or_else(|| rest.strip_prefix('-'))?;
    Some(rest.trim().to_string())
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

/// One `- [ ] T-NNN …` list item → an [`Item`]; a nested `-` list
/// under it parses recursively into `children`.
fn parse_li(src: &str, node: &Node, li: &markdown::mdast::ListItem) -> Result<Item, ParseError> {
    match li.checked {
        Some(true) => {
            return Err(err(
                src,
                node,
                "checked item — done tasks are removed from TODO.md",
            ));
        }
        None => {
            return Err(err(src, node, "TODO items must be `- [ ]` task items"));
        }
        Some(false) => {}
    }
    let (head, nested) = line::item_parts(li).map_err(|m| err(src, node, m))?;
    let text =
        mdx::single_text(head).ok_or_else(|| err(src, node, "item line must be plain text"))?;
    let mut parsed = line::parse_text(text, &SPEC).map_err(|m| err(src, node, m))?;
    if parsed.status == Status::Done {
        return Err(err(
            src,
            node,
            "status:done — done tasks are removed from TODO.md",
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

/// Canonical `TODO.md` — deterministic output. Fails on models the
/// grammar can't express (`done` items, `·` in titles, unsectioned
/// sections after index 0, …).
pub fn emit_todo(doc: &TodoDoc) -> Result<String, EmitError> {
    let err = |m: &str| EmitError::new(FILE, m);
    if doc.repo.is_empty() || doc.repo.trim() != doc.repo {
        return Err(err("empty or untrimmed repo name"));
    }
    let mut out = String::from("# TODO — ");
    out.push_str(&mdx::inline(&doc.repo));
    out.push('\n');
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

/// [`line::check_item`] + the no-done rule, applied to a whole
/// subtree — children follow the same grammar as their parents.
fn check_subtree(item: &Item) -> Result<(), EmitError> {
    line::check_item(item, FILE)?;
    if item.status == Status::Done {
        return Err(EmitError::new(
            FILE,
            format!("done item {} — remove it from TODO.md", item.id),
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
    out.push_str("- [ ] ");
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
        let src = "# TODO — arca\n\n## Now\n\n- [ ] T-012 Write the thing · status:in-progress · pri:high · owner:devin · created:2026-02-09 · #core · blocks:T-010\n- [ ] T-013 plain\n\n## Later\n\n- [ ] T-020 t\n";
        let doc = parse_todo(src).unwrap();
        assert_eq!(doc.repo, "arca");
        assert_eq!(doc.sections.len(), 2);
        assert_eq!(doc.sections[0].items.len(), 2);
        assert_eq!(emit_todo(&doc).unwrap(), src);
    }

    #[test]
    fn hyphen_title_and_unsectioned() {
        let src = "# TODO - arca\n\n- [ ] T-001 loose\n\n## Now\n\n- [ ] T-002 x\n";
        let doc = parse_todo(src).unwrap();
        assert_eq!(doc.repo, "arca");
        assert_eq!(doc.sections[0].name, None);
        assert_eq!(doc.sections[0].items.len(), 1);
        // canonical emit keeps the unsectioned run at the top
        let out = emit_todo(&doc).unwrap();
        assert!(out.starts_with("# TODO — arca\n\n- [ ] T-001 loose\n\n## Now"));
        assert_eq!(parse_todo(&out).unwrap(), doc);
    }

    #[test]
    fn title_parsing() {
        assert_eq!(parse_title("TODO — arca").as_deref(), Some("arca"));
        assert_eq!(parse_title("TODO - arca").as_deref(), Some("arca"));
        assert_eq!(parse_title("TODO—arca").as_deref(), Some("arca"));
        assert_eq!(parse_title("TODO"), None);
        assert_eq!(parse_title("TODOX"), None);
        assert_eq!(parse_title("TODO —"), Some(String::new()));
        assert_eq!(parse_title("Something"), None);
    }

    #[test]
    fn empty_doc() {
        let doc = TodoDoc::new("arca");
        assert_eq!(emit_todo(&doc).unwrap(), "# TODO — arca\n");
        assert_eq!(parse_todo("# TODO — arca\n").unwrap(), doc);
    }

    #[test]
    fn parse_errors() {
        for (src, why) in [
            ("# Something else\n", "bad title"),
            ("# TODO\n", "no repo"),
            ("# TODOX — x\n", "not the title"),
            ("# TODO — \n", "empty repo"),
            ("## TODO — x\n", "h2 title"),
            ("para\n\n# TODO — x\n", "title not first"),
            ("# TODO — x\n\ntext\n", "stray paragraph"),
            ("# TODO — x\n\n### deep\n", "h3"),
            ("# TODO — x\n\n## s *em*\n", "rich heading"),
            ("# TODO — x\n\n- plain item\n", "no checkbox"),
            ("# TODO — x\n\n- [x] T-001 done\n", "checked"),
            ("# TODO — x\n\n- [ ] I-001 wrong\n", "wrong kind"),
            ("# TODO — x\n\n- [ ] T-001 t · status:done\n", "done status"),
            ("# TODO — x\n\n- [ ] T-001\n  continued\n", "multi-line"),
            ("# TODO — x\n\n1. [ ] T-001 o\n", "ordered list"),
            ("# TODO — x\n\n- [ ] T-001 a *b*\n", "rich line"),
        ] {
            assert!(parse_todo(src).is_err(), "{why}: {src:?}");
        }
    }

    #[test]
    fn emit_errors() {
        let mut doc = TodoDoc::new("arca");
        doc.sections.push(Section {
            name: None,
            items: vec![Item::new(ItemId::new(ItemKind::Task, 1), "x")],
        });
        doc.sections.push(Section {
            name: None,
            items: vec![],
        });
        assert!(emit_todo(&doc).is_err(), "unsectioned after index 0");

        let mut doc = TodoDoc::new("arca");
        doc.sections.push(Section {
            name: Some("s".into()),
            items: vec![Item::new(ItemId::new(ItemKind::Task, 1), "x").status(Status::Done)],
        });
        assert!(emit_todo(&doc).is_err(), "done item");

        assert!(emit_todo(&TodoDoc::new("")).is_err(), "empty repo");
        assert!(emit_todo(&TodoDoc::new(" x ")).is_err(), "untrimmed repo");

        let mut doc = TodoDoc::new("arca");
        doc.sections.push(Section {
            name: Some(" ".into()),
            items: vec![],
        });
        assert!(emit_todo(&doc).is_err(), "blank section name");

        let mut doc = TodoDoc::new("arca");
        doc.sections.push(Section::unsectioned());
        assert!(emit_todo(&doc).is_err(), "empty unsectioned");
    }

    #[test]
    fn error_has_span() {
        let src = "# TODO — x\n\n- [x] T-001 done\n";
        let e = parse_todo(src).unwrap_err();
        assert_eq!((e.line(), e.column()), (3, 1));
        assert!(e.message().contains("checked"));
    }

    #[test]
    fn err_falls_back_to_file_start_without_position() {
        // hand-built mdast nodes carry no position — the error
        // degrades to the file start rather than panicking
        let n = Node::Text(markdown::mdast::Text {
            value: "x".into(),
            position: None,
        });
        let e = err("src", &n, "m");
        assert_eq!((e.line(), e.column()), (1, 1));
    }

    #[test]
    fn nested_items_parse_into_children() {
        let src = "# TODO — x\n\n- [ ] T-001 a\n  - [ ] T-002 sub one · pri:low\n  - [ ] T-003 sub two\n    - [ ] T-004 deep\n- [ ] T-005 sib\n";
        let doc = parse_todo(src).unwrap();
        assert_eq!(doc.sections[0].items.len(), 2);
        let parent = &doc.sections[0].items[0];
        assert_eq!(parent.children.len(), 2);
        assert_eq!(parent.children[0].id.number(), 2);
        assert_eq!(parent.children[0].priority, arca_core::Priority::Low);
        assert_eq!(parent.children[1].children[0].id.number(), 4);
        // canonical emit is 2 spaces per level and round-trips
        assert_eq!(emit_todo(&doc).unwrap(), src);
        // flattened view sees the whole subtree
        let ids: Vec<u32> = parent.subtree().map(|i| i.id.number()).collect();
        assert_eq!(ids, [1, 2, 3, 4]);
    }

    #[test]
    fn nested_parse_errors() {
        for (src, why) in [
            // a done child is still done — removed, not parked
            (
                "# TODO — x\n\n- [ ] T-001 a\n  - [x] T-002 done\n",
                "checked",
            ),
            (
                "# TODO — x\n\n- [ ] T-001 a\n  - [ ] T-002 x · status:done\n",
                "status:done",
            ),
            (
                "# TODO — x\n\n- [ ] T-001 a\n  - bare bullet\n",
                "no checkbox",
            ),
            (
                "# TODO — x\n\n- [ ] T-001 a\n  1. [ ] T-002 ordered\n",
                "ordered nested",
            ),
            (
                "# TODO — x\n\n- [ ] T-001 a\n  - [ ] T-002\n    second para\n",
                "paragraph under item",
            ),
        ] {
            assert!(parse_todo(src).is_err(), "{why}: {src:?}");
        }
    }

    #[test]
    fn nested_item_must_have_paragraph_head() {
        // a nested list with no text line at all — nothing to parse
        let src = "# TODO — x\n\n- [ ] T-001 a\n  -\n";
        assert!(parse_todo(src).is_err());
    }

    #[test]
    fn emit_rejects_done_children() {
        let mut doc = TodoDoc::new("x");
        let child = Item::new(ItemId::new(ItemKind::Task, 2), "sub").status(Status::Done);
        doc.sections.push(Section {
            name: None,
            items: vec![Item::new(ItemId::new(ItemKind::Task, 1), "parent").child(child)],
        });
        let e = emit_todo(&doc).unwrap_err();
        assert!(e.to_string().contains("T-002"), "{e}");
    }

    #[test]
    fn parse_list_skips_non_listitem_children() {
        // defensive: mdast lists only ever contain ListItem, but a
        // hand-built list may not
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
    fn escaping_roundtrips() {
        let mut doc = TodoDoc::new("my*repo");
        doc.sections.push(Section {
            name: Some("Sec *one*".into()),
            items: vec![Item::new(ItemId::new(ItemKind::Task, 1), "a *b* c")],
        });
        let out = emit_todo(&doc).unwrap();
        assert!(out.contains("\\*"));
        assert_eq!(parse_todo(&out).unwrap(), doc);
    }

    #[test]
    fn nested_non_line_item_errors() {
        // a nested done item — parse_li's status check rejects it
        // and the error propagates out of the nested-children loop
        let src = "# TODO — x\n\n- [ ] T-001 a\n  - [x] T-002 done\n";
        assert!(parse_todo(src).is_err());
        // a nested item with no line head errors too
        let src = "# TODO — x\n\n- [ ] T-001 a\n  - \n    ```\n    c\n    ```\n";
        assert!(parse_todo(src).is_err());
    }

    #[test]
    fn nested_list_skips_non_listitem_children() {
        // defensive: a real parser never puts non-ListItems in a
        // list, but a hand-built mdast may
        let li = markdown::mdast::ListItem {
            checked: Some(false),
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

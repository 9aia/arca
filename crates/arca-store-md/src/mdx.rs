//! mdast helpers — the `markdown`/`mdast_util_to_markdown` plumbing
//! shared by the document parsers and emitters.

use markdown::mdast::{Node, Paragraph, Text};
use markdown::{to_mdast, Constructs, ParseOptions};

/// `ParseOptions`: CommonMark + GFM task-list items only — autolinks,
/// tables etc. stay off so item lines stay plain.
pub(crate) fn parse_options() -> ParseOptions {
    ParseOptions {
        constructs: Constructs {
            gfm_task_list_item: true,
            ..Constructs::default()
        },
        ..ParseOptions::default()
    }
}

/// `to_mdast` with arca's options; the error is flattened to text.
pub(crate) fn parse(src: &str) -> Result<Node, String> {
    to_mdast(src, &parse_options()).map_err(|m| m.to_string())
}

/// Emit options — canonical style: `-` bullets, `*` emphasis, ATX
/// headings, fenced code with backticks.
pub(crate) fn emit_options() -> mdast_util_to_markdown::Options {
    mdast_util_to_markdown::Options {
        bullet: '-',
        ..Default::default()
    }
}

/// Serialize one inline string — escapes markdown specials (`*`,
/// `[`, leading `#`, …) so hand-emitted lines re-parse to the same
/// text. Uses `mdast_util_to_markdown` but verifies the result by
/// re-parsing: 0.0.2's escaper corrupts sequences like `· #_x`
/// (byte/char index drift — `_` is eaten and the next char is
/// entity-encoded). On divergence it falls back to [`escape`].
pub(crate) fn inline(text: &str) -> String {
    let node = Node::Paragraph(Paragraph {
        children: vec![Node::Text(Text {
            value: text.to_string(),
            position: None,
        })],
        position: None,
    });
    let via_lib = mdast_util_to_markdown::to_markdown_with_options(&node, &emit_options())
        .unwrap_or_default();
    let via_lib = via_lib.trim_end_matches('\n');
    let verified = parse(via_lib)
        .ok()
        .as_ref()
        .and_then(Node::children)
        .and_then(|c| c.first())
        .and_then(single_text)
        == Some(text);
    if verified {
        via_lib.to_string()
    } else {
        escape(text)
    }
}

/// Minimal CommonMark escaper — `\` before every ASCII punctuation
/// that can open an inline construct anywhere (always-safe: `\x`
/// for ASCII punctuation re-parses to literal `x`), plus the
/// block-marker chars that only matter at text start (`#` heading,
/// `+`/`-`/`.` list, `>` quote, `=` setext). `·` is *not* escaped —
/// it's banned from titles/values by the grammar.
pub(crate) fn escape(text: &str) -> String {
    const ANYWHERE: &[char] = &['\\', '`', '*', '_', '[', ']', '<', '&', '!'];
    const LINE_START: &[char] = &['#', '+', '-', '.', '>', '='];
    let mut out = String::with_capacity(text.len() + 8);
    for (i, c) in text.chars().enumerate() {
        if ANYWHERE.contains(&c) || (i == 0 && LINE_START.contains(&c)) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The text of a node that must be exactly one `Text` child —
/// heading names and item lines are plain by spec.
pub(crate) fn single_text(node: &Node) -> Option<&str> {
    let children = node.children()?;
    if children.len() != 1 {
        return None;
    }
    match &children[0] {
        Node::Text(t) => Some(t.value.as_str()),
        _ => None,
    }
}

/// Normalize CRLF → LF up front so spans match the stored source.
pub(crate) fn normalize(src: &str) -> String {
    if src.contains('\r') {
        src.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        src.to_string()
    }
}

/// Byte ranges in `src` covered by code — fenced/indented blocks
/// and `inline` spans — so prose scans (`arca check`'s
/// `dangling-ref`) can whitelist them. Sorted, non-overlapping.
/// Empty when `src` doesn't parse (callers scan unwhitelisted —
/// a stale ref inside a broken fence is still worth flagging).
pub fn code_spans(src: &str) -> Vec<(usize, usize)> {
    fn walk(node: &Node, out: &mut Vec<(usize, usize)>) {
        if matches!(node, Node::Code(_) | Node::InlineCode(_)) {
            if let Some(p) = node.position() {
                out.push((p.start.offset, p.end.offset));
            }
            return; // code nodes can't nest scannable prose
        }
        if let Some(children) = node.children() {
            for c in children {
                walk(c, out);
            }
        }
    }
    let mut out = Vec::new();
    if let Ok(ast) = parse(src) {
        walk(&ast, &mut out);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use markdown::mdast::Heading;

    #[test]
    fn inline_escapes_specials() {
        assert_eq!(inline("plain"), "plain");
        assert_eq!(inline("a *b* c"), "a \\*b\\* c");
        assert_eq!(inline("#hash"), "\\#hash");
        assert_eq!(
            inline("T-1 t · #label · status:open"),
            "T-1 t · #label · status:open"
        );
        assert_eq!(inline(""), "");
    }

    #[test]
    fn inline_falls_back_on_lib_corruption() {
        // mdast_util_to_markdown 0.0.2 mangles `· #_x` — verify our
        // verifier catches it and the fallback round-trips.
        let out = inline("T-001 · #_a-a");
        assert_eq!(out, "T-001 · #\\_a-a");
        let ast = parse(&format!("- {out}\n")).unwrap();
        let list = &ast.children().unwrap()[0];
        assert!(matches!(list, Node::List(_)));
        let li = &list.children().unwrap()[0];
        assert!(matches!(li, Node::ListItem(_)));
        let para = &li.children().unwrap()[0];
        assert_eq!(single_text(para), Some("T-001 · #_a-a"));
    }

    #[test]
    fn escape_covers_punct() {
        assert_eq!(
            escape("a*b_c`d[e]f<g&h!i"),
            "a\\*b\\_c\\`d\\[e\\]f\\<g\\&h\\!i"
        );
        assert_eq!(escape("#lead"), "\\#lead");
        assert_eq!(escape("mid #dash"), "mid #dash");
        assert_eq!(escape("plain, text: ok"), "plain, text: ok");
    }

    #[test]
    fn single_text_strict() {
        let h = Node::Heading(Heading {
            depth: 2,
            children: vec![Node::Text(Text {
                value: "sec".into(),
                position: None,
            })],
            position: None,
        });
        assert_eq!(single_text(&h), Some("sec"));
        let empty = Node::Heading(Heading {
            depth: 2,
            children: vec![],
            position: None,
        });
        assert_eq!(single_text(&empty), None);
        let two = Node::Paragraph(Paragraph {
            children: vec![
                Node::Text(Text {
                    value: "a".into(),
                    position: None,
                }),
                Node::Text(Text {
                    value: "b".into(),
                    position: None,
                }),
            ],
            position: None,
        });
        assert_eq!(single_text(&two), None);
        // a single non-Text child is also rejected
        let em = Node::Paragraph(Paragraph {
            children: vec![Node::Emphasis(markdown::mdast::Emphasis {
                children: vec![],
                position: None,
            })],
            position: None,
        });
        assert_eq!(single_text(&em), None);
        // root has children but no single text
        let t = Node::Text(Text {
            value: "x".into(),
            position: None,
        });
        assert_eq!(single_text(&t), None);
    }

    #[test]
    fn normalize_crlf() {
        assert_eq!(normalize("a\r\nb\rc"), "a\nb\nc");
        assert_eq!(normalize("a\nb"), "a\nb");
    }

    #[test]
    fn parse_uses_gfm_checkboxes() {
        let ast = parse("- [ ] x\n").unwrap();
        // compact Debug inlines `checked: Some(false)` on ListItem
        assert!(format!("{ast:?}").contains("checked: Some(false)"));
    }

    #[test]
    fn code_spans_cover_fences_and_inline() {
        let src = "see `T-001` here\n\n```\nT-002 in a fence\n```\n\nplain T-003\n";
        let spans = code_spans(src);
        // inline code + the fenced block, nothing else
        assert_eq!(spans.len(), 2, "{spans:?}");
        let inline = &src[spans[0].0..spans[0].1];
        assert_eq!(inline, "`T-001`", "{inline}");
        let fence = &src[spans[1].0..spans[1].1];
        assert!(fence.contains("T-002 in a fence"), "{fence}");
        // a hand-built node without position is skipped silently —
        // and unparseable input yields no spans rather than an error
        assert!(code_spans("plain text only\n").is_empty());
    }
}

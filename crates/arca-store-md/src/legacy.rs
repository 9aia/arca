//! Legacy `project/` + `pm/` import — the tolerant parse `arca
//! migrate` uses where the strict parsers reject. Everything is
//! best-effort, nothing is silently lost:
//!
//! - `- [x]` items and `· status:done` lines → counted in
//!   [`LegacyDoc::dropped`] (done items are deleted; git keeps
//!   history).
//! - Id-less bullets → [`LegacyItem::id`] `None`; the caller
//!   allocates via [`arca_store::Store::alloc_id`].
//! - Items already carrying `T-NNN`/`I-NNN`/`…` ids keep them.
//! - Inline markup (`` `code` ``, `*em*`, links) flattens to plain
//!   text — item lines are plain by spec.
//! - A paragraph directly before a list becomes a `##` section
//!   when it reads like a heading (doesn't end with `.`, `!`,
//!   `?`); other prose blocks are skipped.
//! - `·`-separated tokens the line grammar knows (`status:`,
//!   `pri:`, `owner:`, `created:`, `#label`, `blocks:`) are
//!   honored; unrecognized tails fold back into the title so no
//!   text disappears.

use arca_core::{date, is_valid_label, Actor, Item, ItemId, ItemKind, Status};
use markdown::mdast::{Node, Root};

use crate::mdx;

/// A legacy document reduced to sections + items.
#[derive(Debug, Clone, Default)]
pub struct LegacyDoc {
    /// Sections in file order (a `None` name = the run before the
    /// first heading).
    pub sections: Vec<LegacySection>,
    /// Items dropped because they were already done (`[x]`,
    /// `status:done`).
    pub dropped: usize,
}

/// A `##`-level run of legacy items.
#[derive(Debug, Clone, Default)]
pub struct LegacySection {
    /// Heading text (or a heading-like paragraph's).
    pub name: Option<String>,
    /// Items in file order.
    pub items: Vec<LegacyItem>,
}

/// One legacy bullet. `item.id` is a placeholder (`number() == 0`)
/// until the caller assigns [`LegacyItem::id`] or a fresh
/// allocation.
#[derive(Debug, Clone)]
pub struct LegacyItem {
    /// Id already present in the source — kept verbatim.
    pub id: Option<ItemId>,
    /// Parsed fields (title/status/pri/owner/labels/blocks/…);
    /// `id` inside is a placeholder.
    pub item: Item,
}

impl LegacyDoc {
    /// Every item across sections, in file order.
    pub fn items(&self) -> impl Iterator<Item = &LegacyItem> {
        self.sections.iter().flat_map(|s| s.items.iter())
    }
}

/// Parse a legacy `TODO.md`. Returns `(repo, doc)` — `repo` comes
/// from a `# TODO — <repo>`-shaped title, else `fallback`.
/// Infallible: anything markdown-rs accepts converts.
pub fn parse_legacy_todo(src: &str, fallback: &str) -> (String, LegacyDoc) {
    let conv = convert(src);
    let repo = conv
        .h1
        .as_deref()
        .and_then(parse_todo_title)
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| fallback.to_string());
    (repo, conv.doc)
}

/// Parse a legacy `BACKLOG.md` (any `#`-title accepted; the
/// canonical emit is `# Backlog` regardless).
pub fn parse_legacy_backlog(src: &str) -> LegacyDoc {
    convert(src).doc
}

/// `# TODO — <repo>` / `# TODO - <repo>` / `# TODO` → repo name.
fn parse_todo_title(h1: &str) -> Option<String> {
    let rest = h1.strip_prefix("TODO")?.trim_start();
    let rest = rest
        .strip_prefix('—')
        .or_else(|| rest.strip_prefix('-'))
        .unwrap_or(rest);
    Some(rest.trim().to_string())
}

/// Shared conversion — walks top-level blocks. `h1` is the first
/// `#` heading's text (for the repo name); `doc` is everything else.
struct Conv {
    h1: Option<String>,
    doc: LegacyDoc,
}

fn convert(src: &str) -> Conv {
    let src = mdx::normalize(src);
    let mut conv = Conv {
        h1: None,
        doc: LegacyDoc::default(),
    };
    // markdown-rs is effectively infallible for CommonMark; a
    // failure degrades to an empty doc rather than panicking
    let ast = mdx::parse(&src).unwrap_or(Node::Root(Root {
        children: Vec::new(),
        position: None,
    }));
    let blocks = ast.children().into_iter().flatten().collect::<Vec<_>>();
    for (i, block) in blocks.iter().enumerate() {
        match block {
            Node::Heading(h) => {
                let name = flat_text(block).trim().to_string();
                if h.depth == 1 && conv.h1.is_none() {
                    conv.h1 = Some(name);
                } else {
                    conv.doc.sections.push(LegacySection {
                        name: Some(name),
                        items: Vec::new(),
                    });
                }
            }
            Node::Paragraph(_) => {
                let text = flat_text(block).trim().to_string();
                // a heading-like line introducing a list → section;
                // sentence prose is skipped
                let looks_like_heading = !text.is_empty()
                    && !matches!(text.chars().last(), Some('.') | Some('!') | Some('?'));
                let followed_by_list =
                    matches!(blocks.get(i + 1), Some(Node::List(l)) if !l.ordered);
                if looks_like_heading && followed_by_list {
                    conv.doc.sections.push(LegacySection {
                        name: Some(text),
                        items: Vec::new(),
                    });
                }
            }
            Node::List(list) if !list.ordered => {
                for li in &list.children {
                    if let Node::ListItem(item) = li {
                        convert_item(&mut conv.doc, item);
                    }
                }
            }
            _ => {} // code blocks, quotes, rules, html — skipped
        }
    }
    conv
}

fn convert_item(doc: &mut LegacyDoc, li: &markdown::mdast::ListItem) {
    if li.checked == Some(true) {
        doc.dropped += 1;
        return;
    }
    let text = flat_text_children(&li.children);
    if text.is_empty() {
        return; // empty bullet — nothing to keep, nothing to count
    }
    let Some(parsed) = parse_item_text(&text) else {
        doc.dropped += 1; // status:done
        return;
    };
    if doc.sections.is_empty() {
        doc.sections.push(LegacySection::default());
    }
    doc.sections
        .last_mut()
        .expect("non-empty")
        .items
        .push(parsed);
}

/// Flatten an inline/block node to plain text — `Text`/`InlineCode`
/// values, everything else via children; soft breaks become spaces.
fn flat_text(node: &Node) -> String {
    let mut out = String::new();
    flat_into(node, &mut out);
    collapse_ws(&out)
}

fn flat_into(node: &Node, out: &mut String) {
    match node {
        Node::Text(t) => out.push_str(&t.value),
        Node::InlineCode(c) => out.push_str(&c.value),
        Node::Break(_) | Node::Delete(_) => {}
        _ => {
            if let Some(children) = node.children() {
                for c in children {
                    flat_into(c, out);
                    out.push(' ');
                }
            }
        }
    }
}

/// Text of a list item including continuation lines + nested
/// blocks, collapsed to one line.
fn flat_text_children(children: &[Node]) -> String {
    let mut out = String::new();
    for c in children {
        flat_into(c, &mut out);
        out.push(' ');
    }
    collapse_ws(&out)
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `<id>? <title> [· token]…` — tolerant: unknown tokens fold into
/// the title (`·` isn't emittable, so the separator becomes ` - `).
/// `None` ⇐ the item is `status:done`.
fn parse_item_text(text: &str) -> Option<LegacyItem> {
    // callers only hand in non-empty collapsed text
    let mut parts = text.trim().split('·');
    let head = parts.next().unwrap_or_default().trim();
    let mut head_parts = head.splitn(2, char::is_whitespace);
    let first = head_parts.next().unwrap_or_default();
    let (id, rest) = match first.parse::<ItemId>() {
        Ok(id) => (Some(id), head_parts.next().unwrap_or("")),
        Err(_) => (None, head),
    };
    let mut title_bits = vec![rest.trim().to_string()];
    let mut item = Item::new(id.unwrap_or(ItemId::new(ItemKind::Task, 0)), "");
    let mut done = false;
    for raw in parts {
        let tok = raw.trim();
        if tok.is_empty() {
            continue;
        }
        if let Some(label) = tok.strip_prefix('#') {
            if is_valid_label(label) {
                item.labels.insert(label.to_string());
                continue;
            }
            title_bits.push(tok.to_string());
            continue;
        }
        let known = tok
            .split_once(':')
            .and_then(|(k, v)| apply_token(&mut item, k, v.trim()));
        match known {
            Some(true) => {}
            Some(false) => done = true, // status:done
            None => title_bits.push(tok.to_string()),
        }
    }
    if done {
        return None;
    }
    let title = title_bits
        .into_iter()
        .filter(|b| !b.is_empty())
        .collect::<Vec<_>>()
        .join(" - ");
    // `·` can't survive a title anyway; other separators already split
    item.title = collapse_ws(&title);
    Some(LegacyItem { id, item })
}

/// `Some(true)` = applied, `Some(false)` = `status:done` (drop the
/// item), `None` = unrecognized — caller folds it into the title.
fn apply_token(item: &mut Item, key: &str, value: &str) -> Option<bool> {
    match key {
        "status" => Some(match value.parse::<Status>() {
            Ok(Status::Done) => false,
            Ok(s) => {
                item.status = s;
                true
            }
            Err(_) => return None,
        }),
        "pri" => value.parse::<arca_core::Priority>().ok().map(|p| {
            item.priority = p;
            true
        }),
        "owner" => Actor::new(value).ok().map(|a| {
            item.owner = Some(a);
            true
        }),
        "created" => date::parse(value).ok().map(|d| {
            item.created = Some(d);
            true
        }),
        "blocks" => {
            let mut ok = true;
            for bid in value.split(',') {
                match bid.trim().parse::<ItemId>() {
                    Ok(id) => {
                        item.blocks.insert(id);
                    }
                    Err(_) => ok = false,
                }
            }
            ok.then_some(true)
        }
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use arca_core::Priority;

    const DOGFOOD_TODO: &str = "# TODO — arca\n\n## v2 rewrite (phases — see proposals/v2.md)\n\n- [x] Bootstrap from bupha — v0 branch, anatomy copy, rename, prune,\n      worktrees, `xtask check` green\n- [x] `arca-core` + `arca-store-md` — models\n- [ ] `arca-git` — `init` / `check` / `migrate` / auto-commit\n- [ ] Item verbs — todo/backlog/issue/proposal/plan/adr + `item set`\n";

    #[test]
    fn todo_drops_done_and_flattens() {
        let (repo, doc) = parse_legacy_todo(DOGFOOD_TODO, "x");
        assert_eq!(repo, "arca");
        assert_eq!(doc.dropped, 2);
        assert_eq!(doc.sections.len(), 1);
        assert_eq!(
            doc.sections[0].name.as_deref(),
            Some("v2 rewrite (phases — see proposals/v2.md)")
        );
        let items: Vec<_> = doc.items().collect();
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0].item.title,
            "arca-git — init / check / migrate / auto-commit"
        );
        assert!(items[0].id.is_none());
    }

    #[test]
    fn backlog_prose_and_plain_bullets() {
        let src = "# Backlog — arca\n\nUnshaped ideas, parked here until promoted to a proposal, plan or\nissue. Newest at top.\n\n- `arca serve` — ndjson JSON-RPC surface\n- Index location — somewhere\n";
        let doc = parse_legacy_backlog(src);
        // prose paragraph ends with '.' → not a section; items unsectioned
        assert_eq!(doc.sections.len(), 1);
        assert_eq!(doc.sections[0].name, None);
        let titles: Vec<_> = doc.items().map(|i| i.item.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "arca serve — ndjson JSON-RPC surface",
                "Index location — somewhere"
            ]
        );
    }

    #[test]
    fn para_before_list_becomes_section() {
        let src =
            "# TODO\n\nPhase 1:\n\n- thing one\n- thing two\n\nPhase 2\n\n- [x] old\n- later\n";
        let (_repo, doc) = parse_legacy_todo(src, "fb");
        assert_eq!(doc.dropped, 1);
        assert_eq!(doc.sections.len(), 2);
        assert_eq!(doc.sections[0].name.as_deref(), Some("Phase 1:"));
        assert_eq!(doc.sections[0].items.len(), 2);
        assert_eq!(doc.sections[1].items.len(), 1);
    }

    #[test]
    fn keeps_existing_ids_and_tokens() {
        let src = "# TODO — x\n\n- [ ] T-012 keep me · status:in-progress · pri:high · owner:devin · #core · blocks:T-001\n- T-009 done-ish · status:done\n";
        let (_r, doc) = parse_legacy_todo(src, "x");
        assert_eq!(doc.dropped, 1);
        let item = &doc.items().next().unwrap();
        assert_eq!(item.id, Some(ItemId::new(ItemKind::Task, 12)));
        assert_eq!(item.item.status, Status::InProgress);
        assert_eq!(item.item.priority, Priority::High);
        assert_eq!(item.item.owner.as_ref().unwrap().as_str(), "devin");
        assert!(item.item.labels.contains("core"));
        assert!(item.item.blocks.contains(&ItemId::new(ItemKind::Task, 1)));
    }

    #[test]
    fn unknown_tokens_fold_into_title() {
        let src = "# TODO — x\n\n- task · note: remember this · extra words\n";
        let (_r, doc) = parse_legacy_todo(src, "x");
        let item = &doc.items().next().unwrap();
        assert_eq!(item.item.title, "task - note: remember this - extra words");
    }

    #[test]
    fn fallback_repo_and_no_title() {
        let (repo, doc) = parse_legacy_todo("- [ ] just items\n", "repo");
        assert_eq!(repo, "repo");
        assert_eq!(doc.items().count(), 1);
        // `# TODO` alone → fallback too
        let (repo, _) = parse_legacy_todo("# TODO\n\n- x\n", "fb");
        assert_eq!(repo, "fb");
    }

    #[test]
    fn nested_and_multiline_items_join() {
        let src = "# TODO — x\n\n- [ ] top line\n      continued here\n  - sub detail\n";
        let (_r, doc) = parse_legacy_todo(src, "x");
        let t = &doc.items().next().unwrap().item.title;
        assert!(
            t.contains("top line") && t.contains("continued here") && t.contains("sub detail"),
            "{t}"
        );
    }

    #[test]
    fn garbage_in_yields_empty_doc() {
        let doc = parse_legacy_backlog("");
        assert!(doc.sections.is_empty());
        let (_r, doc) = parse_legacy_todo("\u{0}binary-ish\n", "x");
        assert_eq!(doc.items().count(), 0);
    }

    #[test]
    fn empty_bullet_and_empty_token() {
        let src = "# TODO — x\n\n- \n- real ·· open\n";
        let (_r, doc) = parse_legacy_todo(src, "x");
        let titles: Vec<_> = doc.items().map(|i| i.item.title.as_str()).collect();
        assert_eq!(titles, ["real - open"]);
    }

    #[test]
    fn invalid_label_token_folds_into_title() {
        let src = "# TODO — x\n\n- t · #bad label here\n";
        let (_r, doc) = parse_legacy_todo(src, "x");
        assert_eq!(
            doc.items().next().unwrap().item.title,
            "t - #bad label here"
        );
    }

    #[test]
    fn deeper_headings_become_sections() {
        let src = "# TODO — x\n\n### deep\n\n- one\n\n# second h1\n\n- two\n";
        let (_r, doc) = parse_legacy_todo(src, "x");
        assert_eq!(doc.sections.len(), 2);
        assert_eq!(doc.sections[0].name.as_deref(), Some("deep"));
        assert_eq!(doc.sections[1].name.as_deref(), Some("second h1"));
    }

    #[test]
    fn non_item_blocks_and_inline_nodes_are_skipped() {
        // code blocks, rules and html blocks aren't sections/items
        let src = "# TODO — x\n\n```rust\nlet x = 1;\n```\n\n---\n\n<div>html</div>\n\n- one\n";
        let (_r, doc) = parse_legacy_todo(src, "x");
        let titles: Vec<_> = doc.items().map(|i| i.item.title.as_str()).collect();
        assert_eq!(titles, ["one"]);
    }

    #[test]
    fn inline_markup_flattens() {
        // `~~` isn't enabled so it stays literal; hard breaks and
        // childless inline nodes (images) drop out
        let src = "# TODO — x\n\n- a ~~gone~~ `code` ![alt](i.png) two\\\n  wrapped\n";
        let (_r, doc) = parse_legacy_todo(src, "x");
        let t = &doc.items().next().unwrap().item.title;
        assert_eq!(t, "a ~~gone~~ code two wrapped");
    }

    #[test]
    fn token_edges() {
        // `created:` is honored
        let src = "# TODO — x\n\n- T-004 t · created:2024-01-02\n";
        let (_r, doc) = parse_legacy_todo(src, "x");
        let i = doc.items().next().unwrap();
        assert!(i.item.created.is_some());
        // bad status isn't `done` — the token folds into the title
        let src = "# TODO — x\n\n- x · status:bogus\n";
        let (_r, doc) = parse_legacy_todo(src, "x");
        let i = doc.items().next().unwrap();
        assert!(i.item.title.contains("status:bogus"), "{}", i.item.title);
        // unparseable blocks entries drop the token (parsed ids
        // already landed) — it folds into the title
        let src = "# TODO — x\n\n- x · blocks:NOPE,T-001\n";
        let (_r, doc) = parse_legacy_todo(src, "x");
        let i = doc.items().next().unwrap();
        assert!(
            i.item.title.contains("blocks:NOPE,T-001"),
            "{}",
            i.item.title
        );
    }
}

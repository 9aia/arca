//! Handoff files — `handoffs/<ID>.md`, one append-only file per
//! item, uniform across all kinds (tasks can't hold this inline —
//! done tasks leave `TODO.md` — and uniformity beats special-casing):
//!
//! ```text
//! +++
//! item = "T-001"
//! +++
//!
//! # Handoffs — T-001
//!
//! ## 2026-10-29 · devin · session:cli:abc123
//! Done: store landed. Next: wire verbs. Open: archive semantics.
//! ```
//!
//! A `##` heading is `<YYYY-MM-DD> · <actor>[ · session:<id>]`;
//! everything up to the next `##` is that entry's freeform note,
//! verbatim. Files are append-only — `arca handoff` never rewrites
//! an entry; `arca check --fix` moves orphaned files (their item
//! was removed) to `handoffs/archive/`, never rewrites them either.

use arca_core::{date, is_valid_session, Actor, HandoffEntry, ItemId};
use markdown::mdast::{Node, Root};
use serde::{Deserialize, Serialize};

use crate::error::{EmitError, ParseError};
use crate::mdx;

/// The `project/` dir handoff files live in.
pub const HANDOFFS_DIR: &str = "handoffs";
/// `handoffs/archive/` — cold storage for orphaned files.
pub const HANDOFFS_ARCHIVE: &str = "archive";

/// `handoffs/T-001.md` — the filename is the canonical id.
pub fn handoff_filename(id: &ItemId) -> String {
    format!("{id}.md")
}

/// `T-001.md` → `T-001`; `None` for anything else (check reports
/// those names — the store skips them).
pub fn parse_handoff_filename(name: &str) -> Option<ItemId> {
    name.strip_suffix(".md")?.parse().ok()
}

/// Frontmatter DTO — `item = "<ID>"` is the only key; unknown keys
/// are tolerated (forward-compat, like item files).
#[derive(Debug, Deserialize)]
struct FrontmatterIn {
    item: String,
}

#[derive(Serialize)]
struct FrontmatterOut {
    item: String,
}

/// Parse `handoffs/<ID>.md` — returns the `item` id (frontmatter
/// and the `# Handoffs —` title must agree) and the entries in
/// file order. Strict: a `##` heading that doesn't fit the entry
/// grammar is an error with a span.
pub fn parse_handoff_file(
    src: &str,
    path: &str,
) -> Result<(ItemId, Vec<HandoffEntry>), ParseError> {
    let src = mdx::normalize(src);
    let at = |off: usize, len: usize, m: String| ParseError::at(path, src.clone(), off, len, m);
    let whole = |m: String| ParseError::whole_file(path, src.clone(), m);
    let mut lines = src.split_inclusive('\n');
    if lines.next().map(str::trim_end) != Some("+++") {
        return Err(whole(
            "expected `+++` TOML frontmatter fence on line 1".into(),
        ));
    }
    // find the closing `+++` line (byte offsets into src)
    let fm_start = "+++\n".len();
    let mut fm_end = None;
    let mut off = fm_start;
    for line in lines {
        if line.trim_end() == "+++" {
            fm_end = Some((off, off + line.len()));
            break;
        }
        off += line.len();
    }
    let Some((fm_end, fence_end)) = fm_end else {
        return Err(whole("unclosed `+++` frontmatter fence".into()));
    };
    let fm: FrontmatterIn = toml::from_str(&src[fm_start..fm_end]).map_err(|e| {
        let (o, l) = e
            .span()
            .map(|s| (fm_start + s.start, s.end.saturating_sub(s.start)))
            .unwrap_or((fm_start, fm_end - fm_start));
        at(o, l, e.to_string())
    })?;
    let fm_err = |m: String| at(fm_start, fm_end - fm_start, m);
    let item: ItemId = fm
        .item
        .parse()
        .map_err(|e: arca_core::ParseItemIdError| fm_err(e.to_string()))?;

    // body: mdast gives block structure + spans; offsets are into
    // `body` (i.e. relative to `fence_end` in `src`). Parsed nodes
    // always carry positions — a missing one degrades to the
    // whole-file span.
    let body = &src[fence_end..];
    let err_at = |node: &Node, m: String| {
        let (o, l) = node.position().map_or((0, body.len()), |pos| {
            (
                pos.start.offset,
                pos.end.offset.saturating_sub(pos.start.offset),
            )
        });
        at(fence_end + o, l, m)
    };
    let ast = mdx::parse(body).map_err(|m| at(fence_end, src.len() - fence_end, m))?;
    let mut blocks = ast.children().into_iter().flatten();
    let title_msg = "expected `# Handoffs — <ID>` title".to_string();
    let Some(node @ Node::Heading(h1)) = blocks.next() else {
        return Err(whole(title_msg.clone()));
    };
    if h1.depth != 1 {
        return Err(err_at(node, title_msg));
    }
    let text = mdx::single_text(node).unwrap_or_default();
    let title_id = parse_title(text)
        .and_then(|s| s.parse::<ItemId>().ok())
        .ok_or_else(|| err_at(node, title_msg.clone()))?;
    if title_id != item {
        return Err(err_at(
            node,
            format!("title id {title_id} doesn't match item = \"{item}\""),
        ));
    }

    // entries: each h2 starts one; the note is the raw markdown
    // between its heading and the next h2 (verbatim)
    let mut entries: Vec<HandoffEntry> = Vec::new();
    let mut bounds: Vec<(usize, usize)> = Vec::new(); // (h2 start, h2 end) in body
    for block in blocks {
        match block {
            Node::Heading(h) if h.depth == 2 => {
                let (start, end) = block
                    .position()
                    .map(|p| (p.start.offset, p.end.offset))
                    .unwrap_or((0, 0));
                let text = mdx::single_text(block)
                    .ok_or_else(|| err_at(block, "entry heading must be plain text".to_string()))?;
                let mut e = parse_entry_heading(text).map_err(|m| err_at(block, m))?;
                e.note = String::new();
                bounds.push((start, end));
                entries.push(e);
            }
            other => {
                if entries.is_empty() {
                    return Err(err_at(
                        other,
                        "content before the first `##` entry heading".to_string(),
                    ));
                }
                // inside the last entry's note — the raw range covers it
            }
        }
    }
    for (i, e) in entries.iter_mut().enumerate() {
        let start = bounds[i].1;
        let end = bounds.get(i + 1).map_or(body.len(), |b| b.0);
        e.note = body[start..end].trim().to_string();
    }
    Ok((item, entries))
}

/// `# Handoffs — T-001` / `Handoffs - T-1` → the id text (`None`
/// when the title isn't in that shape).
fn parse_title(h1: &str) -> Option<&str> {
    let rest = h1.strip_prefix("Handoffs")?.trim_start();
    let rest = rest.strip_prefix('—').or_else(|| rest.strip_prefix('-'))?;
    Some(rest.trim())
}

/// `<YYYY-MM-DD> · <actor>[ · session:<id>]` — the `##` heading
/// grammar. Field order is fixed; anything else is malformed.
fn parse_entry_heading(text: &str) -> Result<HandoffEntry, String> {
    let bad =
        || format!("entry heading must be `<date> · <actor>[ · session:<id>]` — got {text:?}");
    let parts: Vec<&str> = text.split(" · ").collect();
    if parts.len() < 2 || parts.len() > 3 {
        return Err(bad());
    }
    let date = date::parse(parts[0])
        .map_err(|_| format!("bad entry date {:?} — expected YYYY-MM-DD", parts[0]))?;
    let actor = Actor::new(parts[1]).map_err(|e| e.to_string())?;
    let session = match parts.get(2) {
        None => None,
        Some(p) => {
            let s = p
                .strip_prefix("session:")
                .filter(|s| is_valid_session(s))
                .ok_or_else(|| format!("bad session field {p:?} — expected session:<id>"))?;
            Some(s.to_string())
        }
    };
    Ok(HandoffEntry {
        date,
        actor,
        session,
        note: String::new(),
    })
}

/// One `## <date> · <actor>[ · session:<id>]` block + note —
/// `append_handoff` writes this verbatim. The note may not contain
/// a `##` heading (it would read back as the next entry).
pub fn emit_entry(entry: &HandoffEntry) -> Result<String, EmitError> {
    let err = |m: &str| EmitError::new("handoff entry", m);
    if let Some(s) = &entry.session {
        if !is_valid_session(s) {
            return Err(err("session id can't be empty or contain `·`/newline"));
        }
    }
    // markdown-rs is effectively infallible for CommonMark — an
    // unparseable note can't hide a `##` heading, so emit proceeds
    let ast = mdx::parse(&entry.note).unwrap_or(Node::Root(Root {
        children: Vec::new(),
        position: None,
    }));
    if ast
        .children()
        .into_iter()
        .flatten()
        .any(|n| matches!(n, Node::Heading(h) if h.depth == 2))
    {
        return Err(err("note can't contain a `##` heading"));
    }
    let mut head = format!("{} · {}", date::render(entry.date), entry.actor);
    if let Some(s) = &entry.session {
        head.push_str(&format!(" · session:{s}"));
    }
    // `_`/`*` are legal actor/session chars — escape inline markup
    // or the heading wouldn't read back as a single text run
    let head = mdx::inline(&head);
    let note = entry.note.trim();
    if note.is_empty() {
        return Ok(format!("## {head}\n"));
    }
    Ok(format!("## {head}\n{note}\n"))
}

/// Canonical `handoffs/<ID>.md` — header + entries in file order.
pub fn emit_handoff_file(id: &ItemId, entries: &[HandoffEntry]) -> Result<String, EmitError> {
    let fm = FrontmatterOut {
        item: id.to_string(),
    };
    let toml = toml::to_string(&fm).map_err(|e| EmitError::new("handoff file", e.to_string()))?;
    let mut out = String::from("+++\n");
    out.push_str(&toml);
    out.push_str("+++\n\n# Handoffs — ");
    out.push_str(&id.to_string());
    out.push('\n');
    for e in entries {
        out.push('\n');
        out.push_str(&emit_entry(e)?);
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use arca_core::ItemKind;

    const FILE: &str = "+++\nitem = \"T-001\"\n+++\n\n# Handoffs — T-001\n\n## 2026-10-29 · devin · session:cli:abc123\nDone: store landed. Next: wire verbs. Open: archive semantics.\n\n## 2026-10-30 · human:luis\nSecond note.\n";

    fn entry() -> HandoffEntry {
        HandoffEntry {
            date: date::parse("2026-10-29").unwrap(),
            actor: Actor::new("devin").unwrap(),
            session: Some("cli:abc123".into()),
            note: "Done: x. Next: y.".into(),
        }
    }

    #[test]
    fn filename_roundtrip() {
        assert_eq!(
            handoff_filename(&ItemId::new(ItemKind::Task, 1)),
            "T-001.md"
        );
        assert_eq!(
            handoff_filename(&ItemId::new(ItemKind::Adr, 4)),
            "ADR-0004.md"
        );
        assert_eq!(
            parse_handoff_filename("T-001.md"),
            Some(ItemId::new(ItemKind::Task, 1))
        );
        // non-canonical width still parses (check renames it)
        assert_eq!(
            parse_handoff_filename("T-1.md"),
            Some(ItemId::new(ItemKind::Task, 1))
        );
        for bad in ["T-001.txt", "T-001", "x.md", ".md", "archive"] {
            assert_eq!(parse_handoff_filename(bad), None, "{bad}");
        }
    }

    #[test]
    fn parse_basic() {
        let (id, entries) = parse_handoff_file(FILE, "handoffs/T-001.md").unwrap();
        assert_eq!(id, ItemId::new(ItemKind::Task, 1));
        assert_eq!(entries.len(), 2);
        let e = &entries[0];
        assert_eq!(e.date, date::parse("2026-10-29").unwrap());
        assert_eq!(e.actor.as_str(), "devin");
        assert_eq!(e.session.as_deref(), Some("cli:abc123"));
        assert_eq!(
            e.note,
            "Done: store landed. Next: wire verbs. Open: archive semantics."
        );
        assert_eq!(entries[1].session, None);
        assert_eq!(entries[1].note, "Second note.");
        // emit is a fixed point
        assert_eq!(emit_handoff_file(&id, &entries).unwrap(), FILE);
    }

    #[test]
    fn parse_multiline_and_blankline_notes() {
        let src = "+++\nitem = \"I-002\"\n+++\n\n# Handoffs — I-002\n\n## 2026-01-01 · devin\nline one\nline two\n\n- a\n- b\n";
        let (id, entries) = parse_handoff_file(src, "handoffs/I-002.md").unwrap();
        assert_eq!(id, ItemId::new(ItemKind::Issue, 2));
        assert_eq!(entries[0].note, "line one\nline two\n\n- a\n- b");
        assert_eq!(emit_handoff_file(&id, &entries).unwrap(), src);
    }

    #[test]
    fn emit_entry_variants() {
        assert_eq!(
            emit_entry(&entry()).unwrap(),
            "## 2026-10-29 · devin · session:cli:abc123\nDone: x. Next: y.\n"
        );
        let mut bare = entry();
        bare.session = None;
        bare.note = String::new();
        assert_eq!(emit_entry(&bare).unwrap(), "## 2026-10-29 · devin\n");
        // bad session + h2 in the note are emit errors
        let mut e = entry();
        e.session = Some("a·b".into());
        assert!(emit_entry(&e).is_err());
        let mut e = entry();
        e.session = Some("two\nlines".into());
        assert!(emit_entry(&e).is_err());
        let mut e = entry();
        e.note = "text\n\n## sneaky\n".into();
        assert!(emit_entry(&e).is_err());
        // `#`/list content in the note is fine
        let mut e = entry();
        e.note = "# ok heading\n\n- item\n\n### deeper ok\n".into();
        assert!(emit_entry(&e).is_ok());
    }

    #[test]
    fn parse_errors() {
        for (src, why) in [
            ("no fence\n", "no +++"),
            ("+++\nitem = \"T-001\"\n", "unclosed"),
            ("+++\n+++\n\n# Handoffs — T-001\n", "missing item key"),
            ("+++\nitem = \"X-001\"\n+++\n\n# Handoffs — T-001\n", "bad id"),
            ("+++\nitem = \"T-001\"\n+++\n\n# Handoffs — T-002\n", "title mismatch"),
            ("+++\nitem = \"T-001\"\n+++\n\n## 2026-01-01 · devin\nnote\n", "no title"),
            ("+++\nitem = \"T-001\"\n+++\n", "empty body"),
            ("+++\nitem = \"T-001\"\n+++\n\njust a paragraph\n", "non-heading block"),
            ("+++\nitem = \"T-001\"\n+++\n\n# Handoffs — T-001\n\nstray\n", "content before entry"),
            ("+++\nitem = \"T-001\"\n+++\n\n# Handoffs — T-001\n\n## not a date · devin\nn\n", "bad date"),
            ("+++\nitem = \"T-001\"\n+++\n\n# Handoffs — T-001\n\n## 2026-01-01\ntext\n", "one field"),
            ("+++\nitem = \"T-001\"\n+++\n\n# Handoffs — T-001\n\n## 2026-01-01 · devin · bogus:x\nn\n", "bad session"),
            ("+++\nitem = \"T-001\"\n+++\n\n# Handoffs — T-001\n\n## 2026-01-01 · a b c\nn\n", "bad actor"),
            ("+++\nitem = \"T-001\"\n+++\n\n# Handoffs — T-001\n\n## 2026-01-01 · devin · session:x · extra\nn\n", "too many fields"),
        ] {
            assert!(
                parse_handoff_file(src, "handoffs/T-001.md").is_err(),
                "{why}: {src:?}"
            );
        }
        // deeper headings inside a note are fine
        let src = "+++\nitem = \"T-001\"\n+++\n\n# Handoffs — T-001\n\n## 2026-01-01 · devin\n### sub\nnote\n";
        let (_, entries) = parse_handoff_file(src, "handoffs/T-001.md").unwrap();
        assert_eq!(entries[0].note, "### sub\nnote");
        // a `##` inside a fenced code block is not an entry
        let src = "+++\nitem = \"T-001\"\n+++\n\n# Handoffs — T-001\n\n## 2026-01-01 · devin\n```\n## not an entry\n```\n";
        let (_, entries) = parse_handoff_file(src, "handoffs/T-001.md").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].note, "```\n## not an entry\n```");
    }
}

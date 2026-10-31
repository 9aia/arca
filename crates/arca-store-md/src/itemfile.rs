//! Item files — one md file per issue/proposal/adr/plan:
//!
//! ```text
//! issues/issue-NNNN-slug.md    proposals/proposal-NNNN-slug.md
//! adrs/NNNN-slug.md            plans/pl-NNNN-slug.md
//! ```
//!
//! `NNNN` = id number padded to 4 digits. TOML frontmatter in
//! `+++` fences (id, title, status, created, owner, priority,
//! labels, blocks, note, superseded_by, supersedes, epic,
//! milestone — extendable: unknown keys are ignored on read for
//! forward-compat); body = free-form markdown (verbatim; kind
//! templates are `arca-template`'s job).

use arca_core::{date, Actor, Item, ItemFile, ItemId, ItemKind, Priority, Status};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use crate::error::{EmitError, ParseError};
use crate::mdx;

/// The `project/` subdirectory a file-backed kind lives in.
/// [`ItemKind::Task`] has no files (`None`).
pub fn item_dir(kind: ItemKind) -> Option<&'static str> {
    match kind {
        ItemKind::Task => None,
        ItemKind::Issue => Some("issues"),
        ItemKind::Proposal => Some("proposals"),
        ItemKind::Adr => Some("adrs"),
        ItemKind::Plan => Some("plans"),
    }
}

/// Filename prefix before the number — `issue`, `proposal`, `pl`,
/// or empty for ADRs (`adrs/0004-slug.md`).
fn file_prefix(kind: ItemKind) -> Option<&'static str> {
    match kind {
        ItemKind::Task => None,
        ItemKind::Issue => Some("issue"),
        ItemKind::Proposal => Some("proposal"),
        ItemKind::Adr => Some(""),
        ItemKind::Plan => Some("pl"),
    }
}

/// Canonical filename for an item — `issue-0003-memory-leak.md`,
/// `0004-hybrid-store.md`. Slug = `slug::slugify(title)`,
/// `"untitled"` when the title slugifies to nothing.
pub fn item_filename(id: &ItemId, title: &str) -> Option<String> {
    let prefix = file_prefix(id.kind())?;
    let slug = {
        let s = slug::slugify(title);
        if s.is_empty() {
            "untitled".to_string()
        } else {
            s
        }
    };
    Some(if prefix.is_empty() {
        format!("{:04}-{slug}.md", id.number())
    } else {
        format!("{prefix}-{:04}-{slug}.md", id.number())
    })
}

/// `issues/issue-0003-x.md` → `I-003`; `None` when the filename
/// doesn't fit the kind's pattern.
pub fn parse_item_filename(kind: ItemKind, filename: &str) -> Option<ItemId> {
    let prefix = file_prefix(kind)?;
    let stem = filename.strip_suffix(".md")?;
    let rest = if prefix.is_empty() {
        stem
    } else {
        stem.strip_prefix(prefix)?.strip_prefix('-')?
    };
    let (digits, slug) = rest.split_once('-')?;
    if digits.is_empty()
        || digits.len() > 9
        || !digits.bytes().all(|b| b.is_ascii_digit())
        || slug.is_empty()
    {
        return None;
    }
    let number: u32 = digits.parse().ok()?;
    if number == 0 {
        return None;
    }
    Some(ItemId::new(kind, number))
}

/// `id = "…"` pulled out of a `+++` frontmatter block — the cheap
/// probe for when the strict parse isn't needed (or already
/// failed). `None` for files with no `+++` fence or no parseable
/// `id` line — including frontmatter-less files that merely
/// *borrow* an id number via their filename.
pub fn frontmatter_id(src: &str) -> Option<ItemId> {
    let mut lines = src.lines();
    if lines.next()?.trim() != "+++" {
        return None;
    }
    for l in lines {
        let t = l.trim();
        if t == "+++" {
            break;
        }
        if let Some(rest) = t.strip_prefix("id") {
            if let Some(v) = rest.trim_start().strip_prefix('=') {
                if let Ok(id) = v.trim().trim_matches('"').parse::<ItemId>() {
                    return Some(id);
                }
            }
        }
    }
    None
}

/// Frontmatter DTO — stringly-typed so bad values point at the
/// frontmatter block, not toml's internals.
#[derive(Debug, Deserialize)]
struct FrontmatterIn {
    id: String,
    title: String,
    status: Status,
    created: Option<String>,
    owner: Option<String>,
    #[serde(default)]
    priority: Option<Priority>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    blocks: Vec<String>,
    note: Option<String>,
    superseded_by: Option<String>,
    supersedes: Option<String>,
    epic: Option<String>,
    milestone: Option<String>,
}

/// Emit shape — field order IS the frontmatter key order.
#[derive(Serialize)]
struct FrontmatterOut<'a> {
    id: String,
    title: &'a str,
    status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    created: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
    priority: Priority,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    labels: Vec<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    blocks: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    superseded_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    supersedes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    epic: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    milestone: Option<&'a str>,
}

/// Parse an item file's source. `expected_kind` comes from the
/// directory the file was found in — a mismatch (`P-002` inside
/// `issues/`) is an error.
pub fn parse_item_file(
    src: &str,
    expected_kind: ItemKind,
    path: &str,
) -> Result<ItemFile, ParseError> {
    let src = mdx::normalize(src);
    let at = |off: usize, len: usize, m: String| ParseError::at(path, src.clone(), off, len, m);
    let whole = |m: &str| ParseError::whole_file(path, src.clone(), m);
    let mut lines = src.split_inclusive('\n');
    if lines.next().map(str::trim_end) != Some("+++") {
        return Err(whole("expected `+++` TOML frontmatter fence on line 1"));
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
        return Err(whole("unclosed `+++` frontmatter fence"));
    };
    let fm_src = &src[fm_start..fm_end];
    let fm: FrontmatterIn = toml::from_str(fm_src).map_err(|e| {
        let (o, l) = e
            .span()
            .map(|s| (fm_start + s.start, s.end.saturating_sub(s.start)))
            .unwrap_or((fm_start, fm_end - fm_start));
        at(o, l, e.to_string())
    })?;
    // DTO → Item; value errors point at the whole frontmatter block.
    let fm_err = |m: String| at(fm_start, fm_end - fm_start, m);
    let id: ItemId = fm
        .id
        .parse()
        .map_err(|e: arca_core::ParseItemIdError| fm_err(e.to_string()))?;
    if id.kind() != expected_kind {
        return Err(fm_err(format!(
            "id {id} doesn't belong in this file (expected a {} id)",
            expected_kind.prefix()
        )));
    }
    let owner = fm
        .owner
        .map(|o| Actor::new(&o).map_err(|e| fm_err(e.to_string())))
        .transpose()?;
    let created = fm
        .created
        .map(|c| date::parse(&c).map_err(|e| fm_err(format!("bad created date — {e}"))))
        .transpose()?;
    let mut labels = BTreeSet::new();
    for l in fm.labels {
        if !arca_core::is_valid_label(&l) {
            return Err(fm_err(format!("invalid label {l:?}")));
        }
        labels.insert(l);
    }
    let mut blocks = BTreeSet::new();
    for b in fm.blocks {
        blocks.insert(
            b.parse::<ItemId>()
                .map_err(|e: arca_core::ParseItemIdError| fm_err(e.to_string()))?,
        );
    }
    let mut item = Item::new(id, fm.title);
    item.status = fm.status;
    item.priority = fm.priority.unwrap_or_default();
    item.owner = owner;
    item.labels = labels;
    item.blocks = blocks;
    item.created = created;
    item.note = fm.note;
    item.superseded_by = fm
        .superseded_by
        .map(|s| {
            s.parse::<ItemId>()
                .map_err(|e: arca_core::ParseItemIdError| fm_err(e.to_string()))
        })
        .transpose()?;
    item.supersedes = fm
        .supersedes
        .map(|s| {
            s.parse::<ItemId>()
                .map_err(|e: arca_core::ParseItemIdError| fm_err(e.to_string()))
        })
        .transpose()?;
    for (key, name) in [("epic", &fm.epic), ("milestone", &fm.milestone)] {
        if let Some(name) = name {
            if !arca_core::is_valid_group_name(name) {
                return Err(fm_err(format!("invalid {key} name {name:?}")));
            }
        }
    }
    item.epic = fm.epic;
    item.milestone = fm.milestone;
    // body: everything after the closing fence + one blank line
    let mut body = &src[fence_end..];
    body = body.strip_prefix('\n').unwrap_or(body);
    let file = ItemFile {
        meta: item,
        body: body.trim().to_string(),
    };
    // parse-safe: the body must be markdown the parser accepts
    if !file.body.is_empty() {
        mdx::parse(&file.body).map_err(|m| at(fence_end, src.len() - fence_end, m))?;
    }
    Ok(file)
}

/// Canonical item file — `+++\n<toml>+++\n\n<body>\n`.
pub fn emit_item_file(file: &ItemFile) -> Result<String, EmitError> {
    let fm = FrontmatterOut {
        id: file.meta.id.to_string(),
        title: &file.meta.title,
        status: file.meta.status,
        created: file.meta.created.map(date::render),
        owner: file.meta.owner.as_ref().map(ToString::to_string),
        priority: file.meta.priority,
        labels: file.meta.labels.iter().map(String::as_str).collect(),
        blocks: file.meta.blocks.iter().map(ItemId::to_string).collect(),
        note: file.meta.note.as_deref(),
        superseded_by: file.meta.superseded_by.map(|i| i.to_string()),
        supersedes: file.meta.supersedes.map(|i| i.to_string()),
        epic: file.meta.epic.as_deref(),
        milestone: file.meta.milestone.as_deref(),
    };
    let toml = toml::to_string(&fm).map_err(|e| EmitError::new("item file", e.to_string()))?;
    let mut out = String::from("+++\n");
    out.push_str(&toml);
    out.push_str("+++\n");
    if !file.body.is_empty() {
        out.push('\n');
        out.push_str(file.body.trim_end());
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const ISSUE: &str = "+++\nid = \"I-003\"\ntitle = \"Memory leak\"\nstatus = \"in-progress\"\ncreated = \"2026-02-01\"\nowner = \"devin\"\npriority = \"high\"\nlabels = [\"index\", \"perf\"]\nblocks = [\"T-010\"]\nnote = \"resumed\"\n+++\n\n## Description\n\nBody text.\n";

    #[test]
    fn filename_mapping() {
        assert_eq!(
            item_filename(&ItemId::new(ItemKind::Issue, 3), "Memory leak!"),
            Some("issue-0003-memory-leak.md".into())
        );
        assert_eq!(
            item_filename(&ItemId::new(ItemKind::Adr, 4), "Hybrid store"),
            Some("0004-hybrid-store.md".into())
        );
        assert_eq!(
            item_filename(&ItemId::new(ItemKind::Plan, 2), "v2 phases"),
            Some("pl-0002-v2-phases.md".into())
        );
        assert_eq!(
            item_filename(&ItemId::new(ItemKind::Proposal, 7), ""),
            Some("proposal-0007-untitled.md".into())
        );
        assert_eq!(item_filename(&ItemId::new(ItemKind::Task, 1), "x"), None);
    }

    #[test]
    fn filename_parse() {
        assert_eq!(
            parse_item_filename(ItemKind::Issue, "issue-0003-memory-leak.md"),
            Some(ItemId::new(ItemKind::Issue, 3))
        );
        assert_eq!(
            parse_item_filename(ItemKind::Adr, "0004-hybrid.md"),
            Some(ItemId::new(ItemKind::Adr, 4))
        );
        assert_eq!(
            parse_item_filename(ItemKind::Plan, "pl-0002-x.md"),
            Some(ItemId::new(ItemKind::Plan, 2))
        );
        for (kind, name) in [
            (ItemKind::Task, "issue-0003-x.md"),
            (ItemKind::Issue, "issue-x-y.md"),
            (ItemKind::Issue, "issue-0003.md"),
            (ItemKind::Issue, "issue-0000-x.md"),
            (ItemKind::Issue, "issue-0003-.md"),
            (ItemKind::Issue, "issue-0003-x.txt"),
            (ItemKind::Issue, "proposal-0003-x.md"),
            (ItemKind::Adr, "adr-0004-x.md"),
            (ItemKind::Adr, "0004.md"),
            (ItemKind::Issue, "issue-0003x-y.md"),
        ] {
            assert_eq!(parse_item_filename(kind, name), None, "{name}");
        }
    }

    #[test]
    fn roundtrip() {
        let file =
            parse_item_file(ISSUE, ItemKind::Issue, "issues/issue-0003-memory-leak.md").unwrap();
        assert_eq!(file.meta.id, ItemId::new(ItemKind::Issue, 3));
        assert_eq!(file.meta.status, Status::InProgress);
        assert_eq!(file.meta.priority, Priority::High);
        assert_eq!(file.meta.owner.as_ref().unwrap().as_str(), "devin");
        assert_eq!(file.meta.note.as_deref(), Some("resumed"));
        assert!(file.meta.labels.contains("perf"));
        assert!(file.meta.blocks.contains(&ItemId::new(ItemKind::Task, 10)));
        assert_eq!(file.body, "## Description\n\nBody text.");
        assert_eq!(emit_item_file(&file).unwrap(), ISSUE);
    }

    #[test]
    fn supersede_keys_roundtrip() {
        let src = "+++\nid = \"ADR-0002\"\ntitle = \"old\"\nstatus = \"done\"\npriority = \"medium\"\nsuperseded_by = \"ADR-0003\"\nsupersedes = \"ADR-0001\"\n+++\n";
        let file = parse_item_file(src, ItemKind::Adr, "adrs/0002-old.md").unwrap();
        assert_eq!(file.meta.superseded_by, Some(ItemId::new(ItemKind::Adr, 3)));
        assert_eq!(file.meta.supersedes, Some(ItemId::new(ItemKind::Adr, 1)));
        assert_eq!(emit_item_file(&file).unwrap(), src);
        // bad id values are frontmatter errors
        let bad = "+++\nid = \"ADR-0001\"\ntitle = \"x\"\nstatus = \"open\"\nsuperseded_by = \"nope\"\n+++\n";
        assert!(parse_item_file(bad, ItemKind::Adr, "x.md").is_err());
    }

    #[test]
    fn epic_milestone_roundtrip() {
        let src = "+++\nid = \"I-001\"\ntitle = \"x\"\nstatus = \"open\"\npriority = \"medium\"\nepic = \"v2\"\nmilestone = \"1.0\"\n+++\n";
        let file = parse_item_file(src, ItemKind::Issue, "issues/issue-0001-x.md").unwrap();
        assert_eq!(file.meta.epic.as_deref(), Some("v2"));
        assert_eq!(file.meta.milestone.as_deref(), Some("1.0"));
        assert_eq!(emit_item_file(&file).unwrap(), src);
        // invalid group names are frontmatter errors
        for bad in ["epic = \"has space\"", "milestone = \"a/b\""] {
            let s = format!("+++\nid = \"I-001\"\ntitle = \"x\"\nstatus = \"open\"\n{bad}\n+++\n");
            assert!(
                parse_item_file(&s, ItemKind::Issue, "x.md").is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn minimal_frontmatter() {
        let src = "+++\nid = \"P-002\"\ntitle = \"t\"\nstatus = \"open\"\n+++\n";
        let file = parse_item_file(src, ItemKind::Proposal, "proposals/x.md").unwrap();
        assert_eq!(file.meta.priority, Priority::Medium);
        assert!(file.body.is_empty());
        let out = emit_item_file(&file).unwrap();
        assert!(out.contains("priority = \"medium\""));
        assert!(!out.contains("owner"));
        assert_eq!(
            parse_item_file(&out, ItemKind::Proposal, "p.md").unwrap(),
            file
        );
    }

    #[test]
    fn parse_errors() {
        for (src, why) in [
            ("no fence\n", "no +++"),
            ("+++\nid = \"I-001\"\n", "unclosed"),
            ("+++\n+++\n", "missing keys"),
            (
                "+++\nid = \"x\"\ntitle = \"t\"\nstatus = \"open\"\n+++\n",
                "bad id",
            ),
            (
                "+++\nid = \"I-001\"\ntitle = \"t\"\nstatus = \"bogus\"\n+++\n",
                "bad status",
            ),
            (
                "+++\nid = \"I-001\"\ntitle = \"t\"\nstatus = \"open\"\nowner = \"a b\"\n+++\n",
                "bad actor",
            ),
            (
                "+++\nid = \"I-001\"\ntitle = \"t\"\nstatus = \"open\"\ncreated = \"soon\"\n+++\n",
                "bad date",
            ),
            (
                "+++\nid = \"I-001\"\ntitle = \"t\"\nstatus = \"open\"\nlabels = [\"a b\"]\n+++\n",
                "bad label",
            ),
            (
                "+++\nid = \"I-001\"\ntitle = \"t\"\nstatus = \"open\"\nblocks = [\"x\"]\n+++\n",
                "bad blocks",
            ),
            (
                "+++\nid = \"P-001\"\ntitle = \"t\"\nstatus = \"open\"\n+++\n",
                "wrong kind",
            ),
            (
                "+++\nid = \"I-001\"\ntitle = \"t\"\nstatus = \"open\"\nwho = \"?\"\n+++\n",
                "unknown key ok?",
            ),
        ] {
            let r = parse_item_file(src, ItemKind::Issue, "issues/x.md");
            assert!(r.is_err() == (why != "unknown key ok?"), "{why}: {src:?}");
        }
    }

    #[test]
    fn unknown_keys_tolerated() {
        // forward-compat: files written by a newer arca still load
        let src =
            "+++\nid = \"I-001\"\ntitle = \"t\"\nstatus = \"open\"\nfuture = 42\n+++\n\nbody\n";
        let file = parse_item_file(src, ItemKind::Issue, "issues/x.md").unwrap();
        assert_eq!(file.body, "body");
    }

    #[test]
    fn crlf_body_and_crlf_file() {
        let src = ISSUE.replace('\n', "\r\n");
        let file = parse_item_file(&src, ItemKind::Issue, "x.md").unwrap();
        assert_eq!(file.body, "## Description\n\nBody text.");
    }
}

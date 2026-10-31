//! Item-file body templates — embedded `format!` strings, not a
//! templating engine. Four kinds, four static skeletons; pulling in
//! `minijinja` + a `arca-template` crate would add a dependency for
//! nothing these can't do. Revisit when templates grow variants
//! (epics, milestones).

use arca_core::{date, Item};

fn stamp(item: &Item, label: &str) -> String {
    let created = item.created.map(date::render).unwrap_or_default();
    format!("Status: **{label}** · Created: {created}")
}

/// `issues/` body — mirrors the v0 skill template: Status /
/// Description / Checklist / Resolution.
pub fn issue(item: &Item) -> String {
    format!(
        "# {title}\n\n{status}\n\n\
         ## Description\n\n\
         What is wrong and steps to reproduce.\n\n\
         ## Checklist\n\n\
         - [ ] Action item\n\n\
         ## Resolution\n",
        title = item.title,
        status = stamp(item, "open"),
    )
}

/// `proposals/` body — goal / requirements / design / questions.
pub fn proposal(item: &Item) -> String {
    format!(
        "# {title}\n\n{status}\n\n\
         ## Goal & Overview\n\n\n\n\
         ## Requirements\n\n\n\n\
         ## Technical Design\n\n\n\n\
         ## Open questions\n",
        title = item.title,
        status = stamp(item, "draft"),
    )
}

/// `plans/` body — a phase checklist.
pub fn plan(item: &Item) -> String {
    format!(
        "# {title}\n\n{status}\n\n\
         ## Phases\n\n\
         - [ ] Phase 1 — \n\
         - [ ] Phase 2 — \n",
        title = item.title,
        status = stamp(item, "open"),
    )
}

/// `adrs/` body — context / decision / consequences.
pub fn adr(item: &Item) -> String {
    format!(
        "# {title}\n\n{status}\n\n\
         ## Context\n\n\n\n\
         ## Decision\n\n\n\n\
         ## Consequences\n",
        title = item.title,
        status = stamp(item, "proposed"),
    )
}

/// Append `text` at the end of the `## <heading>` section in
/// `body`; the heading is created at EOF when missing. Used by
/// `issue resolve` (Resolution) and `issue comment` (Comments).
pub fn append_to_section(body: &mut String, heading: &str, text: &str) {
    let lines: Vec<&str> = body.lines().collect();
    if let Some(hi) = lines.iter().position(|l| l.trim() == heading) {
        // the section runs until the next heading or EOF
        let end = lines
            .iter()
            .enumerate()
            .skip(hi + 1)
            .find(|(_, l)| l.trim_start().starts_with('#'))
            .map(|(i, _)| i)
            .unwrap_or(lines.len());
        let mut head: Vec<&str> = lines[..end].to_vec();
        while head.last().is_some_and(|l| l.trim().is_empty()) {
            head.pop();
        }
        let mut out = head.join("\n");
        out.push_str("\n\n");
        out.push_str(text.trim_end());
        let tail = &lines[end..];
        if !tail.is_empty() {
            out.push_str("\n\n");
            out.push_str(&tail.join("\n"));
        }
        out.push('\n');
        *body = out;
    } else {
        let mut out = body.trim_end().to_string();
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(heading);
        out.push_str("\n\n");
        out.push_str(text.trim_end());
        out.push('\n');
        *body = out;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use arca_core::{ItemId, ItemKind};

    fn item() -> Item {
        Item::new(ItemId::new(ItemKind::Issue, 1), "the bug")
            .created(date::parse("2026-02-09").unwrap())
    }

    #[test]
    fn templates_have_sections() {
        let i = item();
        assert!(issue(&i).contains("## Resolution"));
        assert!(issue(&i).contains("Status: **open** · Created: 2026-02-09"));
        for (t, secs) in [
            (
                proposal(&i),
                vec![
                    "Goal & Overview",
                    "Requirements",
                    "Technical Design",
                    "Open questions",
                ],
            ),
            (adr(&i), vec!["Context", "Decision", "Consequences"]),
        ] {
            for s in secs {
                assert!(t.contains(&format!("## {s}")), "{s}");
            }
        }
        let p = plan(&i);
        assert!(p.contains("- [ ] Phase 1"));
    }

    #[test]
    fn append_to_existing_section() {
        let mut body = issue(&item());
        append_to_section(&mut body, "## Resolution", "fixed in abc123");
        assert!(body.contains("## Resolution\n\nfixed in abc123"), "{body}");
        // second append lands after the first
        append_to_section(&mut body, "## Resolution", "verified");
        assert!(body.contains("fixed in abc123\n\nverified"), "{body}");
    }

    #[test]
    fn append_creates_missing_section() {
        let mut body = String::from("# x\n\nbody\n");
        append_to_section(&mut body, "## Comments", "- 2026-01-01 hi");
        assert_eq!(body, "# x\n\nbody\n\n## Comments\n\n- 2026-01-01 hi\n");
        // empty body
        let mut empty = String::new();
        append_to_section(&mut empty, "## Comments", "- d t");
        assert_eq!(empty, "## Comments\n\n- d t\n");
    }

    #[test]
    fn append_before_next_heading() {
        let mut body = String::from("## A\n\none\n\n## B\n\ntwo\n");
        append_to_section(&mut body, "## A", "added");
        assert_eq!(body, "## A\n\none\n\nadded\n\n## B\n\ntwo\n");
    }
}

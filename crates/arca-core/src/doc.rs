//! Document models — the in-memory shapes `arca-store-md` parses
//! `project/` files into and emits back from. `sections` preserve
//! file order; a `name: None` section is the unsectioned run before
//! the first `##` heading (only legal at index 0).

use crate::Item;

/// A named run of items under a `## <section>` heading.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Section {
    /// Heading text; `None` = items before the first `##`.
    pub name: Option<String>,
    /// Items in file order.
    pub items: Vec<Item>,
}

impl Section {
    /// A named section with no items yet.
    pub fn named(name: impl Into<String>) -> Self {
        Section {
            name: Some(name.into()),
            items: Vec::new(),
        }
    }

    /// The unsectioned run (name `None`) with no items yet.
    pub fn unsectioned() -> Self {
        Section::default()
    }
}

/// `TODO.md` — `# TODO — <repo>` plus `## <section>` runs of
/// `- [ ] T-NNN …` task items.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct TodoDoc {
    /// Repo name from the `# TODO — <repo>` title.
    pub repo: String,
    /// Sections in file order.
    pub sections: Vec<Section>,
}

impl TodoDoc {
    /// Fresh doc for `arca init` — title only.
    pub fn new(repo: impl Into<String>) -> Self {
        TodoDoc {
            repo: repo.into(),
            sections: Vec::new(),
        }
    }

    /// Every top-level item across all sections, in file order —
    /// nested children stay inside their parents (flatten with
    /// [`Item::subtree`]).
    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.sections.iter().flat_map(|s| s.items.iter())
    }
}

/// `BACKLOG.md` — `# Backlog` plus optional `##` sections of
/// `- T-NNN …` items (no checkbox; items are `Deferred`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct BacklogDoc {
    /// Sections in file order.
    pub sections: Vec<Section>,
}

impl BacklogDoc {
    /// Every top-level item across all sections, in file order —
    /// nested children stay inside their parents (flatten with
    /// [`Item::subtree`]).
    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.sections.iter().flat_map(|s| s.items.iter())
    }
}

/// One `issues|proposals|adrs|plans/*.md` file — TOML frontmatter
/// in `+++` fences + a free-form markdown body.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct ItemFile {
    /// Frontmatter (`id`, `title`, `status`, `created`, `owner`,
    /// `priority`, `labels`, `blocks`, `note`).
    pub meta: Item,
    /// Markdown body, verbatim (normalized: trimmed). Kind-specific
    /// section templates are `arca-template`'s job, not the store's.
    pub body: String,
}

impl ItemFile {
    /// New file with an empty body.
    pub fn new(meta: Item) -> Self {
        ItemFile {
            meta,
            body: String::new(),
        }
    }

    /// Builder-style body setter — the body is stored trimmed.
    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = body.into().trim().to_string();
        self
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{ItemId, ItemKind};

    fn t(n: u32) -> Item {
        Item::new(ItemId::new(ItemKind::Task, n), format!("t{n}"))
    }

    #[test]
    fn section_ctors() {
        assert_eq!(Section::named("Now").name.as_deref(), Some("Now"));
        assert_eq!(Section::unsectioned().name, None);
    }

    #[test]
    fn doc_items_iterate_in_order() {
        let mut todo = TodoDoc::new("arca");
        todo.sections.push(Section {
            name: Some("a".into()),
            items: vec![t(1), t(2)],
        });
        todo.sections.push(Section {
            name: None,
            items: vec![t(3)],
        });
        let ids: Vec<u32> = todo.items().map(|i| i.id.number()).collect();
        assert_eq!(ids, [1, 2, 3]);

        let mut bl = BacklogDoc::default();
        bl.sections.push(Section {
            name: Some("someday".into()),
            items: vec![t(9)],
        });
        assert_eq!(bl.items().count(), 1);
    }

    #[test]
    fn item_file_body_trims() {
        let f = ItemFile::new(t(1)).body("\n\n  hello \n");
        assert_eq!(f.body, "hello");
        assert_eq!(ItemFile::new(t(1)).body, "");
    }
}

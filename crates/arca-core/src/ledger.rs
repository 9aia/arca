//! The ledger — TODO.md + BACKLOG.md held together: task items
//! move between the two docs on status changes (`deferred` parks,
//! `done` deletes). Pure doc manipulation — loading/saving through
//! a `Store` lives in `arca-ops`.

use crate::{BacklogDoc, Item, ItemId, Section, TodoDoc};

/// TODO.md + BACKLOG.md as one in-memory ledger.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Ledger {
    /// `TODO.md`.
    pub todo: TodoDoc,
    /// `BACKLOG.md`.
    pub backlog: BacklogDoc,
}

/// Which doc a task lives in — and its position inside. `path` is
/// the index trail into the section's item tree: `[2]` is the
/// third top-level item, `[2,0]` its first child.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct TaskLoc {
    /// `true` = BACKLOG.md, `false` = TODO.md.
    pub backlog: bool,
    /// Section index.
    pub section: usize,
    /// Index path within the section — len ≥ 1; deeper than 1
    /// means a nested child.
    pub path: Vec<usize>,
}

impl TaskLoc {
    /// Is this a nested child (not a top-level line)?
    pub fn is_child(&self) -> bool {
        self.path.len() > 1
    }
}

impl Ledger {
    /// Both docs, in memory.
    pub fn new(todo: TodoDoc, backlog: BacklogDoc) -> Self {
        Ledger { todo, backlog }
    }

    /// Locate a task in either doc — top-level lines and nested
    /// children alike (`None` when it's not a task or doesn't
    /// exist).
    pub fn locate(&self, id: ItemId) -> Option<TaskLoc> {
        find_pos(&self.todo.sections, id)
            .map(|(section, path)| TaskLoc {
                backlog: false,
                section,
                path,
            })
            .or_else(|| {
                find_pos(&self.backlog.sections, id).map(|(section, path)| TaskLoc {
                    backlog: true,
                    section,
                    path,
                })
            })
    }

    /// The item at `loc`.
    pub fn item(&self, loc: &TaskLoc) -> &Item {
        let sections = if loc.backlog {
            &self.backlog.sections
        } else {
            &self.todo.sections
        };
        item_at(&sections[loc.section].items, &loc.path)
    }

    /// Mutable access to the item at `loc`.
    pub fn item_mut(&mut self, loc: &TaskLoc) -> &mut Item {
        let sections = if loc.backlog {
            &mut self.backlog.sections
        } else {
            &mut self.todo.sections
        };
        item_at_mut(&mut sections[loc.section].items, &loc.path)
    }

    /// Remove the item at `loc` — the whole subtree goes with it
    /// (`done` on a parent removes its children too). A section
    /// the removal emptied is dropped (an empty unsectioned run
    /// can't even be emitted).
    pub fn take(&mut self, loc: &TaskLoc) -> Item {
        let sections = if loc.backlog {
            &mut self.backlog.sections
        } else {
            &mut self.todo.sections
        };
        let mut level = &mut sections[loc.section].items;
        for &idx in &loc.path[..loc.path.len() - 1] {
            level = &mut level[idx].children;
        }
        let item = level.remove(loc.path[loc.path.len() - 1]);
        if loc.path.len() == 1 && sections[loc.section].items.is_empty() {
            sections.remove(loc.section);
        }
        item
    }

    /// Swap the item at `loc` with a sibling — `delta` -1 moves up,
    /// +1 down. Works on nested children (siblings share the
    /// parent). Returns false at the list edges. `loc` follows the
    /// item to its new index.
    pub fn move_within(&mut self, loc: &mut TaskLoc, delta: isize) -> bool {
        let sections = if loc.backlog {
            &mut self.backlog.sections
        } else {
            &mut self.todo.sections
        };
        let mut level = &mut sections[loc.section].items;
        for &idx in &loc.path[..loc.path.len() - 1] {
            level = &mut level[idx].children;
        }
        let i = loc.path[loc.path.len() - 1] as isize;
        let j = i + delta;
        if j < 0 || j as usize >= level.len() {
            return false;
        }
        level.swap(i as usize, j as usize);
        let last = loc.path.len() - 1;
        loc.path[last] = j as usize;
        true
    }

    /// Append to a TODO.md section: `section` names one (created
    /// when absent); `None` = the first section, else an
    /// unsectioned run.
    pub fn put_todo(&mut self, item: Item, section: Option<&str>) {
        push_item(&mut self.todo.sections, item, section);
    }

    /// Append to BACKLOG.md — same placement rule as [`put_todo`].
    pub fn put_backlog(&mut self, item: Item, section: Option<&str>) {
        push_item(&mut self.backlog.sections, item, section);
    }
}

fn find_pos(sections: &[Section], id: ItemId) -> Option<(usize, Vec<usize>)> {
    sections.iter().enumerate().find_map(|(si, s)| {
        let mut path = Vec::new();
        find_path(&s.items, id, &mut path).then_some((si, path))
    })
}

/// Depth-first id search — pushes the index trail onto `path`.
fn find_path(items: &[Item], id: ItemId, path: &mut Vec<usize>) -> bool {
    for (i, item) in items.iter().enumerate() {
        path.push(i);
        if item.id == id || find_path(&item.children, id, path) {
            return true;
        }
        path.pop();
    }
    false
}

fn item_at<'a>(mut items: &'a [Item], path: &[usize]) -> &'a Item {
    for &idx in &path[..path.len() - 1] {
        items = &items[idx].children;
    }
    &items[path[path.len() - 1]]
}

fn item_at_mut<'a>(mut items: &'a mut [Item], path: &[usize]) -> &'a mut Item {
    for &idx in &path[..path.len() - 1] {
        items = &mut items[idx].children;
    }
    &mut items[path[path.len() - 1]]
}

fn push_item(sections: &mut Vec<Section>, item: Item, section: Option<&str>) {
    match section {
        Some(name) => {
            if let Some(s) = sections
                .iter_mut()
                .find(|s| s.name.as_deref() == Some(name))
            {
                s.items.push(item);
            } else {
                sections.push(Section {
                    name: Some(name.to_string()),
                    items: vec![item],
                });
            }
        }
        None => {
            if sections.is_empty() {
                sections.push(Section::unsectioned());
            }
            sections[0].items.push(item);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{ItemKind, Status};

    fn t(n: u32) -> Item {
        Item::new(ItemId::new(ItemKind::Task, n), format!("t{n}"))
    }

    fn tid(n: u32) -> ItemId {
        ItemId::new(ItemKind::Task, n)
    }

    #[test]
    fn ledger_moves_and_prunes() {
        let mut ledger = Ledger::default();
        ledger.put_todo(t(1), None);
        ledger.put_todo(t(2), Some("Now"));
        let loc = ledger.locate(tid(1)).unwrap();
        assert!(!loc.backlog);
        let item = ledger.take(&loc);
        assert_eq!(item.id, tid(1));
        // emptied unsectioned run dropped
        assert!(ledger.todo.sections.iter().all(|s| !s.items.is_empty()));
        assert!(ledger.locate(tid(1)).is_none());
        ledger.put_backlog(item.status(Status::Deferred), None);
        let loc = ledger.locate(tid(1)).unwrap();
        assert!(loc.backlog);
    }

    #[test]
    fn ledger_locates_and_takes_children() {
        let mut ledger = Ledger::default();
        ledger.put_todo(t(1).child(t(2).child(t(4))).child(t(3)), None);
        ledger.put_todo(t(5), None);
        // locate walks the whole tree — the path says where
        let loc = ledger.locate(tid(4)).unwrap();
        assert_eq!(loc.path, vec![0, 0, 0]);
        assert!(loc.is_child());
        assert_eq!(ledger.item(&loc).id.number(), 4);
        // take on a child removes just that node
        ledger.take(&loc);
        let parent = &ledger.todo.sections[0].items[0];
        assert_eq!(parent.children[0].children.len(), 0);
        // take on a parent removes the whole subtree; sections
        // stay (a sibling remains)
        let loc = ledger.locate(tid(1)).unwrap();
        let gone = ledger.take(&loc);
        assert_eq!(gone.children.len(), 2);
        assert!(ledger.locate(tid(2)).is_none());
        assert_eq!(ledger.todo.sections[0].items.len(), 1);
        // mutation in place reaches children too
        let loc = ledger.locate(tid(5)).unwrap();
        ledger.item_mut(&loc).children.push(t(6));
        assert!(ledger.locate(tid(6)).is_some());
    }

    #[test]
    fn move_within_swaps_siblings() {
        let mut ledger = Ledger::default();
        ledger.put_todo(t(1), None);
        ledger.put_todo(t(2), None);
        ledger.put_todo(t(3).child(t(4)), None);
        // edges don't move
        let mut loc = ledger.locate(tid(1)).unwrap();
        assert!(!ledger.move_within(&mut loc, -1));
        // down swaps with the next sibling and updates loc
        assert!(ledger.move_within(&mut loc, 1));
        assert_eq!(loc.path, vec![1]);
        assert_eq!(ledger.todo.sections[0].items[1].id.number(), 1);
        // nested children reorder among themselves
        let mut loc = ledger.locate(tid(4)).unwrap();
        assert!(!ledger.move_within(&mut loc, -1));
    }
}

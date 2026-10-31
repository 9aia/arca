//! Status-transition walks — the shortest legal path `from → to`
//! through the state machine (BFS over [`Status::transitions`]).
//!
//! Convenience verbs — `done`, `defer`, `promote`, `resolve`,
//! proposal `status`, ADR `supersede` — walk multi-step paths
//! (e.g. `open → in-progress → done`), so every hop stays
//! machine-validated.

use std::collections::{BTreeMap, VecDeque};

use crate::{Item, ItemId, Status};

/// The shortest legal path `from → to`, or the transition error
/// naming the valid targets.
pub fn status_path(id: ItemId, from: Status, to: Status) -> Result<Vec<Status>, String> {
    if from == to {
        return Err(format!("{id} is already {to}"));
    }
    let mut prev: BTreeMap<Status, Status> = BTreeMap::new();
    let mut q = VecDeque::from([from]);
    while let Some(cur) = q.pop_front() {
        if cur == to {
            break;
        }
        for &next in cur.transitions() {
            if next != from && !prev.contains_key(&next) {
                prev.insert(next, cur);
                q.push_back(next);
            }
        }
    }
    if !prev.contains_key(&to) {
        return Err(format!(
            "{id}: invalid status transition {from} → {to} (valid from {from}: {})",
            targets(from)
        ));
    }
    let mut path = vec![to];
    while let Some(&cur) = path.last() {
        if cur == from {
            break;
        }
        path.push(prev[&cur]);
    }
    path.pop();
    path.reverse();
    Ok(path)
}

/// `"claimed, in-progress, deferred"` / `"none (terminal)"` — the
/// valid-targets suffix on transition errors.
pub fn targets(from: Status) -> String {
    let t: Vec<&str> = from.transitions().iter().map(|s| s.token()).collect();
    if t.is_empty() {
        "none (terminal)".into()
    } else {
        t.join(", ")
    }
}

/// Walk `item` along [`status_path`] to `to`, validating each hop.
pub fn walk_to(item: &mut Item, to: Status) -> Result<(), String> {
    for step in status_path(item.id, item.status, to)? {
        item.status = item.status.transition_to(step).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Walk every descendant to `to` — subtree moves keep the ledger
/// consistent: a deferred parent's children can't stay open in
/// BACKLOG.md, and children park/unpark with their parent. Nodes
/// already at `to` are left alone (identity isn't a transition).
pub fn walk_descendants_to(item: &mut Item, to: Status) -> Result<(), String> {
    for c in &mut item.children {
        if c.status != to {
            walk_to(c, to)?;
        }
        walk_descendants_to(c, to)?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::ItemKind;

    fn t(n: u32, s: Status) -> Item {
        Item::new(ItemId::new(ItemKind::Task, n), "x").status(s)
    }

    #[test]
    fn status_path_walks() {
        let id = ItemId::new(ItemKind::Task, 1);
        assert_eq!(
            status_path(id, Status::Open, Status::Done).unwrap(),
            vec![Status::InProgress, Status::Done]
        );
        assert_eq!(
            status_path(id, Status::Open, Status::Deferred).unwrap(),
            vec![Status::Deferred]
        );
        assert_eq!(
            status_path(id, Status::Blocked, Status::Done).unwrap(),
            vec![Status::InProgress, Status::Done]
        );
        // deferred reaches done only via open → in-progress
        assert_eq!(
            status_path(id, Status::Deferred, Status::Done).unwrap(),
            vec![Status::Open, Status::InProgress, Status::Done]
        );
        // no path out of terminal states
        assert!(status_path(id, Status::Done, Status::Open).is_err());
        // same → error
        assert!(status_path(id, Status::Open, Status::Open).is_err());
        // error message lists valid targets
        let e = status_path(id, Status::Done, Status::Open).unwrap_err();
        assert!(e.contains("none (terminal)"), "{e}");
    }

    #[test]
    fn walk_descendants_skips_already_there() {
        let mut parent = t(1, Status::Open).child(t(2, Status::InProgress));
        walk_descendants_to(&mut parent, Status::Deferred).unwrap();
        assert_eq!(parent.children[0].status, Status::Deferred);
        // already-deferred nodes don't re-error on the identity walk
        let mut parked = t(3, Status::Deferred).child(t(4, Status::Deferred));
        walk_descendants_to(&mut parked, Status::Deferred).unwrap();
        // unpark: deferred children come back to open
        walk_descendants_to(&mut parked, Status::Open).unwrap();
        assert_eq!(parked.children[0].status, Status::Open);
    }
}

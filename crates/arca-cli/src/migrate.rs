//! `arca migrate` — bring legacy ledgers to the v2 schema.
//!
//! Two modes, auto-detected:
//!
//! - **`pm/` → `project/`** — a `pm/` dir at the repo root is the
//!   pre-worktree layout. `TODO.md`/`BACKLOG.md` bullets become
//!   T-items, `ROADMAP.md` becomes a `plans/` file, any other
//!   `*.md` becomes an issue; the `pm/` dir is removed and its
//!   deletion committed on the current branch.
//! - **legacy `project/` → v2** — files already on the `project`
//!   branch are normalized in place: id-less items get ids,
//!   `[x]`/done items drop, heading-like lines become sections,
//!   frontmatter-less files in item dirs are adopted (the
//!   filename's number is honored when free — `0001-foo.md` →
//!   `ADR-0001` — collisions take the next unclaimed number with a
//!   warning; synthesized frontmatter + canonical name),
//!   everything re-emitted canonically.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use arca_core::{BacklogDoc, Item, ItemFile, ItemId, ItemKind, Section, Status, TodoDoc};
use arca_store::Store;
use arca_store_md::frontmatter_id;
use serde_json::json;

use crate::discover;
use crate::init;
use crate::ops;

/// The file-backed kinds, in processing order.
const FILE_KINDS: [ItemKind; 4] = [
    ItemKind::Issue,
    ItemKind::Proposal,
    ItemKind::Adr,
    ItemKind::Plan,
];

/// What a `pm/` file converts into.
enum PmDest {
    Todo,
    Backlog,
    Plan,
    Issue,
}

fn pm_dest(name: &str) -> Option<PmDest> {
    if name.eq_ignore_ascii_case("todo.md") {
        Some(PmDest::Todo)
    } else if name.eq_ignore_ascii_case("backlog.md") {
        Some(PmDest::Backlog)
    } else if name.eq_ignore_ascii_case("roadmap.md") {
        Some(PmDest::Plan)
    } else if name.ends_with(".md") {
        Some(PmDest::Issue)
    } else {
        None
    }
}

/// First `#`-heading text in a markdown body, else a fallback.
fn first_heading(src: &str, fallback: &str) -> String {
    for line in src.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix('#') {
            let title = rest.trim_start_matches('#').trim();
            if !title.is_empty() {
                return title.to_string();
            }
        }
    }
    fallback.to_string()
}

/// Per-run counters for the JSON report.
#[derive(Default)]
struct Stats {
    converted_items: usize,
    ids_allocated: usize,
    dropped_done: usize,
    files_moved: BTreeMap<String, String>,
    unparsed: Vec<String>,
    warnings: Vec<String>,
}

/// Filename-number claim table for frontmatter-less adoption —
/// one per `migrate` run, spanning all item dirs. A frontmatter-less
/// `NNNN-slug.md` is *borrowing* its number: adoption honors it when
/// no real item owns it (a `taken` id), and a collision probes
/// upward past numbers other unprocessed adoptees still want
/// (`pending`) so `0001-a` + `0001-b` + `0002-c` land 1, 3, 2 —
/// never a fresh counter id.
#[derive(Default)]
struct AdoptClaims {
    /// Ids owned by real items — frontmatter ids, non-adoptee
    /// filenames, TODO/BACKLOG items, archived files.
    taken: BTreeSet<ItemId>,
    /// Filename numbers adoptees not yet processed still want.
    pending: BTreeMap<ItemId, usize>,
}

impl AdoptClaims {
    /// Claim `desired` for the adoptee now being processed: the
    /// number itself when free, else the next number neither taken
    /// nor wanted by a remaining adoptee. Returns `(id, collided)`.
    fn claim(&mut self, desired: ItemId) -> (ItemId, bool) {
        // desired was registered into pending in the claim pass —
        // or_insert keeps the unreachable unclaimed case honest
        let c = self.pending.entry(desired).or_insert(1);
        *c -= 1;
        if *c == 0 {
            self.pending.remove(&desired);
        }
        if !self.taken.contains(&desired) {
            self.taken.insert(desired);
            return (desired, false);
        }
        let mut n = desired.next();
        while self.taken.contains(&n) || self.pending.contains_key(&n) {
            n = n.next();
        }
        self.taken.insert(n);
        (n, true)
    }

    /// Record an id allocated outside the claim path (e.g.
    /// `alloc_id` for a filename without a number).
    fn reserve(&mut self, id: ItemId) {
        self.taken.insert(id);
    }
}

/// Turn a legacy item into a real one — keep a same-kind id or
/// allocate via the store.
fn materialize(
    store: &arca_store_md::MdStore,
    li: &arca_store_md::LegacyItem,
    kind: ItemKind,
    stats: &mut Stats,
) -> Result<Item, String> {
    let mut item = li.item.clone();
    item.id = match li.id {
        Some(id) if id.kind() == kind => id,
        _ => {
            stats.ids_allocated += 1;
            store.alloc_id(kind).map_err(|e| e.to_string())?
        }
    };
    stats.converted_items += 1;
    Ok(item)
}

/// Legacy sections → v2 `Section`s, allocating ids as needed.
fn convert_sections(
    store: &arca_store_md::MdStore,
    doc: &arca_store_md::LegacyDoc,
    kind: ItemKind,
    stats: &mut Stats,
) -> Result<Vec<Section>, String> {
    let mut out = Vec::new();
    for sec in &doc.sections {
        let mut s = Section {
            name: sec.name.clone(),
            items: Vec::new(),
        };
        for li in &sec.items {
            s.items.push(materialize(store, li, kind, stats)?);
        }
        out.push(s);
    }
    Ok(out)
}

/// Mode A — `pm/` at the repo root.
fn migrate_pm(
    dir: Option<&Path>,
    root_hint: Option<&Path>,
    no_commit: bool,
) -> Result<(serde_json::Value, i32), String> {
    let root = discover::repo_root_for(dir, root_hint)?;
    let pm = root.join("pm");
    // run() only routes here when pm/ exists — a vanish between
    // the two probes just fails at read_dir below
    debug_assert!(pm.is_dir(), "migrate_pm without pm/ at {}", root.display());
    // ensure the project worktree exists (init when needed)
    let ensured = init::ensure(dir, Some(&root), no_commit)?;
    let wt = ensured.worktree.clone();
    let store = ops::store_at(&wt)?;
    let mut stats = Stats::default();

    let mut todo = store.todo().map_err(|e| e.to_string())?;
    let mut backlog = store.backlog().map_err(|e| e.to_string())?;
    let mut names: Vec<String> = std::fs::read_dir(&pm)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .collect();
    names.sort();
    let mut leftovers: Vec<String> = Vec::new();
    for name in &names {
        let src = std::fs::read_to_string(pm.join(name)).map_err(|e| e.to_string())?;
        let rel_from = format!("pm/{name}");
        match pm_dest(name) {
            Some(PmDest::Todo) => {
                let (_repo, doc) = arca_store_md::parse_legacy_todo(&src, "migrated");
                stats.dropped_done += doc.dropped;
                todo.sections
                    .extend(convert_sections(&store, &doc, ItemKind::Task, &mut stats)?);
                stats.files_moved.insert(rel_from, "TODO.md".into());
            }
            Some(PmDest::Backlog) => {
                let doc = arca_store_md::parse_legacy_backlog(&src);
                stats.dropped_done += doc.dropped;
                let mut secs = convert_sections(&store, &doc, ItemKind::Task, &mut stats)?;
                for s in &mut secs {
                    for i in &mut s.items {
                        i.status = Status::Deferred;
                    }
                }
                backlog.sections.extend(secs);
                stats.files_moved.insert(rel_from, "BACKLOG.md".into());
            }
            Some(PmDest::Plan) => {
                let id = store.alloc_id(ItemKind::Plan).map_err(|e| e.to_string())?;
                stats.ids_allocated += 1;
                let title = first_heading(&src, "Roadmap");
                let file = ItemFile::new(Item::new(id, &title)).body(src);
                store.save_item(&file).map_err(|e| e.to_string())?;
                let rel = format!(
                    "plans/{}",
                    arca_store_md::item_filename(&id, &title).unwrap_or_default()
                );
                stats.converted_items += 1;
                stats.files_moved.insert(rel_from, rel);
            }
            Some(PmDest::Issue) => {
                let id = store.alloc_id(ItemKind::Issue).map_err(|e| e.to_string())?;
                stats.ids_allocated += 1;
                let stem = name.trim_end_matches(".md");
                let title = first_heading(&src, stem);
                let file = ItemFile::new(Item::new(id, &title)).body(src);
                store.save_item(&file).map_err(|e| e.to_string())?;
                let rel = format!(
                    "issues/{}",
                    arca_store_md::item_filename(&id, &title).unwrap_or_default()
                );
                stats.converted_items += 1;
                stats.files_moved.insert(rel_from, rel);
            }
            None => leftovers.push(rel_from),
        }
    }
    store.save_todo(&todo).map_err(|e| e.to_string())?;
    store.save_backlog(&backlog).map_err(|e| e.to_string())?;

    // remove converted files; the dir only goes when it's empty
    for name in &names {
        if pm_dest(name).is_some() {
            let _ = std::fs::remove_file(pm.join(name));
        }
    }
    let empty = std::fs::read_dir(&pm)
        .map(|mut it| it.next().is_none())
        .unwrap_or(false);
    let pm_removed = if empty {
        std::fs::remove_dir(&pm).is_ok()
    } else {
        false
    };

    let enabled = ops::commits_enabled(no_commit);
    let msg = format!(
        "chore: migrate pm/ → project/ ({} items)",
        stats.converted_items
    );
    let c1 = ops::autocommit(&wt, &msg, enabled)?;
    let c2 = commit_pm_removal(&root, enabled)?;
    let committed = c1 || c2;

    let out = json!({
        "mode": "pm",
        "converted_items": stats.converted_items,
        "ids_allocated": stats.ids_allocated,
        "dropped_done": stats.dropped_done,
        "files_moved": stats.files_moved.iter()
            .map(|(from, to)| json!({"from": from, "to": to}))
            .collect::<Vec<_>>(),
        "pm_removed": pm_removed,
        "pm_leftover": leftovers,
        "worktree": wt.to_string_lossy(),
        "committed": committed,
    });
    Ok((out, 0))
}

/// `git add -u pm/ && git commit -- pm/` — stage only tracked
/// deletions so untracked leftovers never get swept in.
fn commit_pm_removal(root: &Path, enabled: bool) -> Result<bool, String> {
    if !enabled {
        return Ok(false);
    }
    // `add -u` errors when pm/ holds no tracked files at all —
    // untracked leftovers aren't ours to commit
    if arca_git::git_out(root, &["ls-files", "--", "pm"])
        .map_err(|e| e.to_string())?
        .is_empty()
    {
        return Ok(false);
    }
    arca_git::git_out(root, &["add", "-u", "--", "pm"]).map_err(|e| e.to_string())?;
    let dirty = arca_git::git_out(root, &["status", "--porcelain", "--", "pm"])
        .map_err(|e| e.to_string())?;
    if dirty.is_empty() {
        return Ok(false);
    }
    arca_git::git_out(
        root,
        &["commit", "-m", "chore: migrate pm/ → project/", "--", "pm"],
    )
    .map_err(|e| e.to_string())
    .map(|_| true)
}

/// Mode B — normalize files already on the `project` branch.
fn migrate_project(
    dir: Option<&Path>,
    root_hint: Option<&Path>,
    no_commit: bool,
) -> Result<(serde_json::Value, i32), String> {
    let proj = discover::discover(dir, root_hint)?;
    let wt = &proj.worktree;
    let store = ops::store_at(wt)?;
    let mut stats = Stats::default();

    // seed counters from ids we're about to keep so allocation
    // can't collide with them. A fresh default meta takes the
    // worktree's dir name ("project") — the repo dir name is the
    // better guess.
    let mut meta = store.meta().map_err(|e| e.to_string())?;
    if !wt.join("meta.toml").exists() {
        if let Some(n) = proj.root.file_name() {
            meta.name = n.to_string_lossy().into_owned();
        }
    }

    // --- TODO.md ---
    if let Some(src) = read_str(wt, "TODO.md") {
        let doc = match arca_store_md::parse_todo(&src) {
            Ok(doc) => doc, // already v2 — re-emit normalizes drift
            Err(_) => {
                let (repo, legacy) = arca_store_md::parse_legacy_todo(&src, &meta.name);
                stats.dropped_done += legacy.dropped;
                seed_counters(&store, &legacy)?;
                let mut doc = TodoDoc::new(if repo.is_empty() { &meta.name } else { &repo });
                doc.sections = convert_sections(&store, &legacy, ItemKind::Task, &mut stats)?;
                doc
            }
        };
        store.save_todo(&doc).map_err(|e| e.to_string())?;
    }

    // --- BACKLOG.md ---
    if let Some(src) = read_str(wt, "BACKLOG.md") {
        let doc = match arca_store_md::parse_backlog(&src) {
            Ok(doc) => doc,
            Err(_) => {
                let legacy = arca_store_md::parse_legacy_backlog(&src);
                stats.dropped_done += legacy.dropped;
                seed_counters(&store, &legacy)?;
                let mut secs = convert_sections(&store, &legacy, ItemKind::Task, &mut stats)?;
                for s in &mut secs {
                    for i in &mut s.items {
                        i.status = Status::Deferred;
                    }
                }
                BacklogDoc { sections: secs }
            }
        };
        store.save_backlog(&doc).map_err(|e| e.to_string())?;
    }

    // --- item dirs: normalize filenames + adopt unfrontmattered ---
    //
    // Claim table for adoption: `taken` = ids a real item owns —
    // everything `all_ids` sees plus frontmatter ids (which can
    // outrun the filename view), minus numbers merely *borrowed* by
    // adoptee filenames. `pending` = filename numbers adoptees
    // still want, so a collision probes past them instead of
    // jumping to the counter.
    let mut claims = AdoptClaims {
        taken: store.all_ids().map_err(|e| e.to_string())?,
        pending: BTreeMap::new(),
    };
    for (kind, dir_name) in FILE_KINDS
        .into_iter()
        .filter_map(|k| arca_store_md::item_dir(k).map(|d| (k, d)))
    {
        let dir = wt.join(dir_name);
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut owned: BTreeSet<ItemId> = BTreeSet::new();
        let mut desired: Vec<ItemId> = Vec::new();
        for entry in read.flatten().filter(|e| e.path().is_file()) {
            let Some(name) = entry.file_name().to_str().map(String::from) else {
                continue;
            };
            if !name.ends_with(".md") || name.starts_with('.') {
                continue;
            }
            let fname_id = arca_store_md::parse_item_filename(kind, &name);
            match read_str(wt, &format!("{dir_name}/{name}"))
                .as_deref()
                .and_then(frontmatter_id)
            {
                // adoptee — the filename number is a claim to honor
                None => desired.extend(fname_id),
                // real item — it lands on its frontmatter id, so
                // that number is off-limits (the filename number is
                // vacated by the canonical rename, not reserved)
                Some(fm_id) => {
                    owned.insert(fm_id);
                }
            }
        }
        // archived files keep their numbers reserved forever
        if let Ok(read) = std::fs::read_dir(dir.join("archive")) {
            for entry in read.flatten().filter(|e| e.path().is_file()) {
                if let Some(name) = entry.file_name().to_str() {
                    owned.extend(arca_store_md::parse_item_filename(kind, name));
                }
            }
        }
        for d in &desired {
            if !owned.contains(d) {
                claims.taken.remove(d);
            }
            *claims.pending.entry(*d).or_default() += 1;
        }
        claims.taken.extend(owned);
    }

    let mut renames: BTreeMap<String, String> = BTreeMap::new();
    for (kind, dir_name) in FILE_KINDS
        .into_iter()
        .filter_map(|k| arca_store_md::item_dir(k).map(|d| (k, d)))
    {
        let dir = wt.join(dir_name);
        if !dir.is_dir() {
            continue;
        }
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .map_err(|e| e.to_string())?
            .flatten()
            .filter(|e| e.path().is_file())
            .filter_map(|e| e.file_name().to_str().map(String::from))
            .filter(|n| n.ends_with(".md") && !n.starts_with('.'))
            .collect();
        names.sort();
        for name in names {
            let rel = format!("{dir_name}/{name}");
            let src = match read_str(wt, &rel) {
                Some(s) => s,
                None => continue,
            };
            match adopt_or_normalize(&store, &src, kind, &rel, &mut stats, &mut claims)? {
                Some(new_rel) if new_rel != rel => {
                    renames.insert(rel.clone(), new_rel.clone());
                    stats.files_moved.insert(rel, new_rel);
                }
                _ => {}
            }
        }
    }

    // index.md: repoint links at renamed files (raw passthrough,
    // so a plain path replace is the whole fix)
    if !renames.is_empty() {
        if let Some(mut index) = read_str(wt, "index.md") {
            let orig = index.clone();
            for (from, to) in &renames {
                index = index.replace(from.as_str(), to.as_str());
            }
            if index != orig {
                store.save_index(&index).map_err(|e| e.to_string())?;
            }
        }
    }

    // meta.toml: create if missing + reconcile counters
    {
        let mut reg = meta.registry();
        reg.observe_all(store.all_ids().map_err(|e| e.to_string())?);
        meta.next = reg.counters();
        store.save_meta(&meta).map_err(|e| e.to_string())?;
    }

    // fill any remaining scaffold gaps (dirs, index, docs)
    let created = ops::scaffold(&store, &meta.name).map_err(|e| e.to_string())?;

    let enabled = ops::commits_enabled(no_commit);
    let msg = format!(
        "chore: migrate project/ → v2 schema ({} items)",
        stats.converted_items
    );
    let committed = ops::autocommit(wt, &msg, enabled)?;

    let out = json!({
        "mode": "project",
        "converted_items": stats.converted_items,
        "ids_allocated": stats.ids_allocated,
        "dropped_done": stats.dropped_done,
        "files_moved": stats.files_moved.iter()
            .map(|(from, to)| json!({"from": from, "to": to}))
            .collect::<Vec<_>>(),
        "unparsed": stats.unparsed,
        "warnings": stats.warnings,
        "scaffold_created": created,
        "worktree": wt.to_string_lossy(),
        "committed": committed,
    });
    Ok((out, 0))
}

/// Raise meta counters above any ids the legacy doc keeps, so later
/// `alloc_id` calls can't reissue them. Persists meta.toml.
fn seed_counters(
    store: &arca_store_md::MdStore,
    legacy: &arca_store_md::LegacyDoc,
) -> Result<(), String> {
    let mut meta = store.meta().map_err(|e| e.to_string())?;
    let mut reg = meta.registry();
    reg.observe_all(store.all_ids().map_err(|e| e.to_string())?);
    reg.observe_all(legacy.items().filter_map(|li| li.id));
    meta.next = reg.counters();
    store.save_meta(&meta).map_err(|e| e.to_string())
}

/// Bring one file in an item dir to spec. Returns its new rel path
/// when it moved (or was adopted), `None` when left as-is.
fn adopt_or_normalize(
    store: &arca_store_md::MdStore,
    src: &str,
    kind: ItemKind,
    rel: &str,
    stats: &mut Stats,
    claims: &mut AdoptClaims,
) -> Result<Option<String>, String> {
    let wt = store.root();
    // 1) parses under its own kind → save_item re-emits canonically
    //    and renames the file when the slug drifted
    if let Ok(file) = arca_store_md::parse_item_file(src, kind, rel) {
        let id = file.meta.id;
        store.save_item(&file).map_err(|e| e.to_string())?;
        let canonical = arca_store_md::item_filename(&id, &file.meta.title)
            .map(|f| format!("{}/{}", arca_store_md::item_dir(kind).unwrap_or(""), f));
        // save_item removes a drifted name only when it can tie it
        // to the id — a name without a number is ours to clean up
        if canonical.as_deref() != Some(rel) {
            let _ = std::fs::remove_file(wt.join(rel));
        }
        return Ok(canonical.filter(|c| *c != rel));
    }
    // 2) frontmatter id of a different kind → move + re-emit
    if let Some(id) = frontmatter_id(src) {
        if id.kind() != kind {
            if let Ok(file) = arca_store_md::parse_item_file(src, id.kind(), rel) {
                store.save_item(&file).map_err(|e| e.to_string())?;
                let _ = std::fs::remove_file(wt.join(rel));
                let dir2 = arca_store_md::item_dir(id.kind()).unwrap_or("");
                let canonical = arca_store_md::item_filename(&id, &file.meta.title)
                    .map(|f| format!("{dir2}/{f}"));
                return Ok(canonical);
            }
            // has frontmatter but can't be parsed under any kind —
            // leave it for `arca check` to report
            stats.unparsed.push(rel.to_string());
            return Ok(None);
        }
        // same kind but unparseable — leave to check
        stats.unparsed.push(rel.to_string());
        return Ok(None);
    }
    // 3) no frontmatter at all → adopt: honor the filename number
    //    when free (legacy ids like `0001-gha-compat.md` encode
    //    real numbering), else probe the next number no other
    //    adoptee wants; number-less names take a fresh alloc
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let id = match arca_store_md::parse_item_filename(kind, name) {
        Some(desired) => {
            let (id, collided) = claims.claim(desired);
            if collided {
                stats
                    .warnings
                    .push(format!("{rel}: {desired} already taken — adopted as {id}"));
            }
            id
        }
        None => {
            let id = store.alloc_id(kind).map_err(|e| e.to_string())?;
            claims.reserve(id);
            id
        }
    };
    stats.ids_allocated += 1;
    stats.converted_items += 1;
    let stem = name.trim_end_matches(".md");
    let title = first_heading(src, stem);
    let file = ItemFile::new(Item::new(id, &title)).body(src.to_string());
    store.save_item(&file).map_err(|e| e.to_string())?;
    // an already-canonical adoptee lands on its own name — deleting
    // `rel` then would remove the file we just wrote
    let canonical = arca_store_md::item_filename(&id, &title)
        .map(|f| format!("{}/{f}", arca_store_md::item_dir(kind).unwrap_or("")));
    if canonical.as_deref() != Some(rel) {
        let _ = std::fs::remove_file(wt.join(rel));
    }
    Ok(canonical)
}

fn read_str(wt: &Path, rel: &str) -> Option<String> {
    std::fs::read_to_string(wt.join(rel)).ok()
}

/// The `migrate` verb.
pub fn run(
    dir: Option<PathBuf>,
    root: Option<PathBuf>,
    json_out: bool,
    no_commit: bool,
) -> Result<i32, String> {
    let repo = discover::repo_root_for(dir.as_deref(), root.as_deref());
    let pm_mode = repo
        .as_ref()
        .map(|r| r.join("pm").is_dir())
        .unwrap_or(false);
    let (out, code) = if pm_mode {
        migrate_pm(dir.as_deref(), root.as_deref(), no_commit)?
    } else {
        migrate_project(dir.as_deref(), root.as_deref(), no_commit)?
    };
    if json_out {
        ops::print_json(&out);
    } else {
        println!(
            "arca migrate ({}): {} items, {} ids allocated, {} done dropped",
            out["mode"], out["converted_items"], out["ids_allocated"], out["dropped_done"]
        );
        for m in out["files_moved"].as_array().into_iter().flatten() {
            println!("  {} → {}", m["from"], m["to"]);
        }
        for f in out["unparsed"].as_array().into_iter().flatten() {
            eprintln!("  unparsed: {f}");
        }
        for w in out["warnings"].as_array().into_iter().flatten() {
            eprintln!("  warning: {}", w.as_str().unwrap_or("?"));
        }
    }
    Ok(code)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn pm_dest_mapping() {
        assert!(matches!(pm_dest("TODO.md"), Some(PmDest::Todo)));
        assert!(matches!(pm_dest("todo.md"), Some(PmDest::Todo)));
        assert!(matches!(pm_dest("BACKLOG.md"), Some(PmDest::Backlog)));
        assert!(matches!(pm_dest("ROADMAP.md"), Some(PmDest::Plan)));
        assert!(matches!(pm_dest("NOTES.md"), Some(PmDest::Issue)));
        assert!(pm_dest("x.txt").is_none());
    }

    #[test]
    fn first_heading_extracts() {
        assert_eq!(first_heading("# Title here\n\nbody", "fb"), "Title here");
        assert_eq!(first_heading("## deep\n", "fb"), "deep");
        assert_eq!(first_heading("no heading\n", "fb"), "fb");
    }

    #[test]
    fn frontmatter_id_probe() {
        assert_eq!(
            frontmatter_id("+++\nid = \"I-2\"\nt = \"x\"\n+++\n"),
            Some(ItemId::new(ItemKind::Issue, 2))
        );
        assert_eq!(frontmatter_id("# heading\n"), None);
    }
}

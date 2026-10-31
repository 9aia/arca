//! `MdStore` — the filesystem impl of [`arca_store::Store`] over a
//! `project/` worktree. Writes are atomic (tmp + rename).

use std::collections::BTreeSet;

use arca_core::{BacklogDoc, HandoffEntry, ItemFile, ItemId, ItemKind, ProjectMeta, TodoDoc};
use arca_store::{Error, HandoffFile, Store};
use camino::{Utf8Path, Utf8PathBuf};

use crate::{backlog, handoff, itemfile, meta, todo};

/// The subdir of each item dir holding archived (done, shelved)
/// item files — `issues/archive/issue-NNNN-*.md`.
const ARCHIVE_DIR: &str = "archive";

/// A `project/` worktree on disk.
pub struct MdStore {
    root: Utf8PathBuf,
}

impl MdStore {
    /// Store rooted at the `project/` worktree dir.
    pub fn new(root: impl Into<Utf8PathBuf>) -> Self {
        MdStore { root: root.into() }
    }

    /// The worktree root.
    pub fn root(&self) -> &Utf8Path {
        &self.root
    }

    /// Repo name fallback — the worktree dir name (used when
    /// `meta.toml`/`TODO.md` don't exist yet).
    fn default_name(&self) -> String {
        self.root.file_name().unwrap_or("project").to_string()
    }

    fn path(&self, rel: &str) -> Utf8PathBuf {
        self.root.join(rel)
    }

    fn read(&self, rel: &str) -> Result<String, Error> {
        let p = self.path(rel);
        match std::fs::read_to_string(&p) {
            Ok(s) => Ok(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::NotFound {
                path: rel.to_string(),
            }),
            Err(e) => Err(Error::Io(e)),
        }
    }

    /// Item files of a kind → `(id, path)` pairs, sorted by id.
    /// `archived` scopes the scan to `<dir>/archive/` — cold
    /// storage for done items. Non-matching names are skipped
    /// (`arca check` reports them).
    fn item_paths(
        &self,
        kind: ItemKind,
        archived: bool,
    ) -> Result<Vec<(ItemId, Utf8PathBuf)>, Error> {
        let Some(dir) = itemfile::item_dir(kind) else {
            return Ok(Vec::new());
        };
        let mut dir = self.path(dir);
        if archived {
            dir = dir.join(ARCHIVE_DIR);
        }
        let read = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::Io(e)),
        };
        let mut out = Vec::new();
        for entry in read {
            let entry = entry.map_err(Error::Io)?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if let Some(id) = itemfile::parse_item_filename(kind, name) {
                // name.to_str() succeeded → dir.join(name) is utf8 too
                out.push((id, dir.join(name)));
            }
        }
        out.sort_by_key(|(id, _)| *id);
        Ok(out)
    }

    /// Parse every item file of a kind in one scope (live or
    /// `archive/`) — shared by `list_items`/`list_archived`.
    fn list_scoped(&self, kind: ItemKind, archived: bool) -> Result<Vec<ItemFile>, Error> {
        let mut out = Vec::new();
        for (_, path) in self.item_paths(kind, archived)? {
            let rel = path
                .strip_prefix(&self.root)
                .map_or_else(|_| path.as_str(), |r| r.as_str());
            let src = self.read(rel)?;
            out.push(itemfile::parse_item_file(&src, kind, rel)?);
        }
        Ok(out)
    }
}

impl Store for MdStore {
    fn meta(&self) -> Result<ProjectMeta, Error> {
        match self.read("meta.toml") {
            Ok(src) => Ok(meta::parse_meta(&src)?),
            Err(Error::NotFound { .. }) => Ok(ProjectMeta::new(self.default_name())),
            Err(e) => Err(e),
        }
    }

    fn save_meta(&self, m: &ProjectMeta) -> Result<(), Error> {
        write_atomic(&self.path("meta.toml"), &meta::emit_meta(m)?)
    }

    fn alloc_id(&self, kind: ItemKind) -> Result<ItemId, Error> {
        let mut meta = self.meta()?;
        let mut registry = meta.registry();
        registry.observe_all(self.all_ids()?);
        let id = registry.alloc(kind);
        meta.next = registry.counters();
        self.save_meta(&meta)?;
        Ok(id)
    }

    fn all_ids(&self) -> Result<BTreeSet<ItemId>, Error> {
        let mut ids = BTreeSet::new();
        if let Ok(doc) = self.todo() {
            ids.extend(doc.items().flat_map(|i| i.subtree()).map(|i| i.id));
        }
        if let Ok(doc) = self.backlog() {
            ids.extend(doc.items().flat_map(|i| i.subtree()).map(|i| i.id));
        }
        for kind in [
            ItemKind::Issue,
            ItemKind::Proposal,
            ItemKind::Adr,
            ItemKind::Plan,
        ] {
            // archived ids stay reserved — ids are never reused
            for archived in [false, true] {
                for (id, _) in self.item_paths(kind, archived)? {
                    ids.insert(id);
                }
            }
        }
        Ok(ids)
    }

    fn todo(&self) -> Result<TodoDoc, Error> {
        match self.read("TODO.md") {
            Ok(src) => Ok(todo::parse_todo(&src)?),
            Err(Error::NotFound { .. }) => Ok(TodoDoc::new(self.default_name())),
            Err(e) => Err(e),
        }
    }

    fn save_todo(&self, doc: &TodoDoc) -> Result<(), Error> {
        write_atomic(&self.path("TODO.md"), &todo::emit_todo(doc)?)
    }

    fn backlog(&self) -> Result<BacklogDoc, Error> {
        match self.read("BACKLOG.md") {
            Ok(src) => Ok(backlog::parse_backlog(&src)?),
            Err(Error::NotFound { .. }) => Ok(BacklogDoc::default()),
            Err(e) => Err(e),
        }
    }

    fn save_backlog(&self, doc: &BacklogDoc) -> Result<(), Error> {
        write_atomic(&self.path("BACKLOG.md"), &backlog::emit_backlog(doc)?)
    }

    fn item(&self, id: ItemId) -> Result<ItemFile, Error> {
        // live first, then archive — a same-id file in both scopes
        // surfaces as a duplicate rather than shadowing silently
        let mut hits = self
            .item_paths(id.kind(), false)?
            .into_iter()
            .chain(self.item_paths(id.kind(), true)?)
            .filter(|(i, _)| *i == id);
        let Some((_, path)) = hits.next() else {
            return Err(Error::NotFound {
                path: format!("{}/{id}", itemfile::item_dir(id.kind()).unwrap_or("<none>")),
            });
        };
        if hits.next().is_some() {
            return Err(Error::Invalid(format!("duplicate item files for {id}")));
        }
        let rel = path
            .strip_prefix(&self.root)
            .map_or_else(|_| path.as_str(), |r| r.as_str());
        let src = self.read(rel)?;
        Ok(itemfile::parse_item_file(&src, id.kind(), rel)?)
    }

    fn save_item(&self, file: &ItemFile) -> Result<(), Error> {
        let dir = itemfile::item_dir(file.meta.id.kind()).ok_or_else(|| {
            Error::Invalid("tasks aren't file items — they live in TODO.md/BACKLOG.md".into())
        })?;
        let filename = itemfile::item_filename(&file.meta.id, &file.meta.title)
            .ok_or_else(|| Error::Invalid("not a file-backed item kind".into()))?;
        let dir_path = self.path(dir);
        std::fs::create_dir_all(&dir_path).map_err(Error::Io)?;
        let target = dir_path.join(&filename);
        // rename: an existing file for the same id under a different
        // slug gets removed after the new one lands. The filename
        // match alone isn't enough — frontmatter-less adoptees can
        // borrow the number, so the old file must actually claim
        // this id to count as the same item's previous name.
        let old = self
            .item_paths(file.meta.id.kind(), false)?
            .into_iter()
            .filter(|(i, p)| *i == file.meta.id && *p != target)
            .map(|(_, p)| p)
            .find(|p| {
                let rel = p
                    .strip_prefix(&self.root)
                    .map_or_else(|_| p.as_str(), |r| r.as_str());
                self.read(rel)
                    .ok()
                    .and_then(|src| itemfile::frontmatter_id(&src))
                    == Some(file.meta.id)
            });
        write_atomic(&target, &itemfile::emit_item_file(file)?)?;
        if let Some(old) = old {
            std::fs::remove_file(&old).map_err(Error::Io)?;
        }
        Ok(())
    }

    fn remove_item(&self, id: ItemId) -> Result<(), Error> {
        let Some((_, path)) = self
            .item_paths(id.kind(), false)?
            .into_iter()
            .find(|(i, _)| *i == id)
        else {
            return Err(Error::NotFound {
                path: format!("{}/{id}", itemfile::item_dir(id.kind()).unwrap_or("<none>")),
            });
        };
        std::fs::remove_file(&path).map_err(Error::Io)
    }

    fn list_items(&self, kind: ItemKind) -> Result<Vec<ItemFile>, Error> {
        self.list_scoped(kind, false)
    }

    fn list_archived(&self, kind: ItemKind) -> Result<Vec<ItemFile>, Error> {
        self.list_scoped(kind, true)
    }

    fn archive_item(&self, id: ItemId) -> Result<(), Error> {
        let Some((_, path)) = self
            .item_paths(id.kind(), false)?
            .into_iter()
            .find(|(i, _)| *i == id)
        else {
            if self.is_archived(id)? {
                return Err(Error::Invalid(format!("{id} is already archived")));
            }
            return Err(Error::NotFound {
                path: format!("{}/{id}", itemfile::item_dir(id.kind()).unwrap_or("<none>")),
            });
        };
        let name = path.file_name().unwrap_or_default();
        let parent = path
            .parent()
            .ok_or_else(|| Error::Invalid(format!("no parent dir for {path}")))?;
        let target = parent.join(ARCHIVE_DIR).join(name);
        std::fs::create_dir_all(parent.join(ARCHIVE_DIR)).map_err(Error::Io)?;
        std::fs::rename(&path, &target).map_err(Error::Io)
    }

    fn is_archived(&self, id: ItemId) -> Result<bool, Error> {
        Ok(self
            .item_paths(id.kind(), true)?
            .into_iter()
            .any(|(i, _)| i == id))
    }

    fn index_md(&self) -> Result<String, Error> {
        self.read("index.md")
    }

    fn save_index(&self, md: &str) -> Result<(), Error> {
        write_atomic(&self.path("index.md"), md)
    }

    fn read_handoffs(&self, id: ItemId) -> Result<Vec<HandoffEntry>, Error> {
        // live file first, then handoffs/archive/ — like item()
        for archived in [false, true] {
            let rel = handoff_rel(id, archived);
            match self.read(&rel) {
                Ok(src) => {
                    let (fid, entries) = handoff::parse_handoff_file(&src, &rel)?;
                    if fid != id {
                        return Err(Error::Invalid(format!(
                            "{rel} frontmatter says item = {fid}"
                        )));
                    }
                    return Ok(entries);
                }
                Err(Error::NotFound { .. }) => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(Vec::new())
    }

    fn append_handoff(&self, id: ItemId, entry: &HandoffEntry) -> Result<(), Error> {
        let block = handoff::emit_entry(entry)?;
        let live = handoff_rel(id, false);
        let archived = handoff_rel(id, true);
        // the entry lands wherever the file already lives — an
        // archived file stays archived; a new file starts live
        let existing = match self.read(&live) {
            Ok(src) => Some((live.clone(), src)),
            Err(Error::NotFound { .. }) => match self.read(&archived) {
                Ok(src) => Some((archived, src)),
                Err(Error::NotFound { .. }) => None,
                Err(e) => return Err(e),
            },
            Err(e) => return Err(e),
        };
        match existing {
            Some((rel, src)) => {
                // validate before appending — a malformed file is an
                // error, not a pile-on
                let (fid, _) = handoff::parse_handoff_file(&src, &rel)?;
                if fid != id {
                    return Err(Error::Invalid(format!(
                        "{rel} frontmatter says item = {fid}"
                    )));
                }
                let mut out = src;
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push('\n');
                out.push_str(&block);
                write_atomic(&self.path(&rel), &out)
            }
            None => write_atomic(
                &self.path(&live),
                &handoff::emit_handoff_file(&id, std::slice::from_ref(entry))?,
            ),
        }
    }

    fn list_handoff_files(&self) -> Result<Vec<HandoffFile>, Error> {
        let mut out = Vec::new();
        for archived in [false, true] {
            let mut dir = self.path(handoff::HANDOFFS_DIR);
            if archived {
                dir = dir.join(handoff::HANDOFFS_ARCHIVE);
            }
            let read = match std::fs::read_dir(&dir) {
                Ok(r) => r,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(Error::Io(e)),
            };
            for entry in read {
                let entry = entry.map_err(Error::Io)?;
                if entry.path().is_dir() {
                    continue; // `archive/` itself and stray dirs
                }
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                if let Some(id) = handoff::parse_handoff_filename(name) {
                    out.push(HandoffFile { id, archived });
                }
            }
        }
        out.sort_by_key(|h| (h.id, h.archived));
        Ok(out)
    }
}

/// `handoffs/<ID>.md` or `handoffs/archive/<ID>.md`.
fn handoff_rel(id: ItemId, archived: bool) -> String {
    let name = handoff::handoff_filename(&id);
    if archived {
        format!(
            "{}/{}/{name}",
            handoff::HANDOFFS_DIR,
            handoff::HANDOFFS_ARCHIVE
        )
    } else {
        format!("{}/{name}", handoff::HANDOFFS_DIR)
    }
}

/// Write `contents` to `path` atomically: same-dir tmp file +
/// `rename(2)`; fsync before the rename. Parent dirs are created.
pub fn write_atomic(path: &Utf8Path, contents: &str) -> Result<(), Error> {
    use std::io::Write;
    // bare filenames have an empty parent; "." mkdirs fine either way
    let parent = path
        .parent()
        .filter(|p| !p.as_str().is_empty())
        .unwrap_or_else(|| Utf8Path::new("."));
    std::fs::create_dir_all(parent).map_err(Error::Io)?;
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let write = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if write.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    write.map_err(Error::Io)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use arca_core::{Item, Section, Status};

    fn store() -> (tempfile::TempDir, MdStore) {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().join("proj")).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        (dir, MdStore::new(root))
    }

    #[test]
    fn meta_default_then_save() {
        let (_d, s) = store();
        let m = s.meta().unwrap();
        assert_eq!(m.name, "proj"); // dir-name fallback
        assert_eq!(m.next.get(ItemKind::Task), 1);
        let mut m = m;
        m.name = "arca".into();
        s.save_meta(&m).unwrap();
        assert_eq!(s.meta().unwrap().name, "arca");
    }

    #[test]
    fn todo_roundtrip_and_default() {
        let (_d, s) = store();
        let mut doc = s.todo().unwrap();
        assert_eq!(doc.repo, "proj");
        assert!(doc.sections.is_empty());
        doc.sections.push(Section {
            name: Some("Now".into()),
            items: vec![Item::new(ItemId::new(ItemKind::Task, 1), "x")],
        });
        s.save_todo(&doc).unwrap();
        assert_eq!(s.todo().unwrap(), doc);
        assert!(s.root().join("TODO.md").exists());
    }

    #[test]
    fn backlog_default_and_save() {
        let (_d, s) = store();
        assert!(s.backlog().unwrap().sections.is_empty());
        let mut doc = BacklogDoc::default();
        doc.sections.push(Section {
            name: None,
            items: vec![Item::new(ItemId::new(ItemKind::Task, 3), "later")
                .status(Status::Deferred)],
        });
        s.save_backlog(&doc).unwrap();
        assert_eq!(s.backlog().unwrap(), doc);
    }

    #[test]
    fn item_lifecycle() {
        let (_d, s) = store();
        let file = ItemFile::new(Item::new(ItemId::new(ItemKind::Issue, 3), "Memory leak"))
            .body("## Description\n\nbody");
        s.save_item(&file).unwrap();
        assert!(s.root().join("issues/issue-0003-memory-leak.md").exists());
        assert_eq!(s.item(ItemId::new(ItemKind::Issue, 3)).unwrap(), file);

        // title change → slug change → rename
        let mut file2 = file.clone();
        file2.meta.title = "Leak fix".into();
        s.save_item(&file2).unwrap();
        assert!(s.root().join("issues/issue-0003-leak-fix.md").exists());
        assert!(!s.root().join("issues/issue-0003-memory-leak.md").exists());
        assert_eq!(s.list_items(ItemKind::Issue).unwrap().len(), 1);

        s.remove_item(ItemId::new(ItemKind::Issue, 3)).unwrap();
        assert!(matches!(
            s.item(ItemId::new(ItemKind::Issue, 3)),
            Err(Error::NotFound { .. })
        ));
        assert!(matches!(
            s.remove_item(ItemId::new(ItemKind::Issue, 3)),
            Err(Error::NotFound { .. })
        ));
    }

    #[test]
    fn archive_lifecycle() {
        let (_d, s) = store();
        let file =
            ItemFile::new(Item::new(ItemId::new(ItemKind::Issue, 3), "Leak").status(Status::Done));
        s.save_item(&file).unwrap();
        let live = s.root().join("issues/issue-0003-leak.md");
        assert!(live.exists());

        s.archive_item(ItemId::new(ItemKind::Issue, 3)).unwrap();
        assert!(!live.exists());
        let archived = s.root().join("issues/archive/issue-0003-leak.md");
        assert!(archived.exists());

        // excluded from live lists, present in archived + item lookup
        assert!(s.list_items(ItemKind::Issue).unwrap().is_empty());
        assert_eq!(s.list_archived(ItemKind::Issue).unwrap().len(), 1);
        assert_eq!(s.item(ItemId::new(ItemKind::Issue, 3)).unwrap(), file);
        assert!(s.is_archived(ItemId::new(ItemKind::Issue, 3)).unwrap());
        assert!(!s.is_archived(ItemId::new(ItemKind::Issue, 9)).unwrap());
        // the archived id still feeds the registry — never reused
        assert!(s
            .all_ids()
            .unwrap()
            .contains(&ItemId::new(ItemKind::Issue, 3)));

        // already-archived and missing both error
        assert!(matches!(
            s.archive_item(ItemId::new(ItemKind::Issue, 3)),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            s.archive_item(ItemId::new(ItemKind::Issue, 9)),
            Err(Error::NotFound { .. })
        ));
        assert!(matches!(
            s.archive_item(ItemId::new(ItemKind::Task, 1)),
            Err(Error::NotFound { .. })
        ));

        // live + archive same-id → duplicate error, not a silent shadow
        s.save_item(&file).unwrap();
        assert!(matches!(
            s.item(ItemId::new(ItemKind::Issue, 3)),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn item_rejects_tasks_and_bad_kind() {
        let (_d, s) = store();
        let t = ItemFile::new(Item::new(ItemId::new(ItemKind::Task, 1), "x"));
        assert!(s.save_item(&t).is_err());
        assert!(s.item(ItemId::new(ItemKind::Task, 1)).is_err());
        assert!(s.list_items(ItemKind::Task).unwrap().is_empty());
    }

    #[test]
    fn alloc_reconciles_against_files() {
        let (_d, s) = store();
        // hand-placed items + stale meta
        std::fs::create_dir_all(s.root().join("issues")).unwrap();
        std::fs::write(
            s.root().join("issues/issue-0007-x.md"),
            "+++\nid = \"I-007\"\ntitle = \"x\"\nstatus = \"open\"\n+++\n",
        )
        .unwrap();
        let mut doc = TodoDoc::new("proj");
        doc.sections.push(Section {
            name: None,
            items: vec![Item::new(ItemId::new(ItemKind::Task, 40), "old")],
        });
        s.save_todo(&doc).unwrap();
        assert_eq!(
            s.alloc_id(ItemKind::Task).unwrap(),
            ItemId::new(ItemKind::Task, 41)
        );
        assert_eq!(
            s.alloc_id(ItemKind::Issue).unwrap(),
            ItemId::new(ItemKind::Issue, 8)
        );
        assert_eq!(
            s.alloc_id(ItemKind::Plan).unwrap(),
            ItemId::new(ItemKind::Plan, 1)
        );
        // persisted
        assert_eq!(s.meta().unwrap().next.get(ItemKind::Task), 42);
    }

    #[test]
    fn all_ids_collects() {
        let (_d, s) = store();
        let mut doc = TodoDoc::new("proj");
        doc.sections.push(Section {
            name: None,
            items: vec![Item::new(ItemId::new(ItemKind::Task, 5), "x")],
        });
        s.save_todo(&doc).unwrap();
        s.save_item(&ItemFile::new(Item::new(
            ItemId::new(ItemKind::Proposal, 2),
            "p",
        )))
        .unwrap();
        let ids = s.all_ids().unwrap();
        assert!(ids.contains(&ItemId::new(ItemKind::Task, 5)));
        assert!(ids.contains(&ItemId::new(ItemKind::Proposal, 2)));
    }

    #[test]
    fn index_passthrough() {
        let (_d, s) = store();
        assert!(matches!(s.index_md(), Err(Error::NotFound { .. })));
        s.save_index("# Index\n\n- links\n").unwrap();
        assert_eq!(s.index_md().unwrap(), "# Index\n\n- links\n");
    }

    #[test]
    fn atomic_write_creates_dirs_and_leaves_no_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("sub/deep/f.md")).unwrap();
        write_atomic(&path, "hi").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hi");
        // no tmp litter
        let entries: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn unreadable_file_is_io_error_not_notfound() {
        use std::os::unix::fs::PermissionsExt;
        let (_d, s) = store();
        for f in ["TODO.md", "meta.toml", "BACKLOG.md"] {
            let p = s.root().join(f);
            std::fs::write(&p, "x").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
        }
        assert!(matches!(s.todo(), Err(Error::Io(_))));
        assert!(matches!(s.meta(), Err(Error::Io(_))));
        assert!(matches!(s.backlog(), Err(Error::Io(_))));
    }

    #[test]
    fn malformed_files_propagate_parse_errors() {
        let (_d, s) = store();
        std::fs::write(s.root().join("TODO.md"), "nope\n").unwrap();
        std::fs::write(s.root().join("BACKLOG.md"), "nope\n").unwrap();
        std::fs::write(s.root().join("meta.toml"), "nope = [").unwrap();
        assert!(matches!(s.todo(), Err(Error::Parse { .. })));
        assert!(matches!(s.backlog(), Err(Error::Parse { .. })));
        assert!(matches!(s.meta(), Err(Error::Parse { .. })));
        // all_ids skips the broken docs, keeps file-backed ids
        s.save_item(&ItemFile::new(Item::new(
            ItemId::new(ItemKind::Issue, 1),
            "ok",
        )))
        .unwrap();
        let ids = s.all_ids().unwrap();
        assert_eq!(ids.len(), 1);
    }

    #[test]
    fn item_paths_edge_cases() {
        use std::os::unix::ffi::OsStrExt;
        let (_d, s) = store();
        // `issues` as a regular file → read_dir fails (not NotFound)
        std::fs::write(s.root().join("issues"), "x").unwrap();
        assert!(matches!(s.list_items(ItemKind::Issue), Err(Error::Io(_))));
        // non-UTF8 filename is skipped
        std::fs::remove_file(s.root().join("issues")).unwrap();
        let dir = s.root().join("issues");
        std::fs::create_dir_all(&dir).unwrap();
        let weird = dir
            .as_std_path()
            .join(std::ffi::OsStr::from_bytes(b"\xff.md"));
        std::fs::write(&weird, "x").unwrap();
        assert!(s.list_items(ItemKind::Issue).unwrap().is_empty());
    }

    #[test]
    fn duplicate_item_files_error() {
        let (_d, s) = store();
        let f = ItemFile::new(Item::new(ItemId::new(ItemKind::Issue, 3), "a"));
        s.save_item(&f).unwrap();
        let mut f2 = f.clone();
        f2.meta.title = "b".into();
        // same id, different slug → two files for I-003
        std::fs::write(
            s.root().join("issues/issue-0003-other.md"),
            crate::itemfile::emit_item_file(&f2).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            s.item(ItemId::new(ItemKind::Issue, 3)),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn write_atomic_cleans_tmp_on_failure() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let sub = Utf8PathBuf::from_path_buf(dir.path().join("ro")).unwrap();
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::set_permissions(sub.as_std_path(), std::fs::Permissions::from_mode(0o555))
            .unwrap();
        let target = sub.join("f.md");
        assert!(write_atomic(&target, "hi").is_err());
        // tmp file cleaned up
        let left: Vec<_> = std::fs::read_dir(&sub).unwrap().collect();
        assert!(left.is_empty());
        std::fs::set_permissions(sub.as_std_path(), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }

    fn entry(note: &str) -> HandoffEntry {
        HandoffEntry {
            date: arca_core::date::parse("2026-10-29").unwrap(),
            actor: arca_core::Actor::new("devin").unwrap(),
            session: Some("cli:test".into()),
            note: note.into(),
        }
    }

    #[test]
    fn handoff_append_read_list() {
        let (_d, s) = store();
        let t1 = ItemId::new(ItemKind::Task, 1);
        let i3 = ItemId::new(ItemKind::Issue, 3);
        // no file → empty vec, not an error
        assert!(s.read_handoffs(t1).unwrap().is_empty());
        assert!(s.list_handoff_files().unwrap().is_empty());

        // first append creates the canonical file
        s.append_handoff(t1, &entry("Done: a. Next: b.")).unwrap();
        let path = s.root().join("handoffs/T-001.md");
        let src = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            src,
            "+++\nitem = \"T-001\"\n+++\n\n# Handoffs — T-001\n\n## 2026-10-29 · devin · session:cli:test\nDone: a. Next: b.\n"
        );

        // second append is append-only — existing bytes untouched
        let mut e2 = entry("note two");
        e2.session = None;
        s.append_handoff(t1, &e2).unwrap();
        let src2 = std::fs::read_to_string(&path).unwrap();
        assert!(src2.starts_with(&src), "{src2}");
        assert_eq!(
            src2[src.len()..].trim_end(),
            "\n## 2026-10-29 · devin\nnote two"
        );
        let entries = s.read_handoffs(t1).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].note, "note two");
        assert_eq!(entries[1].session, None);

        // a second item gets its own file; list covers both
        s.append_handoff(i3, &entry("issue note")).unwrap();
        assert!(s.root().join("handoffs/I-003.md").exists());
        let files = s.list_handoff_files().unwrap();
        assert_eq!(
            files,
            vec![
                HandoffFile {
                    id: t1,
                    archived: false
                },
                HandoffFile {
                    id: i3,
                    archived: false
                },
            ]
        );
        assert_eq!(s.read_handoffs(i3).unwrap()[0].note, "issue note");
    }

    #[test]
    fn handoff_append_errors() {
        let (_d, s) = store();
        let t1 = ItemId::new(ItemKind::Task, 1);
        // frontmatter id mismatch
        std::fs::create_dir_all(s.root().join("handoffs")).unwrap();
        std::fs::write(
            s.root().join("handoffs/T-001.md"),
            "+++\nitem = \"T-002\"\n+++\n\n# Handoffs — T-002\n",
        )
        .unwrap();
        assert!(matches!(s.read_handoffs(t1), Err(Error::Invalid(_))));
        assert!(matches!(
            s.append_handoff(t1, &entry("x")),
            Err(Error::Invalid(_))
        ));
        // malformed existing file → parse error, nothing appended
        std::fs::write(s.root().join("handoffs/T-001.md"), "garbage\n").unwrap();
        assert!(matches!(
            s.append_handoff(t1, &entry("x")),
            Err(Error::Parse { .. })
        ));
        // unemittable entry
        let bad = HandoffEntry {
            session: Some("a·b".into()),
            ..entry("x")
        };
        assert!(s.append_handoff(t1, &bad).is_err());
    }

    #[test]
    fn handoff_archive_scope() {
        let (_d, s) = store();
        let t1 = ItemId::new(ItemKind::Task, 1);
        // a file under handoffs/archive/ is read + listed
        std::fs::create_dir_all(s.root().join("handoffs/archive")).unwrap();
        std::fs::write(
            s.root().join("handoffs/archive/T-001.md"),
            "+++\nitem = \"T-001\"\n+++\n\n# Handoffs — T-001\n\n## 2026-01-01 · luis\nold\n",
        )
        .unwrap();
        let entries = s.read_handoffs(t1).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].actor.as_str(), "luis");
        let files = s.list_handoff_files().unwrap();
        assert_eq!(
            files,
            vec![HandoffFile {
                id: t1,
                archived: true
            }]
        );
        // appending keeps it in the archive scope
        s.append_handoff(t1, &entry("new note")).unwrap();
        assert!(!s.root().join("handoffs/T-001.md").exists());
        assert_eq!(s.read_handoffs(t1).unwrap().len(), 2);
        // live file wins when both exist
        std::fs::write(
            s.root().join("handoffs/T-001.md"),
            "+++\nitem = \"T-001\"\n+++\n\n# Handoffs — T-001\n\n## 2026-02-02 · devin\nlive\n",
        )
        .unwrap();
        assert_eq!(s.read_handoffs(t1).unwrap().len(), 1);
        let files = s.list_handoff_files().unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f.archived));
        assert!(files.iter().any(|f| !f.archived));
    }

    #[test]
    fn handoff_io_errors_propagate() {
        let (_d, s) = store();
        let t1 = ItemId::new(ItemKind::Task, 1);
        // a directory where the handoff file should be → Io, not
        // NotFound, on both read paths
        std::fs::create_dir_all(s.root().join("handoffs/T-001.md")).unwrap();
        assert!(matches!(s.read_handoffs(t1), Err(Error::Io(_))));
        assert!(matches!(
            s.append_handoff(t1, &entry("n")),
            Err(Error::Io(_))
        ));
        // same, but only the archived slot
        std::fs::remove_dir_all(s.root().join("handoffs/T-001.md")).unwrap();
        std::fs::create_dir_all(s.root().join("handoffs/archive/T-001.md")).unwrap();
        assert!(matches!(s.read_handoffs(t1), Err(Error::Io(_))));
        assert!(matches!(
            s.append_handoff(t1, &entry("n")),
            Err(Error::Io(_))
        ));
    }

    #[test]
    fn append_handoff_normalizes_missing_trailing_newline() {
        let (_d, s) = store();
        let t1 = ItemId::new(ItemKind::Task, 1);
        std::fs::create_dir_all(s.root().join("handoffs")).unwrap();
        std::fs::write(
            s.root().join("handoffs/T-001.md"),
            "+++\nitem = \"T-001\"\n+++\n\n# Handoffs — T-001\n\n## 2026-01-01 · luis\nnote",
        )
        .unwrap();
        s.append_handoff(t1, &entry("next")).unwrap();
        assert_eq!(s.read_handoffs(t1).unwrap().len(), 2);
    }

    #[test]
    fn list_handoff_files_io_and_nonutf8() {
        use std::os::unix::ffi::OsStrExt;
        let (_d, s) = store();
        // `handoffs` is a regular file → read_dir fails non-NotFound
        std::fs::write(s.root().join("handoffs"), "x").unwrap();
        assert!(matches!(s.list_handoff_files(), Err(Error::Io(_))));
        // non-UTF8 filenames are skipped, not fatal
        std::fs::remove_file(s.root().join("handoffs")).unwrap();
        let dir = s.root().join("handoffs");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.as_std_path().join(std::ffi::OsStr::from_bytes(b"\xff")),
            "x",
        )
        .unwrap();
        std::fs::write(dir.join("stray.txt"), "x").unwrap();
        assert!(s.list_handoff_files().unwrap().is_empty());
    }
}

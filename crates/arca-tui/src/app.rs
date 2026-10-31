//! The TUI model — boards, cursor, modes, and the ledger ops every
//! key maps to. Mirrors pm's design: two board tabs over
//! `arca_ops::items::Ledger`, a flattened row list, and writes that
//! end in `items::commit`.

use std::path::{Path, PathBuf};

use arca_core::Ledger;
use arca_core::{Item, ItemFile, ItemId, ItemKind, Status};
use arca_ops::{discover, items};
use arca_store::Store;
use arca_store_md::MdStore;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

/// Which ledger doc the board tab shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Board {
    /// `TODO.md` — committed work.
    Todo,
    /// `BACKLOG.md` — deferred items.
    Backlog,
    /// File-backed kinds — `issues/`/`proposals/`/`plans/`/`adrs/`,
    /// grouped under kind headers. Read-only for now: task verbs
    /// (`done`/`defer`/…) don't apply here.
    Files,
}

impl Board {
    /// Tab label.
    pub fn label(self) -> &'static str {
        match self {
            Board::Todo => "TODO",
            Board::Backlog => "BACKLOG",
            Board::Files => "ITEMS",
        }
    }

    /// Tab order for `h`/`l` and the `1`/`2`/`3` jumps.
    pub const ALL: [Board; 3] = [Board::Todo, Board::Backlog, Board::Files];
}

/// UI mode — like pm's: the board handles keys differently per mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Browsing.
    Normal,
    /// Full-pane item preview (`Enter`, `esc` back).
    Preview,
    /// `a` — typing a new item title; `Enter` commits, `esc` drops.
    Add,
    /// `/` — live filter over the current board's items; `Enter`
    /// keeps the filter, `esc` clears it.
    Filter,
    /// `:` — ex-style command line (`:q` `:w` `:e` `:<id>` `:go <id>`
    /// `:set id`); `Enter` runs it, `esc` drops the line.
    Command,
    /// `?` — keybind overlay; `esc`/`?`/`q` closes.
    Help,
}

/// A flattened display row — section headers aren't selectable.
#[derive(Debug, Clone)]
pub enum Row {
    /// `## <section>` header line.
    Header(String),
    /// An item line; `depth` indents children under their parent.
    Item { item: Box<Item>, depth: usize },
}

/// The whole TUI state.
pub struct Model {
    proj: discover::Project,
    store: MdStore,
    ledger: Ledger,
    no_commit: bool,
    /// Active tab.
    pub board: Board,
    /// Display rows for `board`, rebuilt on reload/switch.
    pub rows: Vec<Row>,
    /// Index into `rows` (always on an `Item` row when any exist).
    pub sel: usize,
    /// Current mode.
    pub mode: Mode,
    /// `a` input buffer.
    pub input: String,
    /// `/` live filter — narrows visible item rows when non-empty.
    pub filter: String,
    /// `:` command-line buffer — separate from `input` so an `a`
    /// draft isn't clobbered.
    pub cmdline: String,
    /// Item rows render their `T-NNN` id span — `:set id` /
    /// `:set noid` (vim aliases `numbers`/`nonumbers` accepted).
    pub show_ids: bool,
    /// Status-bar message (last action / error).
    pub status: String,
    /// Editor to open on the next loop turn (`E`).
    pub pending_editor: Option<PathBuf>,
    /// File-backed items for the `Files` board.
    files: Vec<ItemFile>,
    /// Multi-key prefix: `g` armed for `gg`, `d` for `dd`.
    pending: Option<char>,
    /// Board-list rect from the last frame — `render::draw` writes
    /// it so mouse clicks and wheel hits map to rows.
    pub board_area: Rect,
    /// First row index visible in `board_area` — the scroll offset
    /// the `List` widget chose to keep `sel` on screen.
    pub viewport_top: usize,
    /// Tab-bar rect when the frame is tall enough to draw one —
    /// for click-to-switch hit-testing.
    pub tabs_area: Option<Rect>,
}

impl Model {
    /// Discovery + first ledger load. `dir`/`root` override like the
    /// CLI globals.
    pub fn open(dir: Option<&Path>, root: Option<&Path>, no_commit: bool) -> Result<Self, String> {
        let (proj, store) = items::open(dir, root)?;
        let ledger = items::load_ledger(&store)?;
        let mut m = Model {
            proj,
            store,
            ledger,
            no_commit,
            board: Board::Todo,
            rows: Vec::new(),
            sel: 0,
            mode: Mode::Normal,
            input: String::new(),
            filter: String::new(),
            cmdline: String::new(),
            show_ids: true,
            status: String::new(),
            pending_editor: None,
            files: Vec::new(),
            pending: None,
            board_area: Rect::default(),
            viewport_top: 0,
            tabs_area: None,
        };
        m.load_files();
        m.rebuild_rows();
        m.restore_session();
        Ok(m)
    }

    /// Pull file-backed items for the `Files` board.
    fn load_files(&mut self) {
        self.files.clear();
        for kind in [
            ItemKind::Issue,
            ItemKind::Proposal,
            ItemKind::Plan,
            ItemKind::Adr,
        ] {
            if let Ok(mut v) = self.store.list_items(kind) {
                self.files.append(&mut v);
            }
        }
    }

    /// Body of a file-backed item (preview on the `Files` board).
    pub fn file_body(&self, id: ItemId) -> Option<&str> {
        self.files
            .iter()
            .find(|f| f.meta.id == id)
            .map(|f| f.body.as_str())
    }

    /// Reload both docs from disk (`R`, post-editor).
    pub fn reload(&mut self) {
        let sel_id = self.selected_id();
        match items::load_ledger(&self.store) {
            Ok(ledger) => {
                self.ledger = ledger;
                self.rebuild_rows();
                self.reselect(sel_id);
            }
            Err(e) => self.status = format!("reload: {e}"),
        }
    }

    /// Flatten the active board's sections into rows.
    fn rebuild_rows(&mut self) {
        self.rows.clear();
        match self.board {
            Board::Files => {
                for (kind, label) in [
                    (ItemKind::Issue, "issues"),
                    (ItemKind::Proposal, "proposals"),
                    (ItemKind::Plan, "plans"),
                    (ItemKind::Adr, "adrs"),
                ] {
                    let group: Vec<Item> = self
                        .files
                        .iter()
                        .filter(|f| f.meta.id.kind() == kind)
                        .map(|f| f.meta.clone())
                        .collect();
                    if group.is_empty() {
                        continue;
                    }
                    self.rows.push(Row::Header(label.to_string()));
                    push_items(&group, 0, &mut self.rows);
                }
            }
            _ => {
                let sections = match self.board {
                    Board::Todo => &self.ledger.todo.sections,
                    Board::Backlog => &self.ledger.backlog.sections,
                    Board::Files => unreachable!(),
                };
                for s in sections {
                    if let Some(name) = &s.name {
                        self.rows.push(Row::Header(name.clone()));
                    }
                    push_items(&s.items, 0, &mut self.rows);
                }
            }
        }
        self.apply_filter();
        self.sel = self.first_item_row().unwrap_or(0);
    }

    /// `/` filter — keep only item rows matching the query
    /// (title/id/labels, case-insensitive); headers drop out.
    fn apply_filter(&mut self) {
        if self.filter.is_empty() {
            return;
        }
        let q = self.filter.to_lowercase();
        self.rows.retain(|r| {
            matches!(r, Row::Item { item, .. }
                if item.title.to_lowercase().contains(&q)
                    || item.id.to_string().to_lowercase().contains(&q)
                    || item.labels.iter().any(|l| l.to_lowercase().contains(&q)))
        });
    }

    /// After a mutation: land the cursor back on `id` (or the
    /// nearest item row).
    fn reselect(&mut self, id: Option<ItemId>) {
        if let Some(id) = id {
            if let Some(i) = self
                .rows
                .iter()
                .position(|r| matches!(r, Row::Item { item, .. } if item.id == id))
            {
                self.sel = i;
                return;
            }
        }
        self.sel = self
            .first_item_row()
            .unwrap_or(0)
            .min(self.rows.len().saturating_sub(1));
    }

    fn first_item_row(&self) -> Option<usize> {
        self.rows.iter().position(|r| matches!(r, Row::Item { .. }))
    }

    /// The id under the cursor.
    fn selected_id(&self) -> Option<ItemId> {
        self.selected().map(|i| i.id)
    }

    /// The item under the cursor.
    pub fn selected(&self) -> Option<&Item> {
        match self.rows.get(self.sel) {
            Some(Row::Item { item, .. }) => Some(item),
            _ => None,
        }
    }

    /// The file `E` opens — the board doc, or the selected item's
    /// file on the `Files` board.
    fn board_path(&self) -> PathBuf {
        match self.board {
            Board::Todo => self.proj.worktree.join("TODO.md"),
            Board::Backlog => self.proj.worktree.join("BACKLOG.md"),
            Board::Files => self
                .selected()
                .map(|i| self.proj.worktree.join(items::item_rel_path(i)))
                .unwrap_or_else(|| self.proj.worktree.clone()),
        }
    }

    /// Handle one key. Returns `true` to quit.
    pub fn key(&mut self, k: KeyEvent) -> Result<bool, String> {
        match self.mode {
            Mode::Preview => return self.key_preview(k),
            Mode::Add => return self.key_add(k),
            Mode::Filter => return self.key_filter(k),
            Mode::Command => return self.key_command(k),
            Mode::Help => return self.key_help(k),
            Mode::Normal => {}
        }
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let was_g = self.pending == Some('g');
        let was_d = self.pending == Some('d');
        self.pending = None;
        match k.code {
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return Ok(true),
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Char('1') => self.switch_board(0),
            KeyCode::Char('2') => self.switch_board(1),
            KeyCode::Char('3') => self.switch_board(2),
            KeyCode::Char('h') | KeyCode::Left => self.step_board(-1),
            KeyCode::Char('l') | KeyCode::Right => self.step_board(1),
            KeyCode::Char('j') | KeyCode::Down if alt => self.reorder(1),
            KeyCode::Char('k') | KeyCode::Up if alt => self.reorder(-1),
            KeyCode::Char('j') | KeyCode::Down => self.move_sel(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_sel(-1),
            KeyCode::Char('g') if was_g => self.sel = self.first_item_row().unwrap_or(0),
            KeyCode::Char('g') => self.pending = Some('g'),
            KeyCode::Char('G') => self.sel = self.last_item_row().unwrap_or(0),
            KeyCode::Char('d') if was_d => self.done_selected()?,
            KeyCode::Char('d') => self.pending = Some('d'),
            KeyCode::Char('D') => self.defer_selected()?,
            KeyCode::Char('p') => self.promote_selected()?,
            KeyCode::Char(' ') => self.done_selected()?,
            KeyCode::Char('a') => {
                if self.board == Board::Files {
                    self.status = "add is a TODO/BACKLOG op — `arca <kind> new` for files".into();
                } else {
                    self.mode = Mode::Add;
                    self.input.clear();
                }
            }
            KeyCode::Char('/') => self.mode = Mode::Filter,
            KeyCode::Char(':') => {
                self.mode = Mode::Command;
                self.cmdline.clear();
            }
            KeyCode::Char('F') => {
                self.filter.clear();
                self.rebuild_rows();
            }
            KeyCode::Enter => {
                if self.selected().is_some() {
                    self.mode = Mode::Preview;
                }
            }
            KeyCode::Char('E') => self.pending_editor = Some(self.board_path()),
            KeyCode::Char('R') => self.reload(),
            KeyCode::Char('?') => self.mode = Mode::Help,
            _ => {}
        }
        Ok(false)
    }

    fn key_preview(&mut self, k: KeyEvent) -> Result<bool, String> {
        match k.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.mode = Mode::Normal,
            _ => {}
        }
        Ok(false)
    }

    /// `/` filter — chars narrow rows live; `Enter` keeps the
    /// filter set, `esc` clears it.
    fn key_filter(&mut self, k: KeyEvent) -> Result<bool, String> {
        match k.code {
            KeyCode::Esc => {
                self.filter.clear();
                self.mode = Mode::Normal;
                self.rebuild_rows();
            }
            KeyCode::Enter => self.mode = Mode::Normal,
            KeyCode::Backspace => {
                self.filter.pop();
                self.rebuild_rows();
            }
            KeyCode::Char(c) => {
                self.filter.push(c);
                self.rebuild_rows();
            }
            _ => {}
        }
        Ok(false)
    }

    /// `:` command line — chars append, `Enter` runs `exec_command`,
    /// `esc` drops the line.
    fn key_command(&mut self, k: KeyEvent) -> Result<bool, String> {
        match k.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let line = std::mem::take(&mut self.cmdline);
                self.mode = Mode::Normal;
                return self.exec_command(&line);
            }
            KeyCode::Backspace => {
                self.cmdline.pop();
            }
            KeyCode::Char(c) => self.cmdline.push(c),
            _ => {}
        }
        Ok(false)
    }

    /// Run one `:` line — first word is the command, the rest its
    /// args. v1 keeps the set small: `q`/`q!` quit, `w` reloads
    /// from disk, `e` opens the board doc in `$EDITOR`, `go <id>`
    /// and a bare `<id>` jump the cursor, `set id`/`noid` toggles
    /// the id column (`numbers`/`nonumbers` also accepted).
    fn exec_command(&mut self, line: &str) -> Result<bool, String> {
        let line = line.trim();
        let mut parts = line.splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or("");
        let arg = parts.next().unwrap_or("").trim();
        match cmd {
            "" => {}
            "q" | "q!" => return Ok(true),
            "w" => self.reload(),
            "e" => self.pending_editor = Some(self.board_path()),
            "go" => match arg.parse::<ItemId>() {
                Ok(id) => self.jump_to(id),
                Err(_) => self.status = format!(":go expects an id, got {arg:?}"),
            },
            "set" => match arg {
                "id" | "numbers" => self.show_ids = true,
                "noid" | "nonumbers" => self.show_ids = false,
                _ => self.status = ":set takes id / noid".into(),
            },
            _ => match cmd.parse::<ItemId>() {
                Ok(id) => self.jump_to(id),
                Err(_) => self.status = format!("unknown command :{cmd}"),
            },
        }
        Ok(false)
    }

    /// `:<id>` / `:go <id>` — land the cursor on the item when it's
    /// a row on this board.
    fn jump_to(&mut self, id: ItemId) {
        if self
            .rows
            .iter()
            .any(|r| matches!(r, Row::Item { item, .. } if item.id == id))
        {
            self.reselect(Some(id));
        } else {
            self.status = format!("{id}: not on this board");
        }
    }

    /// Help is a dead end — every key but the closers is swallowed.
    fn key_help(&mut self, k: KeyEvent) -> Result<bool, String> {
        match k.code {
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => self.mode = Mode::Normal,
            _ => {}
        }
        Ok(false)
    }

    fn key_add(&mut self, k: KeyEvent) -> Result<bool, String> {
        match k.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let title = self.input.trim().to_string();
                self.mode = Mode::Normal;
                if !title.is_empty() {
                    self.add_item(&title)?;
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) => self.input.push(c),
            _ => {}
        }
        Ok(false)
    }

    /// Mouse input (Normal mode only — acting on clicks under an
    /// overlay or mid-`a`/`/` edit would be surprising): the wheel
    /// moves the cursor over the board list, a left click picks a
    /// board tab on the tab row or selects the item row under it.
    pub fn mouse(&mut self, ev: MouseEvent) {
        if self.mode != Mode::Normal {
            return;
        }
        let pos = (ev.column, ev.row).into();
        match ev.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                if self.board_area.contains(pos) {
                    let delta = if ev.kind == MouseEventKind::ScrollUp {
                        -1
                    } else {
                        1
                    };
                    self.move_sel(delta);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(tabs) = self.tabs_area {
                    if ev.row == tabs.y {
                        if let Some(i) =
                            crate::render::tab_hit(tabs.width, ev.column.saturating_sub(tabs.x))
                        {
                            self.switch_board(i);
                        }
                        return;
                    }
                }
                if self.board_area.contains(pos) {
                    let row = self.viewport_top + usize::from(ev.row - self.board_area.y);
                    self.select_row(row);
                }
            }
            _ => {}
        }
    }

    fn last_item_row(&self) -> Option<usize> {
        self.rows
            .iter()
            .rposition(|r| matches!(r, Row::Item { .. }))
    }

    /// j/k — skip header rows, wrap nothing (pm stops at edges).
    fn move_sel(&mut self, delta: isize) {
        let mut i = self.sel as isize;
        loop {
            i += delta;
            if i < 0 || i as usize >= self.rows.len() {
                break;
            }
            if matches!(self.rows[i as usize], Row::Item { .. }) {
                self.sel = i as usize;
                break;
            }
        }
    }

    /// Mouse click — land the cursor on `row`, but only on items:
    /// headers aren't selectable and misses keep `sel` put.
    pub fn select_row(&mut self, row: usize) {
        if matches!(self.rows.get(row), Some(Row::Item { .. })) {
            self.sel = row;
        }
    }

    fn switch_board(&mut self, idx: usize) {
        self.board = Board::ALL[idx];
        self.rebuild_rows();
    }

    fn step_board(&mut self, delta: isize) {
        let idx = (Board::ALL
            .iter()
            .position(|b| *b == self.board)
            .unwrap_or(0) as isize
            + delta)
            .rem_euclid(Board::ALL.len() as isize) as usize;
        self.switch_board(idx);
    }

    /// `Space`/`dd` — the `todo done` verb: walk to done, remove the
    /// line (and its subtree), one commit.
    fn done_selected(&mut self) -> Result<(), String> {
        let id = match self.selected_id() {
            Some(id) => id,
            None => return Ok(()),
        };
        let loc = self
            .ledger
            .locate(id)
            .ok_or_else(|| format!("{id}: no such task"))?;
        if loc.backlog {
            self.status = format!("{id} is deferred — `p` promotes it first");
            return Ok(());
        }
        let mut item = self.ledger.take(&loc);
        items::walk_to(&mut item, Status::Done)?;
        items::save_ledger(&self.store, &self.ledger)?;
        items::commit(
            &self.proj.worktree,
            &format!("chore: done {id}"),
            self.no_commit,
        )?;
        self.status = format!("done {id} — {}", item.title);
        self.rebuild_rows();
        self.reselect(None);
        Ok(())
    }

    /// `D` — `todo defer`: park the subtree into BACKLOG.md.
    fn defer_selected(&mut self) -> Result<(), String> {
        let id = match self.selected_id() {
            Some(id) => id,
            None => return Ok(()),
        };
        let loc = self
            .ledger
            .locate(id)
            .ok_or_else(|| format!("{id}: no such task"))?;
        if loc.backlog {
            self.status = format!("{id} is already deferred");
            return Ok(());
        }
        let mut item = self.ledger.take(&loc);
        items::walk_to(&mut item, Status::Deferred)?;
        items::walk_descendants_to(&mut item, Status::Deferred)?;
        self.ledger.put_backlog(item, None);
        items::save_ledger(&self.store, &self.ledger)?;
        items::commit(
            &self.proj.worktree,
            &format!("chore: defer {id}"),
            self.no_commit,
        )?;
        self.status = format!("deferred {id} → BACKLOG.md");
        self.rebuild_rows();
        self.reselect(None);
        Ok(())
    }

    /// `p` — `backlog promote`: unpark the subtree into TODO.md.
    fn promote_selected(&mut self) -> Result<(), String> {
        let id = match self.selected_id() {
            Some(id) => id,
            None => return Ok(()),
        };
        let loc = self
            .ledger
            .locate(id)
            .ok_or_else(|| format!("{id}: no such task"))?;
        if !loc.backlog {
            self.status = format!("{id} isn't in BACKLOG.md");
            return Ok(());
        }
        let mut item = self.ledger.take(&loc);
        items::walk_to(&mut item, Status::Open)?;
        items::walk_descendants_to(&mut item, Status::Open)?;
        self.ledger.put_todo(item, None);
        items::save_ledger(&self.store, &self.ledger)?;
        items::commit(
            &self.proj.worktree,
            &format!("chore: promote {id}"),
            self.no_commit,
        )?;
        self.status = format!("promoted {id} → TODO.md");
        self.rebuild_rows();
        self.reselect(Some(id));
        Ok(())
    }

    /// `Alt+↑/↓` — sibling reorder (pm's backlog priority move).
    fn reorder(&mut self, delta: isize) {
        let Some(id) = self.selected_id() else {
            return;
        };
        let Some(mut loc) = self.ledger.locate(id) else {
            return;
        };
        if self.ledger.move_within(&mut loc, delta) {
            if let Err(e) = items::save_ledger(&self.store, &self.ledger).and_then(|_| {
                items::commit(
                    &self.proj.worktree,
                    &format!("chore: reorder {id}"),
                    self.no_commit,
                )
                .map(|_| ())
            }) {
                self.status = format!("reorder {id}: {e}");
                self.reload();
            } else {
                self.status = format!("reordered {id}");
                self.rebuild_rows();
                self.reselect(Some(id));
            }
        }
    }

    /// `a` Enter — `todo add`/`backlog add` for the active board.
    fn add_item(&mut self, title: &str) -> Result<(), String> {
        let id = self
            .store
            .alloc_id(ItemKind::Task)
            .map_err(|e| e.to_string())?;
        if self.board == Board::Files {
            return Err("add is a TODO/BACKLOG op".into());
        }
        let item = match self.board {
            Board::Todo => Item::new(id, title).created(items::today()),
            _ => Item::new(id, title)
                .status(Status::Deferred)
                .created(items::today()),
        };
        match self.board {
            Board::Todo => self.ledger.put_todo(item.clone(), None),
            _ => self.ledger.put_backlog(item.clone(), None),
        }
        items::save_ledger(&self.store, &self.ledger)?;
        items::commit(
            &self.proj.worktree,
            &format!("chore: add {id}"),
            self.no_commit,
        )?;
        self.status = format!("added {}", items::compact(&item));
        self.rebuild_rows();
        self.reselect(Some(id));
        Ok(())
    }
}

/// `$ARCA_HOME` → `$XDG_DATA_HOME/arca` → `~/.local/share/arca` —
/// same convention as the index.
fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("ARCA_HOME") {
        return PathBuf::from(d);
    }
    if let Some(d) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(d).join("arca");
    }
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share/arca")
}

/// Per-repo session file — `<data_dir>/ui/<hash-of-root>.toml`.
fn session_path(root: &Path) -> PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    root.hash(&mut h);
    data_dir()
        .join("ui")
        .join(format!("{:016x}.toml", h.finish()))
}

impl Model {
    /// Persist board + selected item for the next launch (`q`).
    pub fn save_session(&self) {
        let board = match self.board {
            Board::Todo => "todo",
            Board::Backlog => "backlog",
            Board::Files => "files",
        };
        let item = self
            .selected_id()
            .map(|i| i.to_string())
            .unwrap_or_default();
        let p = session_path(&self.proj.root);
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(p, format!("board = \"{board}\"\nitem = \"{item}\"\n"));
    }

    /// Reopen on the last board + item (best effort).
    fn restore_session(&mut self) {
        let Ok(src) = std::fs::read_to_string(session_path(&self.proj.root)) else {
            return;
        };
        let get = |key: &str| {
            src.lines()
                .find_map(|l| l.split_once('=').filter(|(k, _)| k.trim() == key))
                .map(|(_, v)| v.trim().trim_matches('"').to_string())
        };
        self.board = match get("board").as_deref() {
            Some("backlog") => Board::Backlog,
            Some("files") => Board::Files,
            _ => Board::Todo,
        };
        self.rebuild_rows();
        if let Some(id) = get("item").and_then(|s| s.parse::<ItemId>().ok()) {
            self.reselect(Some(id));
        }
    }
}

fn push_items(items: &[Item], depth: usize, rows: &mut Vec<Row>) {
    for item in items {
        rows.push(Row::Item {
            item: Box::new(item.clone()),
            depth,
        });
        push_items(&item.children, depth + 1, rows);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use arca_core::Section;
    use camino::Utf8PathBuf;

    fn t(n: u32) -> Item {
        Item::new(ItemId::new(ItemKind::Task, n), format!("t{n}"))
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    /// A model over a temp `project/` dir — `no_commit` keeps git
    /// out of the loop (autocommit short-circuits).
    fn model(populate: impl FnOnce(&mut Ledger)) -> Model {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("project");
        std::fs::create_dir_all(&worktree).unwrap();
        let store = MdStore::new(Utf8PathBuf::from_path_buf(worktree.clone()).unwrap());
        let mut ledger = items::load_ledger(&store).unwrap();
        populate(&mut ledger);
        let mut m = Model {
            proj: discover::Project {
                root: dir.path().to_path_buf(),
                worktree,
            },
            store,
            ledger,
            no_commit: true,
            board: Board::Todo,
            rows: Vec::new(),
            sel: 0,
            mode: Mode::Normal,
            input: String::new(),
            filter: String::new(),
            cmdline: String::new(),
            show_ids: true,
            status: String::new(),
            pending_editor: None,
            files: Vec::new(),
            pending: None,
            board_area: Rect::default(),
            viewport_top: 0,
            tabs_area: None,
        };
        m.rebuild_rows();
        // keep the tempdir alive for the test's duration
        std::mem::forget(dir);
        m
    }

    #[test]
    fn rows_flatten_sections_and_children() {
        let m = model(|l| {
            l.todo.sections.push(Section {
                name: Some("Now".into()),
                items: vec![t(1), t(2).child(t(4))],
            });
        });
        assert!(matches!(m.rows[0], Row::Header(_)));
        assert!(matches!(m.rows[1], Row::Item { ref item, depth: 0 } if item.id.number() == 1));
        assert!(matches!(m.rows[3], Row::Item { ref item, depth: 1 } if item.id.number() == 4));
        // cursor lands on the first item, not the header
        assert_eq!(m.sel, 1);
        assert_eq!(m.selected().unwrap().id.number(), 1);
    }

    #[test]
    fn j_k_skip_headers_and_stop_at_edges() {
        let mut m = model(|l| {
            l.todo.sections.push(Section::named("S"));
            l.todo.sections[0].items = vec![t(1)];
            l.todo.sections.push(Section::named("T"));
            l.todo.sections[1].items = vec![t(2)];
        });
        // rows: H S, t1, H T, t2 — j from t1 lands on t2
        m.key(key('j')).unwrap();
        assert_eq!(m.selected().unwrap().id.number(), 2);
        // edge: no wrap
        m.key(key('j')).unwrap();
        assert_eq!(m.selected().unwrap().id.number(), 2);
        m.key(key('k')).unwrap();
        assert_eq!(m.selected().unwrap().id.number(), 1);
    }

    #[test]
    fn gg_and_g_and_tab_switch() {
        let mut m = model(|l| {
            l.put_todo(t(1), None);
            l.put_todo(t(2), None);
            l.put_backlog(t(3).status(Status::Deferred), None);
        });
        m.key(key('G')).unwrap();
        assert_eq!(m.selected().unwrap().id.number(), 2);
        m.key(key('g')).unwrap();
        m.key(key('g')).unwrap();
        assert_eq!(m.selected().unwrap().id.number(), 1);
        m.key(key('2')).unwrap();
        assert_eq!(m.board, Board::Backlog);
        assert_eq!(m.selected().unwrap().id.number(), 3);
        m.key(key('1')).unwrap();
        assert_eq!(m.board, Board::Todo);
    }

    #[test]
    fn dd_remembers_the_prefix_and_dones() {
        let mut m = model(|l| l.put_todo(t(1), None));
        m.key(key('d')).unwrap();
        assert!(m.selected().is_some()); // still there — only armed
        m.key(key('d')).unwrap();
        assert!(m.selected().is_none());
        assert!(m.ledger.locate(ItemId::new(ItemKind::Task, 1)).is_none());
    }

    #[test]
    fn defer_and_promote_cross_boards() {
        let mut m = model(|l| l.put_todo(t(1), None));
        m.key(key('D')).unwrap();
        let loc = m.ledger.locate(ItemId::new(ItemKind::Task, 1)).unwrap();
        assert!(loc.backlog);
        m.key(key('2')).unwrap();
        m.key(key('p')).unwrap();
        let loc = m.ledger.locate(ItemId::new(ItemKind::Task, 1)).unwrap();
        assert!(!loc.backlog);
        assert_eq!(m.board, Board::Backlog); // board stays put
    }

    #[test]
    fn help_toggles_and_swallows_keys() {
        let mut m = model(|_| {});
        m.key(key('?')).unwrap();
        assert_eq!(m.mode, Mode::Help);
        // other keys are swallowed, not applied to the board
        m.key(key('j')).unwrap();
        assert_eq!(m.mode, Mode::Help);
        // ? closes
        m.key(key('?')).unwrap();
        assert_eq!(m.mode, Mode::Normal);
        // q closes rather than quitting
        m.key(key('?')).unwrap();
        assert!(!m.key(key('q')).unwrap());
        assert_eq!(m.mode, Mode::Normal);
        // esc closes
        m.key(key('?')).unwrap();
        m.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(m.mode, Mode::Normal);
    }

    #[test]
    fn slash_filters_rows_live() {
        let mut m = model(|l| {
            l.put_todo(t(1), None);
            l.put_todo(t(2), None);
        });
        m.key(key('/')).unwrap();
        for c in "t2".chars() {
            m.key(key(c)).unwrap();
        }
        assert_eq!(m.rows.len(), 1);
        assert_eq!(m.selected().unwrap().id.number(), 2);
        // esc clears back to the full board
        m.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(m.mode, Mode::Normal);
        assert_eq!(m.rows.len(), 2);
    }

    fn click(x: u16, y: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn click_selects_items_and_wheel_moves() {
        let mut m = model(|l| {
            l.todo.sections.push(Section::named("S"));
            l.todo.sections[0].items = vec![t(1), t(2)];
        });
        // rows: H S, t1, t2 — select_row only takes Item rows
        m.select_row(0);
        assert_eq!(m.selected().unwrap().id.number(), 1);
        m.select_row(2);
        assert_eq!(m.selected().unwrap().id.number(), 2);
        m.select_row(9); // out of range — sel stays put
        assert_eq!(m.selected().unwrap().id.number(), 2);

        // a click in the board area maps viewport y → row index
        m.board_area = Rect::new(0, 1, 80, 20);
        m.viewport_top = 0;
        m.mouse(click(0, 2)); // y 2 → rows[1] = t1
        assert_eq!(m.selected().unwrap().id.number(), 1);
        // wheel over the board moves the cursor
        m.mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(m.selected().unwrap().id.number(), 2);
        // clicks outside the board area don't move the cursor
        m.mouse(click(0, 0));
        assert_eq!(m.selected().unwrap().id.number(), 2);

        // a click on the tab row switches boards
        m.tabs_area = Some(Rect::new(0, 0, 80, 1));
        m.mouse(click(8, 0)); // " BACKLOG " starts at col 8
        assert_eq!(m.board, Board::Backlog);
    }

    #[test]
    fn add_mode_commits_title() {
        let mut m = model(|_| {});
        m.key(key('a')).unwrap();
        for c in "ship it".chars() {
            m.key(key(c)).unwrap();
        }
        m.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(m.mode, Mode::Normal);
        let item = m.selected().unwrap();
        assert_eq!(item.title, "ship it");
    }

    /// Drive `:` + `line` + `Enter` — returns the key() result so
    /// `:q` tests can assert the quit flag.
    fn cmd(m: &mut Model, line: &str) -> bool {
        m.key(key(':')).unwrap();
        assert_eq!(m.mode, Mode::Command);
        for c in line.chars() {
            m.key(key(c)).unwrap();
        }
        m.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap()
    }

    #[test]
    fn colon_w_reloads_from_disk() {
        let mut m = model(|l| l.put_todo(t(1), None));
        // another writer lands a task while the board is open
        let mut other = items::load_ledger(&m.store).unwrap();
        other.put_todo(t(1), None);
        other.put_todo(t(2), None);
        items::save_ledger(&m.store, &other).unwrap();
        assert_eq!(m.rows.len(), 1);
        assert!(!cmd(&mut m, "w"));
        assert_eq!(m.mode, Mode::Normal);
        assert_eq!(m.rows.len(), 2);
    }

    #[test]
    fn colon_id_and_go_jump_the_cursor() {
        let mut m = model(|l| {
            l.put_todo(t(1), None);
            l.put_todo(t(2), None);
        });
        cmd(&mut m, "T-2");
        assert_eq!(m.selected().unwrap().id.number(), 2);
        cmd(&mut m, "go T-1");
        assert_eq!(m.selected().unwrap().id.number(), 1);
        // a valid id that's not a row here reports instead of moving
        cmd(&mut m, "T-9");
        assert_eq!(m.selected().unwrap().id.number(), 1);
        assert_eq!(m.status, "T-009: not on this board");
    }

    #[test]
    fn colon_set_toggles_ids() {
        let mut m = model(|l| l.put_todo(t(1), None));
        assert!(m.show_ids);
        cmd(&mut m, "set noid");
        assert!(!m.show_ids);
        cmd(&mut m, "set id");
        assert!(m.show_ids);
        cmd(&mut m, "set bogus");
        assert_eq!(m.status, ":set takes id / noid");
    }

    #[test]
    fn colon_unknown_and_esc_cancel() {
        let mut m = model(|l| l.put_todo(t(1), None));
        assert!(!cmd(&mut m, "bogus arg"));
        assert_eq!(m.mode, Mode::Normal);
        assert_eq!(m.status, "unknown command :bogus");
        // esc drops a half-typed line and re-entry starts empty
        m.key(key(':')).unwrap();
        m.key(key('w')).unwrap();
        m.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(m.mode, Mode::Normal);
        m.key(key(':')).unwrap();
        assert_eq!(m.cmdline, "");
    }

    #[test]
    fn colon_q_quits() {
        let mut m = model(|_| {});
        assert!(cmd(&mut m, "q"));
        let mut m = model(|_| {});
        assert!(cmd(&mut m, "q!"));
    }
}

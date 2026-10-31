//! arca-tui — interactive board TUI for a repo's `project/`
//! worktree.
//!
//! A thin front-end over `arca-ops`: every mutation goes through the
//! same ledger ops the CLI verbs use, so writes and commits are
//! identical to `arca todo done` et al. Ported from pm's Bubble Tea
//! reference (`9aia/pm`, `docs/tui.md` there) — the keymap is
//! vim-style: `j/k` move, `h/l`/`1`/`2`/`3` boards, `Space`/`dd` done,
//! `D` defer, `p` promote, `Alt+↑/↓` reorder, `a` add, `/` filter,
//! `:` command line (`:q` `:w` `:e` `:<id>` `:set id`), `Enter`/`esc`
//! preview, `E` $EDITOR, `R` reload, `?` help, `q` quit — plus mouse:
//! the wheel scrolls, a click selects an item row or a board tab.

mod app;
mod render;

use std::io::{self, Stdout};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

/// Restore the terminal on any exit path.
struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
    }
}

/// Launch the TUI. `dir`/`root` are the same discovery overrides the
/// CLI takes; `no_commit` suppresses the per-mutation commit.
pub fn run(dir: Option<&Path>, root: Option<&Path>, no_commit: bool) -> Result<(), String> {
    let mut model = app::Model::open(dir, root, no_commit)?;
    enable_raw_mode().map_err(|e| format!("raw mode: {e}"))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .map_err(|e| format!("alt screen: {e}"))?;
    let _guard = Guard;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(stdout)).map_err(|e| format!("terminal: {e}"))?;

    loop {
        if let Some(path) = model.pending_editor.take() {
            run_editor(&mut terminal, &path)?;
            model.reload();
        }
        terminal
            .draw(|f| render::draw(f, &mut model))
            .map_err(|e| format!("draw: {e}"))?;
        if event::poll(Duration::from_millis(250)).map_err(|e| e.to_string())? {
            match event::read().map_err(|e| e.to_string())? {
                Event::Key(k) if matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                    if model.key(k)? {
                        model.save_session();
                        break;
                    }
                }
                Event::Mouse(m) => model.mouse(m),
                Event::Resize(..) => {}
                _ => {}
            }
        }
    }
    Ok(())
}

/// Suspend the TUI, run `$EDITOR path`, resume.
fn run_editor(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    path: &Path,
) -> Result<(), String> {
    disable_raw_mode().map_err(|e| e.to_string())?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )
    .map_err(|e| e.to_string())?;
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    let status = Command::new(&editor).arg(path).status();
    enable_raw_mode().map_err(|e| e.to_string())?;
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture
    )
    .map_err(|e| e.to_string())?;
    terminal.clear().map_err(|e| e.to_string())?;
    status.map_err(|e| format!("$EDITOR {editor}: {e}"))?;
    Ok(())
}

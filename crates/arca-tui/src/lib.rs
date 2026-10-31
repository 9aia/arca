//! arca-tui — interactive vault browser (`arca ui`).
//!
//! A thin, read-only front-end over `arca-ops`: env tabs come from
//! `[envs]` routing, the ref list and detail pane come from
//! `list_records` — metadata only. The posture is *browse the
//! surface, never spill*: the single way a value renders is `r`
//! then `r` again — a deliberate two-step that goes through
//! `Ops::get_in`, emitting the same `get` materialization audit
//! event the CLI does. Keys: `j/k`/arrows move, `tab`/`1-9`
//! switch env, `/` filters, `?` help, `R` reload, `q` quits.
//!
//! A vault that won't open renders as an error line in the pane —
//! the browser never panics on backend failure, and a non-tty
//! stdout declines up front instead of drawing into a pipe.

mod app;
mod render;

use std::io::{self, IsTerminal};
use std::time::Duration;

use crossterm::event::{self, Event, KeyEventKind};
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
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

/// Panic hook — restore the terminal *before* the default hook
/// prints, or the panic is erased by the alt-screen exit and the
/// shell is left in raw mode.
fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        prev(info);
    }));
}

/// Launch the browser on the loaded [`arca_ops::Ops`]. The caller
/// (`arca ui`) emits the op-scoped audit event on success — the
/// TUI itself only emits via `get_in` reveals.
pub fn run(ops: &arca_ops::Ops) -> io::Result<()> {
    if !io::stdout().is_terminal() {
        return Err(io::Error::other(
            "arca ui needs a terminal — stdout is not a tty",
        ));
    }
    let mut model = app::Model::open(ops);
    enable_raw_mode()?;
    // Guard first — raw mode is on from here, so every fallible
    // step below must restore on `?`.
    let _guard = Guard;
    install_panic_hook();
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    loop {
        terminal.draw(|f| render::draw(f, &mut model))?;
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(k) = event::read()? {
                if matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) && model.key(k) {
                    break;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use arca_core::{Env, Ref, Secret};
    use arca_vault::Vault;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;

    /// An Ops over a real temp age vault with secrets stored —
    /// exercises the real load path, not a stub.
    fn ops_with_secrets(envs: &[(&str, &[&str])]) -> (tempfile::TempDir, arca_ops::Ops) {
        let home = tempfile::tempdir().unwrap();
        let id_path = home.path().join("identity.txt");
        let vault_path = home.path().join("vault.age");
        arca_vault_age::AgeVault::ensure_identity(&id_path).unwrap();
        let vault = arca_vault_age::AgeVault::open(&vault_path, &id_path, &[]).unwrap();
        for (env, refs) in envs {
            for r in *refs {
                Vault::put(
                    &vault,
                    &Env::new(env).unwrap(),
                    &Ref::new(r).unwrap(),
                    Secret::new("fake-tui-value"),
                )
                .unwrap();
            }
        }
        // Ops fields are private — point it at a temp arca.toml so
        // Ops::load resolves like the CLI does.
        let cfg_path = home.path().join("arca.toml");
        std::fs::write(
            &cfg_path,
            format!(
                "[vaults.local]\nkind = \"age\"\npath = {:?}\nidentity = {:?}\n",
                vault_path, id_path
            ),
        )
        .unwrap();
        let ops = arca_ops::Ops::load(arca_ops::LoadOpts {
            config_path: Some(cfg_path.to_string_lossy().into_owned()),
            env: None,
            actor: Some("tui-test".into()),
        })
        .unwrap();
        (home, ops)
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    /// Buffer → text rows for substring assertions.
    fn screen(term: &Terminal<TestBackend>) -> String {
        let buf = term.backend().buffer();
        let w = buf.area.width as usize;
        buf.content()
            .iter()
            .map(|c| c.symbol())
            .collect::<Vec<_>>()
            .chunks(w)
            .map(|row| row.concat())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_refs_and_never_values() {
        let (_h, ops) = ops_with_secrets(&[("dev", &["arca://billing/stripe", "arca://db/pg"])]);
        let mut m = app::Model::open(&ops);
        let backend = TestBackend::new(100, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render::draw(f, &mut m)).unwrap();
        let s = screen(&term);
        assert!(s.contains("arca://billing/stripe"), "{s}");
        assert!(s.contains("arca://db/pg"), "{s}");
        assert!(s.contains("sealed"), "{s}");
        assert!(!s.contains("fake-tui-value"), "{s}");
    }

    #[test]
    fn env_tabs_and_filter_and_help() {
        let (_h, ops) = ops_with_secrets(&[
            ("dev", &["arca://billing/stripe"]),
            ("prod", &["arca://prodonly/key"]),
        ]);
        let mut m = app::Model::open(&ops);
        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        // prod tab via digit key — tabs are dev(1) staging(2) prod(3)
        m.key(key('3'));
        term.draw(|f| render::draw(f, &mut m)).unwrap();
        let s = screen(&term);
        assert!(s.contains("arca://prodonly/key"), "{s}");
        assert!(!s.contains("stripe"), "{s}");
        // filter narrows
        m.key(key('/'));
        for c in "nomatch".chars() {
            m.key(key(c));
        }
        m.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        term.draw(|f| render::draw(f, &mut m)).unwrap();
        assert!(
            screen(&term).contains("nothing matches"),
            "{}",
            screen(&term)
        );
        // help overlay
        m.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        m.key(key('?'));
        term.draw(|f| render::draw(f, &mut m)).unwrap();
        assert!(screen(&term).contains("keys"), "{}", screen(&term));
        // q quits
        m.key(key('?'));
        assert!(m.key(key('q')));
    }

    #[test]
    fn broken_vault_renders_error_not_panic() {
        let home = tempfile::tempdir().unwrap();
        let cfg_path = home.path().join("arca.toml");
        // Points at a vault path that can't open — missing identity.
        std::fs::write(
            &cfg_path,
            format!(
                "[vaults.local]\nkind = \"age\"\npath = {:?}\nidentity = {:?}\n",
                home.path().join("vault.age"),
                home.path().join("no-identity.txt"),
            ),
        )
        .unwrap();
        let ops = arca_ops::Ops::load(arca_ops::LoadOpts {
            config_path: Some(cfg_path.to_string_lossy().into_owned()),
            env: None,
            actor: Some("tui-test".into()),
        })
        .unwrap();
        let mut m = app::Model::open(&ops);
        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        term.draw(|f| render::draw(f, &mut m)).unwrap();
        let s = screen(&term);
        assert!(s.contains("vault not readable"), "{s}");
    }

    #[test]
    fn non_tty_declines() {
        // Test stdout in this harness is a pipe — run() must refuse.
        let (_h, ops) = ops_with_secrets(&[("dev", &["arca://x/y"])]);
        assert!(run(&ops).is_err());
    }
}

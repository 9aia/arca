//! arca-tui — interactive board TUI over `arca-ops`. Standalone
//! entry; `arca ui` embeds the same `arca_tui::run`.

fn main() {
    if let Err(e) = arca_tui::run(None, None, false) {
        eprintln!("arca-tui: error: {e}");
        std::process::exit(1);
    }
}

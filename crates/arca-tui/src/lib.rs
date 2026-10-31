//! arca-tui — interactive vault browser for the local vault.
//!
//! A thin front-end over `arca-ops`: every call goes through the
//! same ops the CLI verbs use, so the TUI can never diverge on
//! resolution or audit. The real ratatui interface is phase-5
//! polish (PL-001); for now [`run`] lists the refs in the selected
//! env and gets out of the way — honest, working, and
//! feature-gated in the CLI.

use std::io;

/// v1 stand-in: print the refs stored in the selected env.
/// Interactive ratatui browsing lands in phase 5.
pub fn run(ops: &arca_ops::Ops) -> io::Result<()> {
    let (env, refs) = ops.list(None).map_err(io::Error::other)?;
    eprintln!("arca ui — interactive mode lands in phase 5; env {env} holds:");
    for r in refs {
        println!("{r}");
    }
    Ok(())
}

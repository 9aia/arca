//! arca-ops — the I/O shell every front-end calls.
//!
//! `arca-core` is pure domain (ledger doc surgery, state-machine
//! walks, item metadata); this crate is everything that touches the
//! outside world: `discover` resolves the `project/` worktree via
//! git, `ops` owns the scaffold + commit-per-mutation machinery, and
//! `items` is store open/save + the verbs' shared mutation/output
//! tail — re-exporting the core symbols so call sites read
//! `items::X` for either. `arca` (CLI) is a thin clap shell over
//! this; `arca-tui` calls the same functions, so both front-ends
//! produce identical writes and identical commits.

pub mod discover;
pub mod items;
pub mod ops;

//! arca — project management, grown not built. A mycorrhizal
//! network for your agents: tasks, signals, handoffs.
//!
//! Agent-first PM: markdown in `project/` is canonical and
//! human-editable; every deterministic write goes through this CLI
//! (auto-commit per verb, `--json` everywhere). The skill teaches
//! agents the verbs; `arca manifest`/`arca schema` are the
//! self-describing surface.
//!
//! Phase 6 wires continuity — `handoff`/`resume` over append-only
//! `handoffs/<ID>.md` files (one per item, any kind); sepia hooks
//! call them at session boundaries. Phase 7 lands the Jira-parity
//! slice — `epic:`/`ms:` item metadata + grouping views,
//! `absorb` importers (comment sweep, Jira CSV, GitHub dumps).

use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod absorb;
mod adr;
mod agentmd;
mod backlog;
mod board;
mod check;
mod commit;
mod continuity;
mod digest;
mod discover;
mod group;
mod indexcmd;
mod init;
mod issue;
mod itemcmd;
mod items;
mod migrate;
mod ops;
mod org;
mod plan;
mod proposal;
mod report;
mod schema;
mod serve;
mod status;
mod templates;
mod todo;

/// arca — agent-first project management for a repo's `project/`
/// worktree.
#[derive(Parser)]
#[command(name = "arca", version, about, long_about = None)]
struct Cli {
    /// Emit JSON (accepted everywhere; default output for machines).
    #[arg(long, global = true)]
    json: bool,

    /// Skip the per-verb commit (also suppressed by $ARCA_NO_COMMIT).
    #[arg(long, global = true)]
    no_commit: bool,

    /// Project worktree to act on, overriding discovery.
    #[arg(long, global = true, value_name = "PATH")]
    dir: Option<PathBuf>,

    /// Repo root to act on, overriding discovery (the worktree is
    /// `<root>/project`).
    #[arg(long, global = true, value_name = "PATH")]
    root: Option<PathBuf>,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Capabilities + protocol version — agent self-discovery.
    Manifest,
    /// The file/state spec, machine-readable JSON Schema.
    Schema,
    /// Scaffold `project/`: orphan branch + worktree + .gitignore +
    /// seed files + the managed AGENTS.md agent-awareness block.
    /// Idempotent — existing worktree and AGENTS.md prose are
    /// preserved, only the marked block is (re)written.
    Init,
    /// Validate schemas + state machine, repair file/index drift;
    /// also validates the managed AGENTS.md block on the repo
    /// root. Exit 1 with findings; `--fix` applies safe repairs +
    /// commits.
    Check {
        /// Repair drift in place instead of only reporting.
        #[arg(long)]
        fix: bool,
    },
    /// Convert legacy ledgers to v2: `pm/` → `project/`, or
    /// normalize files already on the `project` branch.
    Migrate,
    /// Stage + commit worktree changes (conventional message).
    Commit {
        /// Commit message (default: `chore: update project ledger`).
        #[arg(short, long)]
        message: Option<String>,
    },
    /// TODO.md tasks — add / done / defer / list.
    Todo {
        #[command(subcommand)]
        cmd: todo::Cmd,
    },
    /// BACKLOG.md parked items — add / promote / list.
    Backlog {
        #[command(subcommand)]
        cmd: backlog::Cmd,
    },
    /// Tracked issues, one file each under `issues/`.
    Issue {
        #[command(subcommand)]
        cmd: issue::Cmd,
    },
    /// Design proposals under `proposals/` (`P-NNN`).
    Proposal {
        #[command(subcommand)]
        cmd: proposal::Cmd,
    },
    /// Execution plans under `plans/` (`PL-NNN`).
    Plan {
        #[command(subcommand)]
        cmd: plan::Cmd,
    },
    /// Architecture decision records under `adrs/` (`ADR-NNNN`).
    Adr {
        #[command(subcommand)]
        cmd: adr::Cmd,
    },
    /// Kind-agnostic item verbs — show / set.
    Item {
        #[command(subcommand)]
        cmd: itemcmd::Cmd,
    },
    /// Attach resumable state at session end — appends a dated
    /// entry to `handoffs/<ID>.md` (any item kind, live or
    /// archived; append-only).
    Handoff {
        /// Any item id (`T-NNN`, `I-NNN`, `P-NNN`, `ADR-NNNN`,
        /// `PL-NNN`).
        id: arca_core::ItemId,
        /// Handoff note — done/next/open. `-` reads stdin.
        #[arg(long, value_name = "TEXT|-")]
        note: String,
        /// Who wrote it (`devin`, `human:luis`, …; default `cli`).
        #[arg(long)]
        actor: Option<arca_core::Actor>,
        /// Session provenance — a sepia session id, `cli:<pid>`,
        /// any id string.
        #[arg(long)]
        session: Option<String>,
    },
    /// A session ended with work still claimed/in-progress/
    /// blocked — revert the item to `open` + `interrupted`
    /// handoff, one commit. Session-lifecycle hooks (sepia, CLI
    /// session-end) call this; arca never detects sessions itself.
    Interrupt {
        /// Any item id (`T-NNN`, `I-NNN`, `P-NNN`, `ADR-NNNN`,
        /// `PL-NNN`).
        id: arca_core::ItemId,
        /// Extra context appended to the `interrupted` note.
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,
        /// Who interrupted it (default `cli`).
        #[arg(long)]
        actor: Option<arca_core::Actor>,
        /// Session provenance — a sepia session id, `cli:<pid>`,
        /// any id string.
        #[arg(long)]
        session: Option<String>,
    },
    /// Minimal context block to restart work on an item — the
    /// item, `blocks:` dep titles, latest handoffs, open
    /// checklist. The single-item `status --prompt`.
    Resume {
        /// Any item id.
        id: arca_core::ItemId,
        /// Only the latest N handoff entries (default 10).
        #[arg(long)]
        last: Option<usize>,
    },
    /// Cheap digest of the work ledger. `--prompt` = token-sized.
    Status {
        /// Render for an agent prompt (minimal tokens).
        #[arg(long)]
        prompt: bool,
    },
    /// Kanban of the ledger — Backlog | Open | Claimed | In
    /// Progress | Blocked. `--md` emits a markdown table.
    Board {
        /// Emit a markdown table (for docs/READMEs).
        #[arg(long)]
        md: bool,
        /// Show every card — no row cap.
        #[arg(long)]
        all: bool,
        /// Only items in this epic.
        #[arg(long)]
        epic: Option<String>,
        /// Only items in this milestone.
        #[arg(long)]
        milestone: Option<String>,
    },
    /// Items grouped by epic — bare lists epics with counts, a
    /// name renders that epic's board (`epic v2` ≡ `board
    /// --epic v2`).
    Epic {
        /// Epic name — omit for the all-epics summary.
        name: Option<String>,
        /// Emit a markdown table (with a name).
        #[arg(long)]
        md: bool,
        /// Show every card — no row cap (with a name).
        #[arg(long)]
        all: bool,
    },
    /// Items grouped by milestone — bare lists milestones with
    /// counts, a name renders that milestone's board.
    Milestone {
        /// Milestone name — omit for the all-milestones summary.
        name: Option<String>,
        /// Emit a markdown table (with a name).
        #[arg(long)]
        md: bool,
        /// Show every card — no row cap (with a name).
        #[arg(long)]
        all: bool,
    },
    /// Import work into the ledger — TODO/FIXME comments, Jira
    /// CSV exports, GitHub issue dumps.
    Absorb {
        #[command(subcommand)]
        cmd: absorb::Cmd,
    },
    /// Aging + velocity + blocked — how the ledger is trending.
    Report,
    /// The derived index: register repos, query/rebuild snapshots.
    Index {
        #[command(subcommand)]
        cmd: indexcmd::Cmd,
    },
    /// Cross-repo views over the index (auto-rebuilds stale repos).
    Org {
        #[command(subcommand)]
        cmd: org::Cmd,
    },
    /// Interactive board TUI (ratatui) — TODO/BACKLOG tabs, item
    /// preview, writes through the same ops the verbs use.
    #[cfg(feature = "tui")]
    Ui,
    /// Sequential request/response verb surface on stdio —
    /// JSON-RPC 2.0, one line in / one line out. For embedding
    /// (peregrin spawns one per repo); stdin IS the wire, so
    /// no served method reads content from it.
    Serve,
    /// Print the CLI reference as markdown (docs pipeline).
    #[command(hide = true, name = "gen-cli-docs")]
    GenCliDocs,
}

fn manifest() -> serde_json::Value {
    serde_json::json!({
        "name": "arca",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": 1,
        "description": env!("CARGO_PKG_DESCRIPTION"),
        "repository": env!("CARGO_PKG_REPOSITORY"),
        "capabilities": {
            "store": "markdown",
            "index": "redb",
            "protocol": 1,
            "serve": {
                "protocol": 1,
                "transport": "stdio",
                "wire": "jsonrpc-2.0",
            },
        },
        "verbs": [
            "manifest", "schema",
            "init", "check", "migrate", "commit", "absorb",
            "todo", "backlog", "issue", "proposal", "plan", "adr",
            "item show", "item set",
            "epic", "milestone",
            "status", "board", "report",
            "index add", "index remove", "index list", "index rebuild",
            "org status", "org board", "org init",
            "handoff", "resume", "interrupt", "ui", "serve",
        ],
        "implemented": [
            "manifest", "gen-cli-docs", "schema",
            "init", "check", "migrate", "commit", "absorb",
            "todo", "backlog", "issue", "proposal", "plan", "adr",
            "item show", "item set",
            "epic", "milestone",
            "status", "board", "report",
            "index add", "index remove", "index list", "index rebuild",
            "org status", "org board", "org init",
            "handoff", "resume", "interrupt",
            "ui", "serve",
        ],
        "notes": "markdown in project/ is canonical; every verb is one \
                  atomic commit on the `project` branch; the redb index \
                  under $ARCA_HOME|~/.local/share/arca is a disposable \
                  derived cache",
    })
}

fn main() {
    // Restore conventional-Unix SIGPIPE behavior (die silently on
    // EPIPE) — `println!` would otherwise panic when piped to a
    // reader that closes early (e.g. `arca board | head`).
    sigpipe::reset();
    let cli = Cli::parse();
    let json = cli.json;
    let no_commit = cli.no_commit;
    let dir = cli.dir.clone();
    let root = cli.root.clone();
    let result: Result<i32, String> = match cli.command {
        Cmd::Manifest => {
            ops::print_json(&manifest());
            Ok(0)
        }
        Cmd::GenCliDocs => {
            println!("{}", clap_markdown::help_markdown::<Cli>());
            Ok(0)
        }
        Cmd::Init => init::run(dir, root, json, no_commit),
        Cmd::Check { fix } => check::run(dir, root, fix, json, no_commit),
        Cmd::Migrate => migrate::run(dir, root, json, no_commit),
        Cmd::Commit { message } => commit::run(dir, root, message, json, no_commit),
        Cmd::Todo { cmd } => todo::run(cmd, dir, root, json, no_commit),
        Cmd::Backlog { cmd } => backlog::run(cmd, dir, root, json, no_commit),
        Cmd::Issue { cmd } => issue::run(cmd, dir, root, json, no_commit),
        Cmd::Proposal { cmd } => proposal::run(cmd, dir, root, json, no_commit),
        Cmd::Plan { cmd } => plan::run(cmd, dir, root, json, no_commit),
        Cmd::Adr { cmd } => adr::run(cmd, dir, root, json, no_commit),
        Cmd::Item { cmd } => itemcmd::run(cmd, dir, root, json, no_commit),
        Cmd::Handoff {
            id,
            note,
            actor,
            session,
        } => continuity::handoff(id, &note, actor, session, dir, root, json, no_commit),
        Cmd::Interrupt {
            id,
            note,
            actor,
            session,
        } => continuity::interrupt(
            id,
            note.as_deref(),
            actor,
            session,
            dir,
            root,
            json,
            no_commit,
        ),
        Cmd::Resume { id, last } => continuity::resume(id, last, dir, root, json),
        Cmd::Schema => schema::run(),
        Cmd::Status { prompt } => status::run(dir, root, prompt, json),
        Cmd::Board {
            md,
            all,
            epic,
            milestone,
        } => board::run(dir, root, md, all, epic, milestone, json),
        Cmd::Epic { name, md, all } => {
            group::run(group::Group::Epic, name, md, all, dir, root, json)
        }
        Cmd::Milestone { name, md, all } => {
            group::run(group::Group::Milestone, name, md, all, dir, root, json)
        }
        Cmd::Absorb { cmd } => absorb::run(cmd, dir, root, json, no_commit),
        Cmd::Report => report::run(dir, root, json),
        Cmd::Index { cmd } => indexcmd::run(cmd, json),
        Cmd::Org { cmd } => org::run(cmd, json, no_commit),
        #[cfg(feature = "tui")]
        Cmd::Ui => arca_tui::run(dir.as_deref(), root.as_deref(), no_commit).map(|_| 0),
        Cmd::Serve => serve::run(dir, root, no_commit),
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            if json {
                ops::print_json(&serde_json::json!({"error": e}));
            } else {
                eprintln!("arca: error: {e}");
            }
            std::process::exit(2);
        }
    }
}

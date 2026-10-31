//! arca-store-md — the markdown impl of [`arca_store::Store`], and
//! the owner of the `project/` file spec. Strict parse (spans on
//! every error), canonical emit (byte-identical for the same model,
//! `emit → parse → emit` fixed point).
//!
//! ## The grammar
//!
//! ```text
//! TODO.md      # TODO — <repo> · ## <section> · - [ ] T-NNN <title> · <tokens>
//! BACKLOG.md   # Backlog · ## <section>? · - T-NNN <title> · <tokens>  (deferred)
//! meta.toml    name = "<repo>" · [next] t|i|p|adr|pl = <n>
//! <dir>/<file> +++ <toml frontmatter> +++ · <markdown body>
//! index.md     verbatim passthrough (generated views land later)
//! ```
//!
//! Item-line tokens (`·`-separated, after the title): `status:`,
//! `pri:` (low|med|high|crit), `owner:`, `created:` (YYYY-MM-DD),
//! `#label`s, `blocks:T-NNN[,…]`, `epic:<name>`, `ms:<name>`,
//! `note:<free text>` (the dedup/provenance slot absorb fills).
//! Canonical order is that order; defaults (`status:open`/
//! `deferred`, `pri:med`, unset owner/created/labels/blocks/
//! epic/ms/note) are omitted.
//!
//! Engine: `markdown` (wooorm markdown-rs) parses everything to
//! mdast for structure + spans. Emit is canonical, hand-written
//! line layout; `mdast_util_to_markdown` provides inline escaping,
//! verified by re-parse with a local escaper as fallback — 0.0.2's
//! escaper can't emit GFM `[ ]` checkboxes and corrupts sequences
//! like `· #_x` (byte/char index drift).
//!
//! Item files: `issues/issue-NNNN-slug.md`,
//! `proposals/proposal-NNNN-slug.md`, `adrs/NNNN-slug.md`,
//! `plans/pl-NNNN-slug.md` — `+++` TOML frontmatter + verbatim body.
//!
//! Handoff files: `handoffs/<ID>.md` — one append-only file per
//! item (any kind): `+++ item +++`, `# Handoffs — <ID>` title,
//! `## <date> · <actor>[ · session:<id>]` entries + verbatim
//! notes; `handoffs/archive/` holds files whose item was removed.

mod backlog;
mod error;
mod handoff;
mod itemfile;
mod legacy;
mod line;
mod mdx;
mod meta;
mod store;
mod todo;

pub use backlog::{emit_backlog, parse_backlog};
pub use error::{EmitError, ParseError};
pub use handoff::{
    emit_entry, emit_handoff_file, handoff_filename, parse_handoff_file, parse_handoff_filename,
    HANDOFFS_ARCHIVE, HANDOFFS_DIR,
};
pub use itemfile::{
    emit_item_file, frontmatter_id, item_dir, item_filename, parse_item_file, parse_item_filename,
};
pub use legacy::{parse_legacy_backlog, parse_legacy_todo, LegacyDoc, LegacyItem, LegacySection};
pub use mdx::code_spans;
pub use meta::{emit_meta, parse_meta};
pub use store::{write_atomic, MdStore};
pub use todo::{emit_todo, parse_todo};

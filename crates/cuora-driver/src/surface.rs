//! Embedded surface dispatch — the driver serving its own engine's
//! ops through `pavo-driver-kit`'s surface layer, no second binary
//! needed. `cli`/`mcp` argv verbs (feature-gated) plug this
//! [`Surface`] impl into `surface::cli::run` /
//! `surface::mcp::serve_stdio`.
//!
//! `op` reuses the same `cli.*` path wire queries take
//! (`ops::run_verb` re-execs `run` in a child — output stays
//! captured off the serve stream). `surface` dispatches in-process
//! through `cuora::run_from`, which owns sigpipe + the audit flush
//! exactly like the `cuora` bin — that's where the unbounded
//! (`serve`) and stdin-bound (`put`/`rotate`) verbs live, since
//! they need inherited stdio a captured op child can't provide.

use pavo_driver_kit::surface::{OpOutcome, OpSpec, Surface};
use serde_json::{Map, Value};

use crate::ops;

/// cuora's [`Surface`] — engine dispatch, already linked.
pub struct Engine;

impl Surface for Engine {
    /// Same path as the `cli.<verb>` wire query: re-exec `run` in
    /// a child, captured output → `OpOutcome`.
    fn op(&mut self, op: &OpSpec, params: &Map<String, Value>) -> OpOutcome {
        OpOutcome::from_response(&ops::run_op(&op.name, params))
    }

    /// `cuora <name> <args>` in-process — for the surfaces
    /// (`serve`/`put`/`rotate`): inherited stdio, audit flush on
    /// exit. Bounded ops never reach here.
    fn surface(&mut self, name: &str, args: &[String]) -> i32 {
        let argv = std::iter::once("cuora".to_string())
            .chain(std::iter::once(name.to_string()))
            .chain(args.iter().cloned());
        cuora::run_from(argv)
    }
}

//! `arca serve` — a sequential request/response verb surface for
//! embedding: an orchestrator (peregrin) spawns one long-lived
//! process per repo instead of one spawn per verb. A single
//! process serializes writes — no `index.lock` races — and the
//! store is re-opened per request, so the served process always
//! sees on-disk truth.
//!
//! Two abstractions keep the surface honest: [`ServeTransport`]
//! moves one raw line in and out (stdio first; a unix socket can
//! slot in untouched), and [`WireProtocol`] frames requests and
//! responses (JSON-RPC 2.0 first — the default, not the spec).
//! [`Server::dispatch`] is the verb surface itself and knows
//! neither. stdin IS the wire — no served method reads content
//! from it.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use arca_core::{Actor, ItemId, ItemKind, Priority, Status};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    backlog, board, check, continuity, digest, itemcmd, items, report, schema, status, todo,
};

// ---------------------------------------------------------------- wire

/// JSON-RPC 2.0 error codes served on the wire.
pub const PARSE_ERROR: i64 = -32700;
/// The envelope decoded but isn't a callable request (also a
/// missing `id` — notifications aren't served).
pub const INVALID_REQUEST: i64 = -32600;
/// The method isn't on the v1 surface.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// `params` didn't decode into the method's shape.
pub const INVALID_PARAMS: i64 = -32602;
/// The verb itself failed.
pub const VERB_ERROR: i64 = -32000;
/// A status-machine rejection — `data` carries
/// `{from, to, valid: []}`.
pub const INVALID_TRANSITION: i64 = -32001;

/// One decoded request — `id` is echoed back verbatim so callers
/// correlate responses on a shared pipe.
#[derive(Debug)]
pub struct Request {
    /// Wire-level correlation id.
    pub id: Value,
    /// The method name (`todo.add`, `status`, …).
    pub method: String,
    /// Method params (`null` when absent).
    pub params: Value,
}

/// One response to encode.
pub enum Response {
    /// The method's payload.
    Result {
        /// Correlation id echoed from the request.
        id: Value,
        /// The result value.
        result: Value,
    },
    /// A wire or dispatch failure.
    Error {
        /// Correlation id (`null` when the request never decoded).
        id: Value,
        /// One of the `-32xxx` codes.
        code: i64,
        /// Human/agent-readable message.
        message: String,
        /// Optional structured detail (transition targets, …).
        data: Option<Value>,
    },
}

/// A line that never decoded into a [`Request`].
#[derive(Debug)]
pub struct WireError {
    /// The error code.
    pub code: i64,
    /// What went wrong.
    pub message: String,
}

/// Request/response framing — the abstraction point. JSON-RPC
/// 2.0 is the default impl; [`Server::serve`] doesn't know it.
pub trait WireProtocol {
    /// Decode one raw line into a request.
    fn parse(&self, line: &str) -> Result<Request, WireError>;
    /// Encode one response as a single line.
    fn encode(&self, response: &Response) -> String;
}

/// JSON-RPC 2.0 — one request/response object per line.
pub struct JsonRpc2;

impl WireProtocol for JsonRpc2 {
    fn parse(&self, line: &str) -> Result<Request, WireError> {
        let v: Value = serde_json::from_str(line).map_err(|e| WireError {
            code: PARSE_ERROR,
            message: format!("parse error: {e}"),
        })?;
        let invalid = |message: &str| WireError {
            code: INVALID_REQUEST,
            message: message.to_string(),
        };
        let obj = v
            .as_object()
            .ok_or_else(|| invalid("request is not a JSON object"))?;
        let method = obj
            .get("method")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
            .ok_or_else(|| invalid("missing or invalid `method`"))?
            .to_string();
        let id = obj
            .get("id")
            .filter(|id| !id.is_null())
            .cloned()
            .ok_or_else(|| invalid("missing `id` — notifications aren't served"))?;
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        Ok(Request { id, method, params })
    }

    fn encode(&self, response: &Response) -> String {
        match response {
            Response::Result { id, result } => {
                json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
            }
            Response::Error {
                id,
                code,
                message,
                data,
            } => {
                let mut e = json!({"code": code, "message": message});
                if let Some(d) = data {
                    e["data"] = d.clone();
                }
                json!({"jsonrpc": "2.0", "id": id, "error": e}).to_string()
            }
        }
    }
}

// ---------------------------------------------------------------- transport

/// One request line in, one response line out — the transport
/// abstraction.
pub trait ServeTransport {
    /// The next raw request line — `Ok(None)` on clean EOF.
    fn read_request(&mut self) -> Result<Option<String>, String>;
    /// Write one raw response line (and flush).
    fn write_response(&mut self, line: &str) -> Result<(), String>;
}

/// stdin/stdout — `BufRead` lines in, `writeln!` + flush out.
pub struct StdioTransport<R, W> {
    reader: R,
    writer: W,
}

impl<R, W> StdioTransport<R, W> {
    /// Wrap a reader/writer pair.
    pub fn new(reader: R, writer: W) -> Self {
        StdioTransport { reader, writer }
    }
}

impl<R: BufRead, W: Write> ServeTransport for StdioTransport<R, W> {
    fn read_request(&mut self) -> Result<Option<String>, String> {
        let mut line = String::new();
        let n = self
            .reader
            .read_line(&mut line)
            .map_err(|e| format!("read request: {e}"))?;
        if n == 0 {
            return Ok(None);
        }
        Ok(Some(line.trim_end_matches(['\n', '\r']).to_string()))
    }

    fn write_response(&mut self, line: &str) -> Result<(), String> {
        writeln!(self.writer, "{line}")
            .and_then(|()| self.writer.flush())
            .map_err(|e| format!("write response: {e}"))
    }
}

// ---------------------------------------------------------------- params

/// `params` (or absent) → a typed struct — bad shape is -32602.
fn parse_params<T: serde::de::DeserializeOwned>(v: &Value) -> Result<T, Fail> {
    let v = if v.is_null() { json!({}) } else { v.clone() };
    serde_json::from_value(v).map_err(|e| Fail::BadParams(format!("invalid params: {e}")))
}

#[derive(Deserialize)]
struct StatusParams {
    #[serde(default)]
    prompt: bool,
}

#[derive(Deserialize)]
struct BoardParams {
    epic: Option<String>,
    milestone: Option<String>,
}

#[derive(Deserialize)]
struct IdParams {
    id: ItemId,
    last: Option<usize>,
}

/// `--status/--owner/--label/--limit/--archived` — every
/// `*.list` method's params.
#[derive(Deserialize)]
struct ListParams {
    status: Option<Status>,
    owner: Option<String>,
    #[serde(default)]
    label: Vec<String>,
    limit: Option<usize>,
    #[serde(default)]
    archived: bool,
}

impl ListParams {
    fn filter(&self) -> items::Filter {
        items::Filter {
            status: self.status,
            owner: self.owner.clone(),
            labels: self.label.clone(),
            limit: self.limit,
        }
    }
}

/// The `pri/owner/label/blocks/epic/milestone` metadata fields
/// shared by `*.add` params — flattened into them.
#[derive(Deserialize, Default)]
struct MetaParams {
    pri: Option<Priority>,
    owner: Option<Actor>,
    #[serde(default)]
    label: Vec<String>,
    #[serde(default)]
    blocks: Vec<ItemId>,
    epic: Option<String>,
    milestone: Option<String>,
}

impl MetaParams {
    fn into_meta(self) -> arca_core::NewItemMeta {
        arca_core::NewItemMeta {
            priority: self.pri,
            owner: self.owner,
            labels: self.label,
            blocks: self.blocks,
            epic: self.epic,
            milestone: self.milestone,
        }
    }
}

#[derive(Deserialize)]
struct TodoAddParams {
    title: String,
    section: Option<String>,
    parent: Option<ItemId>,
    #[serde(flatten)]
    meta: MetaParams,
}

#[derive(Deserialize)]
struct BacklogAddParams {
    title: String,
    section: Option<String>,
    #[serde(flatten)]
    meta: MetaParams,
}

/// `item.set` — `status` goes through the transition machine
/// (single step); `""` clears `epic`/`milestone`.
#[derive(Deserialize)]
struct SetParams {
    id: ItemId,
    status: Option<Status>,
    pri: Option<Priority>,
    owner: Option<Actor>,
    #[serde(default)]
    label: Vec<String>,
    #[serde(default)]
    unlabel: Vec<String>,
    #[serde(default)]
    blocks: Vec<String>,
    #[serde(default)]
    unblock: Vec<ItemId>,
    epic: Option<String>,
    milestone: Option<String>,
}

#[derive(Deserialize)]
struct HandoffParams {
    id: ItemId,
    note: String,
    actor: Option<Actor>,
    session: Option<String>,
}

#[derive(Deserialize)]
struct InterruptParams {
    id: ItemId,
    note: Option<String>,
    actor: Option<Actor>,
    session: Option<String>,
}

// ---------------------------------------------------------------- dispatch

/// Dispatch failure — [`Server::respond`] maps these to wire
/// codes; dispatch itself knows no wire.
#[derive(Debug)]
enum Fail {
    /// The method isn't served.
    UnknownMethod,
    /// Params didn't decode — -32602.
    BadParams(String),
    /// The verb failed — -32000.
    Verb(String),
    /// Status-machine rejection — -32001 + `data`.
    Transition {
        /// The item being set.
        id: ItemId,
        /// Current status.
        from: Status,
        /// Requested status.
        to: Status,
    },
}

impl From<String> for Fail {
    fn from(s: String) -> Fail {
        Fail::Verb(s)
    }
}

/// The v1 method surface — mirrored by `initialize`.
const METHODS: &[&str] = &[
    "initialize",
    "manifest",
    "schema",
    "status",
    "board",
    "item.show",
    "item.set",
    "todo.list",
    "todo.add",
    "todo.done",
    "todo.defer",
    "backlog.list",
    "backlog.add",
    "backlog.promote",
    "issue.list",
    "proposal.list",
    "plan.list",
    "adr.list",
    "report",
    "resume",
    "check",
    "handoff",
    "interrupt",
];

/// The served surface — discovery flags + `--no-commit`, shared
/// across requests. Nothing caches store state: every method
/// re-opens the store.
pub struct Server {
    dir: Option<PathBuf>,
    root: Option<PathBuf>,
    no_commit: bool,
}

impl Server {
    /// A server over the caller's discovery context.
    pub fn new(dir: Option<PathBuf>, root: Option<PathBuf>, no_commit: bool) -> Self {
        Server {
            dir,
            root,
            no_commit,
        }
    }

    fn open(&self) -> Result<(crate::discover::Project, arca_store_md::MdStore), Fail> {
        items::open(self.dir.as_deref(), self.root.as_deref()).map_err(Fail::Verb)
    }

    fn commit(&self, wt: &std::path::Path, msg: &str) -> Result<bool, Fail> {
        items::commit(wt, msg, self.no_commit).map_err(Fail::Verb)
    }

    /// The request loop — wire- and transport-agnostic: read one
    /// request, write one response, until EOF (`Ok(0)`).
    pub fn serve<T: ServeTransport, W: WireProtocol>(
        &self,
        transport: &mut T,
        wire: &W,
    ) -> Result<i32, String> {
        while let Some(line) = transport.read_request()? {
            let response = match wire.parse(&line) {
                Ok(req) => self.respond(req),
                Err(e) => Response::Error {
                    id: Value::Null,
                    code: e.code,
                    message: e.message,
                    data: None,
                },
            };
            transport.write_response(&wire.encode(&response))?;
        }
        Ok(0)
    }

    fn respond(&self, req: Request) -> Response {
        match self.dispatch(&req.method, &req.params) {
            Ok(result) => Response::Result { id: req.id, result },
            Err(fail) => {
                let (code, message, data) = match fail {
                    Fail::UnknownMethod => (
                        METHOD_NOT_FOUND,
                        format!("unknown method {:?}", req.method),
                        None,
                    ),
                    Fail::BadParams(m) => (INVALID_PARAMS, m, None),
                    Fail::Verb(m) => (VERB_ERROR, m, None),
                    Fail::Transition { id, from, to } => (
                        INVALID_TRANSITION,
                        format!("{id}: invalid status transition {from} → {to}"),
                        Some(json!({
                            "from": from.token(),
                            "to": to.token(),
                            "valid": from
                                .transitions()
                                .iter()
                                .map(|s| s.token())
                                .collect::<Vec<_>>(),
                        })),
                    ),
                };
                Response::Error {
                    id: req.id,
                    code,
                    message,
                    data,
                }
            }
        }
    }

    /// The verb surface — `method` + `params` → a JSON payload,
    /// transport- and wire-agnostic.
    fn dispatch(&self, method: &str, params: &Value) -> Result<Value, Fail> {
        match method {
            "initialize" => Ok(json!({
                "protocol": 1,
                "wire": "jsonrpc-2.0",
                "transport": "stdio",
                "server": {"name": "arca", "version": env!("CARGO_PKG_VERSION")},
                "methods": METHODS,
            })),
            "manifest" => Ok(crate::manifest()),
            "schema" => Ok(schema::document()),
            "status" => {
                let p: StatusParams = parse_params(params)?;
                let (_proj, store) = self.open()?;
                if p.prompt {
                    status::prompt_json(&store).map_err(Fail::Verb)
                } else {
                    status::status_json(&store).map_err(Fail::Verb)
                }
            }
            "board" => {
                let p: BoardParams = parse_params(params)?;
                let (_proj, store) = self.open()?;
                let scan = digest::scan(&store).map_err(Fail::Verb)?;
                let live: Vec<&arca_core::Item> = scan
                    .live()
                    .filter(|i| {
                        p.epic
                            .as_deref()
                            .is_none_or(|e| i.epic.as_deref() == Some(e))
                            && p.milestone
                                .as_deref()
                                .is_none_or(|m| i.milestone.as_deref() == Some(m))
                    })
                    .collect();
                Ok(board::columns_json(live.into_iter(), None))
            }
            "item.show" => {
                let p: IdParams = parse_params(params)?;
                let (_proj, store) = self.open()?;
                itemcmd::show_json(&store, p.id).map_err(Fail::Verb)
            }
            "todo.list" | "backlog.list" => {
                let p: ListParams = parse_params(params)?;
                let (_proj, store) = self.open()?;
                let found = todo::list_items(&store, method == "backlog.list", &p.filter())?;
                Ok(items_json(&found))
            }
            "issue.list" | "proposal.list" | "plan.list" | "adr.list" => {
                let p: ListParams = parse_params(params)?;
                let (_proj, store) = self.open()?;
                let kind = match method {
                    "issue.list" => ItemKind::Issue,
                    "proposal.list" => ItemKind::Proposal,
                    "plan.list" => ItemKind::Plan,
                    _ => ItemKind::Adr,
                };
                let found = items::query_files(&store, kind, p.archived, &p.filter())
                    .map_err(Fail::Verb)?;
                Ok(items_json(&found))
            }
            "report" => {
                let (proj, store) = self.open()?;
                report::report_json(&store, &proj.worktree).map_err(Fail::Verb)
            }
            "resume" => {
                let p: IdParams = parse_params(params)?;
                let (_proj, store) = self.open()?;
                continuity::resume_json(&store, p.id, p.last).map_err(Fail::Verb)
            }
            "check" => {
                let (proj, _store) = self.open()?;
                check::check_json(&proj).map_err(Fail::Verb)
            }
            "todo.add" => {
                let p: TodoAddParams = parse_params(params)?;
                let (proj, store) = self.open()?;
                let (item, file) = todo::add_item(
                    &store,
                    &p.title,
                    p.section.as_deref(),
                    p.parent,
                    &p.meta.into_meta(),
                )
                .map_err(Fail::Verb)?;
                let committed = self.commit(&proj.worktree, &format!("chore: add {}", item.id))?;
                Ok(json!({"item": item, "file": file, "parent": p.parent, "committed": committed}))
            }
            "todo.done" => {
                let p: IdParams = parse_params(params)?;
                let (proj, store) = self.open()?;
                let item = todo::done_item(&store, p.id).map_err(Fail::Verb)?;
                let committed = self.commit(&proj.worktree, &format!("chore: done {}", p.id))?;
                Ok(json!({"item": item, "committed": committed}))
            }
            "todo.defer" => {
                let p: IdParams = parse_params(params)?;
                let (proj, store) = self.open()?;
                let item = todo::defer_item(&store, p.id).map_err(Fail::Verb)?;
                let committed = self.commit(&proj.worktree, &format!("chore: defer {}", p.id))?;
                Ok(json!({"item": item, "file": "BACKLOG.md", "committed": committed}))
            }
            "backlog.add" => {
                let p: BacklogAddParams = parse_params(params)?;
                let (proj, store) = self.open()?;
                let item =
                    backlog::add_item(&store, &p.title, p.section.as_deref(), &p.meta.into_meta())
                        .map_err(Fail::Verb)?;
                let committed = self.commit(&proj.worktree, &format!("chore: add {}", item.id))?;
                Ok(json!({"item": item, "file": "BACKLOG.md", "committed": committed}))
            }
            "backlog.promote" => {
                let p: IdParams = parse_params(params)?;
                let (proj, store) = self.open()?;
                let item = backlog::promote_item(&store, p.id).map_err(Fail::Verb)?;
                let committed = self.commit(&proj.worktree, &format!("chore: promote {}", p.id))?;
                Ok(json!({"item": item, "file": "TODO.md", "committed": committed}))
            }
            "item.set" => {
                let p: SetParams = parse_params(params)?;
                let (proj, store) = self.open()?;
                let flags = itemcmd::SetFlags {
                    status: p.status,
                    pri: p.pri,
                    owner: p.owner,
                    label: p.label,
                    unlabel: p.unlabel,
                    blocks: p.blocks,
                    unblock: p.unblock,
                    epic: p.epic,
                    milestone: p.milestone,
                };
                if flags.is_noop() {
                    return Err(Fail::Verb(
                        "item set: nothing to change — pass at least one field".into(),
                    ));
                }
                // pre-check the status step so the wire gets the
                // typed -32001 + valid targets, not a string
                if let Some(to) = flags.status {
                    let Some((item, _, _)) = items::find_item(&store, p.id).map_err(Fail::Verb)?
                    else {
                        return Err(Fail::Verb(format!("{}: no such item", p.id)));
                    };
                    item.status
                        .transition_to(to)
                        .map_err(|_| Fail::Transition {
                            id: p.id,
                            from: item.status,
                            to,
                        })?;
                }
                let (item, location) =
                    itemcmd::mutate_item(&store, p.id, &flags).map_err(Fail::Verb)?;
                let committed = self.commit(&proj.worktree, &format!("chore: set {}", p.id))?;
                Ok(json!({"item": item, "location": location, "committed": committed}))
            }
            "handoff" => {
                let p: HandoffParams = parse_params(params)?;
                if p.note == "-" {
                    return Err(Fail::BadParams(
                        "note \"-\" reads stdin — stdin is the wire; pass the note inline".into(),
                    ));
                }
                let (proj, store) = self.open()?;
                let (entry, rel) = continuity::record_handoff(
                    &store,
                    &proj.worktree,
                    p.id,
                    p.note,
                    p.actor,
                    p.session,
                )
                .map_err(Fail::Verb)?;
                let committed = self.commit(&proj.worktree, &format!("chore: handoff {}", p.id))?;
                Ok(
                    json!({"id": p.id.to_string(), "path": rel, "entry": entry, "committed": committed}),
                )
            }
            "interrupt" => {
                let p: InterruptParams = parse_params(params)?;
                let (proj, store) = self.open()?;
                let entry =
                    continuity::interrupt_item(&store, p.id, p.note.as_deref(), p.actor, p.session)
                        .map_err(Fail::Verb)?;
                let committed =
                    self.commit(&proj.worktree, &format!("chore: interrupt {}", p.id))?;
                Ok(
                    json!({"id": p.id.to_string(), "status": "open", "entry": entry, "committed": committed}),
                )
            }
            _ => Err(Fail::UnknownMethod),
        }
    }
}

/// `Vec<Item>` → a JSON array of item objects.
fn items_json(items: &[arca_core::Item]) -> Value {
    json!(items
        .iter()
        .map(|i| serde_json::to_value(i).unwrap_or(json!(null)))
        .collect::<Vec<_>>())
}

/// The `serve` verb — stdio transport, JSON-RPC 2.0 wire, EOF
/// exits 0.
pub fn run(dir: Option<PathBuf>, root: Option<PathBuf>, no_commit: bool) -> Result<i32, String> {
    let server = Server::new(dir, root, no_commit);
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut transport = StdioTransport::new(stdin.lock(), stdout.lock());
    server.serve(&mut transport, &JsonRpc2)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A scaffolded project dir (no git needed — discovery
    /// accepts `--dir` verbatim and `no_commit` skips commits).
    fn fixture() -> (tempfile::TempDir, Server) {
        let d = tempfile::tempdir().unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(d.path().to_path_buf()).unwrap();
        let store = arca_store_md::MdStore::new(root);
        crate::ops::scaffold(&store, "p").unwrap();
        let s = Server::new(Some(d.path().to_path_buf()), None, true);
        (d, s)
    }

    fn call(s: &Server, method: &str, params: Value) -> Result<Value, Fail> {
        s.dispatch(method, &params)
    }

    #[test]
    fn jsonrpc_parse_valid_and_errors() {
        let w = JsonRpc2;
        let r = w
            .parse(r#"{"jsonrpc":"2.0","id":7,"method":"status","params":{"prompt":true}}"#)
            .unwrap();
        assert_eq!(r.id, json!(7));
        assert_eq!(r.method, "status");
        assert_eq!(r.params["prompt"], true);
        // params optional
        let r = w.parse(r#"{"id":"a","method":"manifest"}"#).unwrap();
        assert_eq!(r.id, json!("a"));
        assert!(r.params.is_null());
        // not json → -32700
        assert_eq!(w.parse("hello").unwrap_err().code, PARSE_ERROR);
        // non-object → -32600
        assert_eq!(w.parse("[1,2]").unwrap_err().code, INVALID_REQUEST);
        // missing id → -32600 (notifications aren't served)
        assert_eq!(
            w.parse(r#"{"method":"status"}"#).unwrap_err().code,
            INVALID_REQUEST
        );
        // missing method → -32600
        assert_eq!(w.parse(r#"{"id":1}"#).unwrap_err().code, INVALID_REQUEST);
    }

    #[test]
    fn jsonrpc_encode_roundtrip() {
        let w = JsonRpc2;
        let line = w.encode(&Response::Result {
            id: json!("x"),
            result: json!({"ok": true}),
        });
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], "x");
        assert_eq!(v["result"]["ok"], true);
        assert!(!line.contains('\n'));
        // error with data
        let line = w.encode(&Response::Error {
            id: Value::Null,
            code: INVALID_TRANSITION,
            message: "nope".into(),
            data: Some(json!({"valid": ["open"]})),
        });
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["id"], Value::Null);
        assert_eq!(v["error"]["code"], INVALID_TRANSITION);
        assert_eq!(v["error"]["data"]["valid"], json!(["open"]));
    }

    #[test]
    fn serve_loop_over_memory_transport() {
        let (_d, s) = fixture();
        let input = b"{\"id\":1,\"method\":\"manifest\"}\ngarbage\n{\"id\":2,\"method\":\"todo.add\",\"params\":{\"title\":\"x\"}}\n";
        let mut out = Vec::new();
        {
            let mut t = StdioTransport::new(&input[..], &mut out);
            assert_eq!(s.serve(&mut t, &JsonRpc2).unwrap(), 0);
        }
        let lines: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["id"], 1);
        assert_eq!(lines[0]["result"]["name"], "arca");
        // a parse error doesn't kill the loop
        assert_eq!(lines[1]["error"]["code"], PARSE_ERROR);
        assert_eq!(lines[1]["id"], Value::Null);
        assert_eq!(lines[2]["result"]["item"]["id"], "T-001");
        // EOF → clean exit (serve returned already)
    }

    #[test]
    fn dispatch_reads_and_writes() {
        let (_d, s) = fixture();
        // reads on an empty ledger
        let v = call(&s, "status", Value::Null).unwrap();
        assert_eq!(v["repo"], "p");
        assert_eq!(v["counts"]["tasks"]["open"], 0);
        let v = call(&s, "check", Value::Null).unwrap();
        assert!(v["errors"].is_array());
        // write + read-back through fresh store opens
        let v = call(&s, "todo.add", json!({"title": "first", "pri": "high"})).unwrap();
        assert_eq!(v["item"]["id"], "T-001");
        assert_eq!(v["committed"], false); // no_commit
        let v = call(&s, "todo.list", Value::Null).unwrap();
        assert_eq!(v[0]["id"], "T-001");
        assert_eq!(v[0]["priority"], "high");
        let v = call(&s, "status", json!({"prompt": true})).unwrap();
        assert!(v["prompt"].as_str().unwrap().contains("T-001"));
        // item.set — legal step, then an invalid one → -32001 shape
        let v = call(
            &s,
            "item.set",
            json!({"id": "T-001", "status": "in-progress"}),
        )
        .unwrap();
        assert_eq!(v["item"]["status"], "in-progress");
        match call(&s, "item.set", json!({"id": "T-001", "status": "claimed"})) {
            Err(Fail::Transition { id, from, to }) => {
                assert_eq!(id.to_string(), "T-001");
                assert_eq!(from, Status::InProgress);
                assert_eq!(to, Status::Claimed);
            }
            _ => panic!("expected transition failure"),
        }
        // interrupt a live item → open + marker
        let v = call(&s, "interrupt", json!({"id": "T-001"})).unwrap();
        assert_eq!(v["status"], "open");
        // todo.done removes the line
        call(
            &s,
            "item.set",
            json!({"id": "T-001", "status": "in-progress"}),
        )
        .unwrap();
        let v = call(&s, "todo.done", json!({"id": "T-001"})).unwrap();
        assert_eq!(v["item"]["status"], "done");
        assert_eq!(
            call(&s, "todo.list", Value::Null)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            0
        );
        // unknown + bad params
        assert!(matches!(
            call(&s, "nope", Value::Null),
            Err(Fail::UnknownMethod)
        ));
        assert!(matches!(
            call(&s, "todo.add", json!({})),
            Err(Fail::BadParams(_))
        ));
    }

    #[test]
    fn stdio_transport_lines_and_eof() {
        let input = b"one\n\r\nthree";
        let mut out = Vec::new();
        let mut t = StdioTransport::new(&input[..], &mut out);
        assert_eq!(t.read_request().unwrap().as_deref(), Some("one"));
        assert_eq!(t.read_request().unwrap().as_deref(), Some(""));
        assert_eq!(t.read_request().unwrap().as_deref(), Some("three"));
        assert_eq!(t.read_request().unwrap(), None);
        t.write_response("resp").unwrap();
        assert_eq!(out, b"resp\n");
    }
}

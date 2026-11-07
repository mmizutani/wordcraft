//! JSON-RPC 2.0 framing and the MCP lifecycle / tools / resources methods.

use std::io::{BufRead, Read, Write};

use serde_json::{Value, json};

use crate::backend::Backend;
use crate::tools::{call_tool, tool_definitions};

/// The MCP revision a client gets from `initialize` (the handshake era, up to 2025-06-18).
pub const PROTOCOL_VERSION: &str = "2025-06-18";
const SUPPORTED_VERSIONS: &[&str] = &[PROTOCOL_VERSION, "2025-03-26", "2024-11-05"];
/// The stateless revision: no `initialize`, every request names its version in `_meta`
/// (`io.modelcontextprotocol/protocolVersion`), and `server/discover` says what the server is.
pub const STATELESS_VERSION: &str = "2026-07-28";
/// Every revision served, newest first (`server/discover`'s `supportedVersions`).
pub const ALL_VERSIONS: &[&str] = &[STATELESS_VERSION, PROTOCOL_VERSION, "2025-03-26", "2024-11-05"];
/// The `_meta` key a 2026-07-28 request names its revision with.
pub const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
const RESOURCE_NOT_FOUND: i64 = -32002;
/// A request naming a revision the server doesn't speak (2026-07-28).
pub const UNSUPPORTED_VERSION: i64 = -32022;

const INSTRUCTIONS: &str = "WordCraft is a word processor (a Microsoft Word clone). Every action is a command: find ids and \
parameters with list_commands and run them with execute (or several with batch). Positions are \
{story:\"body\", path:[block,(row,cell,block)...], off:byteOffset}. Typical flow: new_document → type_text \
\"Hello\" → execute text.newParagraph → execute para.style {style:\"Heading 1\"} → select_text {text:\"Hello\"} → \
execute format.bold → inspect_document to verify → render_page to look at the result → save {path:\"out.docx\"}. \
Several documents can be open: new_document and open_document open another one and make it active; \
list_documents gives their ids, and `doc` on execute, batch and the other document tools works on one \
without switching.";

/// Resource URIs.
pub const DOC_URI: &str = "wordcraft://document";
pub const COMMANDS_URI: &str = "wordcraft://commands";

/// Longest request line accepted on stdin.
pub const MAX_REQUEST_LINE: usize = 1 << 20;

/// One bounded line read ([`read_bounded_line`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BoundedLine {
    Eof,
    Line(String),
    /// The line exceeded the limit; the rest of it has been discarded.
    TooLong,
}

/// Read one line of at most `max` bytes (excluding the newline) without buffering more than
/// `max + 1` bytes; an over-long line is skipped up to its newline. A line that isn't UTF-8 is
/// an [`std::io::ErrorKind::InvalidData`] error, after which the next line can be read. The
/// built-in assistant reads its ACP agents' lines with it too.
pub fn read_bounded_line(reader: &mut impl BufRead, max: usize) -> std::io::Result<BoundedLine> {
    let mut buf = Vec::new();
    reader.by_ref().take(max as u64 + 1).read_until(b'\n', &mut buf)?;
    if buf.is_empty() {
        return Ok(BoundedLine::Eof);
    }
    if buf.last() != Some(&b'\n') && buf.len() > max {
        loop {
            let chunk = reader.fill_buf()?;
            if chunk.is_empty() {
                break;
            }
            let found = chunk.iter().position(|&b| b == b'\n');
            let used = found.map_or(chunk.len(), |i| i + 1);
            reader.consume(used);
            if found.is_some() {
                break;
            }
        }
        return Ok(BoundedLine::TooLong);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
    }
    String::from_utf8(buf).map(BoundedLine::Line).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// An MCP server bound to one backend.
pub struct Server {
    backend: Box<dyn Backend>,
    initialized: bool,
    /// The longest reply line this server builds ([`Server::limit_replies`]).
    max_reply: usize,
}

std::thread_local! {
    /// The longest text a reply being built on this thread may carry: what [`Server::handle_line`]
    /// is limited to while it runs, so a tool's answer stops growing at the limit instead of
    /// being built whole and thrown away.
    static REPLY_CAP: std::cell::Cell<usize> = const { std::cell::Cell::new(usize::MAX) };
}

/// The longest text a reply being built on this thread may carry.
pub(crate) fn reply_cap() -> usize {
    REPLY_CAP.with(std::cell::Cell::get)
}

/// Sets [`REPLY_CAP`] while it lives, and puts the one before back.
struct CapFor(usize);

impl CapFor {
    fn set(max: usize) -> Self {
        CapFor(REPLY_CAP.with(|c| c.replace(max)))
    }
}

impl Drop for CapFor {
    fn drop(&mut self) {
        REPLY_CAP.with(|c| c.set(self.0));
    }
}

/// A buffer that refuses to grow past `max` bytes.
struct Capped {
    buf: Vec<u8>,
    max: usize,
}

impl Write for Capped {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if self.buf.len().saturating_add(b.len()) > self.max {
            return Err(std::io::Error::other("larger than the limit"));
        }
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `v` as JSON text (indented when `pretty`), or `None` once it would pass `max` bytes: writing
/// stops there, so nothing larger is ever built.
pub(crate) fn capped_json(v: &Value, pretty: bool, max: usize) -> Option<String> {
    let mut out = Capped { buf: Vec::new(), max };
    let written = if pretty { serde_json::to_writer_pretty(&mut out, v) } else { serde_json::to_writer(&mut out, v) };
    written.ok()?;
    String::from_utf8(out.buf).ok()
}

/// What a tool or resource says when its answer would pass [`reply_cap`].
pub(crate) fn too_large_note(max: usize) -> String {
    let size = if max >= 1 << 20 { format!("{} MiB", max >> 20) } else { format!("{} KiB", (max >> 10).max(1)) };
    format!("the answer is larger than this connection sends at once ({size}): ask for less")
}

fn response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

/// The revision a 2026-07-28-style request names in its `_meta`, if it names one: such a request
/// is answered statelessly, whatever came before it.
pub fn stateless_version(params: &Value) -> Option<&str> {
    params.get("_meta").and_then(|m| m.get(META_VERSION)).and_then(Value::as_str)
}

fn server_info() -> Value {
    json!({"name": "wordcraft", "title": "WordCraft", "version": env!("CARGO_PKG_VERSION"), "websiteUrl": "https://getartcraft.com/apps/wordcraft"})
}

/// How long a 2026-07-28 client may keep a list: the tools and resources a build has don't
/// change while it runs, unlike the document, which is never cached.
const STATIC_TTL_MS: u64 = 60 * 60 * 1000;

impl Server {
    pub fn new(backend: Box<dyn Backend>) -> Self {
        Self { backend, initialized: false, max_reply: usize::MAX }
    }

    /// Build no reply line longer than `max` bytes, and read none longer from the backend: an
    /// answer that would pass it becomes an error saying so, and stops growing at the limit
    /// rather than being built whole first (`http::Endpoint`, whose answers are budgeted).
    pub fn limit_replies(&mut self, max: usize) {
        self.max_reply = max;
        self.backend.limit_replies(max);
    }

    pub fn backend(&mut self) -> &mut dyn Backend {
        self.backend.as_mut()
    }

    /// Whether the client has sent `notifications/initialized`.
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Serve newline-delimited JSON-RPC until `input` closes. Logs go to stderr only (stdout is
    /// the protocol stream).
    pub fn serve(&mut self, mut input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
        loop {
            let reply = match read_bounded_line(&mut input, MAX_REQUEST_LINE)? {
                BoundedLine::Eof => break,
                BoundedLine::Line(line) => self.handle_line(&line),
                BoundedLine::TooLong => Some(error(Value::Null, INVALID_REQUEST, "request line too long").to_string()),
            };
            if let Some(reply) = reply {
                output.write_all(reply.as_bytes())?;
                output.write_all(b"\n")?;
                output.flush()?;
            }
        }
        Ok(())
    }

    /// Handle one line; returns the reply line (None for notifications and blank lines).
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let max = self.max_reply;
        let _cap = CapFor::set(max);
        let reply = match serde_json::from_str::<Value>(line) {
            Ok(Value::Array(batch)) => {
                // Batches were removed in 2025-06-18; still answer older clients sensibly.
                if batch.is_empty() {
                    Some(error(Value::Null, INVALID_REQUEST, "empty batch"))
                } else {
                    let replies: Vec<Value> = batch.into_iter().filter_map(|m| self.handle(m)).collect();
                    (!replies.is_empty()).then_some(Value::Array(replies))
                }
            }
            Ok(msg) => self.handle(msg),
            Err(e) => Some(error(Value::Null, PARSE_ERROR, format!("parse error: {e}"))),
        };
        reply.map(|r| match capped_json(&r, false, max) {
            Some(text) => text,
            None => error(r.get("id").cloned().unwrap_or(Value::Null), INTERNAL_ERROR, too_large_note(max)).to_string(),
        })
    }

    /// Handle one JSON-RPC message; `None` for notifications and responses.
    pub fn handle(&mut self, msg: Value) -> Option<Value> {
        let Value::Object(o) = &msg else { return Some(error(Value::Null, INVALID_REQUEST, "message must be an object")) };
        let id = o.get("id").cloned();
        let Some(method) = o.get("method").and_then(Value::as_str) else {
            // A response to a server→client request (we send none): ignore. Anything else is invalid.
            if o.contains_key("result") || o.contains_key("error") {
                return None;
            }
            return Some(error(id.unwrap_or(Value::Null), INVALID_REQUEST, "missing `method`"));
        };
        let params = o.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = id else {
            self.notification(method);
            return None;
        };
        if !(id.is_string() || id.is_number()) {
            return Some(error(Value::Null, INVALID_REQUEST, "`id` must be a string or number"));
        }
        if let Some(version) = stateless_version(&params) {
            if version != STATELESS_VERSION {
                let mut e = error(id, UNSUPPORTED_VERSION, format!("protocol version {version} is not supported"));
                e["error"]["data"] = json!({"supported": ALL_VERSIONS, "requested": version});
                return Some(e);
            }
            return Some(match self.stateless(method, &params) {
                Ok(r) => response(id, r),
                Err((code, m)) => error(id, code, m),
            });
        }
        Some(match self.request(method, &params) {
            Ok(r) => response(id, r),
            Err((code, m)) => error(id, code, m),
        })
    }

    fn notification(&mut self, method: &str) {
        match method {
            "notifications/initialized" => self.initialized = true,
            "notifications/cancelled" | "notifications/progress" | "notifications/roots/list_changed" => {}
            other => log::debug!("ignoring notification {other}"),
        }
    }

    fn instructions(&self) -> String {
        format!(
            "{INSTRUCTIONS} Backend: {}. Community: https://discord.gg/artcraft · https://getartcraft.com/apps/wordcraft · https://github.com/storytold/wordcraft",
            self.backend.describe()
        )
    }

    fn resources() -> Value {
        json!([
            {"uri": DOC_URI, "name": "document", "title": "Active document", "description": "Blocks, text, formatting, sections, parts and selection of the document (document.inspect)", "mimeType": "application/json"},
            {"uri": COMMANDS_URI, "name": "commands", "title": "Commands", "description": "Every command: id, label, ribbon location, shortcut, params, enabled (engine.commands)", "mimeType": "application/json"},
        ])
    }

    /// The contents of resource `uri`, or `None` when there is none.
    fn read_resource(&mut self, uri: &str) -> Option<Result<Value, String>> {
        let v = match uri {
            DOC_URI => self.backend.call("document.inspect", json!({})),
            COMMANDS_URI => self.backend.call("engine.commands", json!({})),
            _ => return None,
        };
        let max = reply_cap();
        Some(v.and_then(|v| {
            let text = capped_json(&v, true, max).ok_or_else(|| too_large_note(max))?;
            Ok(json!([{"uri": uri, "mimeType": "application/json", "text": text}]))
        }))
    }

    /// A 2026-07-28 request: no handshake before it, and nothing kept from one request to the
    /// next. Each result says it is complete (`resultType`) and names the server in its `_meta`;
    /// lists and reads say how long they may be kept (`ttlMs`, `cacheScope`).
    fn stateless(&mut self, method: &str, params: &Value) -> Result<Value, (i64, String)> {
        let cache = |ttl: u64, scope: &str| json!({"ttlMs": ttl, "cacheScope": scope});
        let mut result = match method {
            "server/discover" => {
                let mut r = json!({
                    "supportedVersions": ALL_VERSIONS,
                    "capabilities": {"tools": {}, "resources": {}},
                    "instructions": self.instructions(),
                });
                merge(&mut r, cache(STATIC_TTL_MS, "private"));
                r
            }
            "tools/list" => {
                let mut r = json!({"tools": tool_definitions()});
                merge(&mut r, cache(STATIC_TTL_MS, "public"));
                r
            }
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).ok_or((INVALID_PARAMS, "missing tool `name`".to_string()))?;
                let args = params.get("arguments").cloned().unwrap_or(Value::Null);
                call_tool(self.backend.as_mut(), name, &args).to_value()
            }
            "resources/list" => {
                let mut r = json!({"resources": Self::resources()});
                merge(&mut r, cache(STATIC_TTL_MS, "public"));
                r
            }
            "resources/templates/list" => {
                let mut r = json!({"resourceTemplates": []});
                merge(&mut r, cache(STATIC_TTL_MS, "public"));
                r
            }
            "resources/read" => {
                let uri = params.get("uri").and_then(Value::as_str).ok_or((INVALID_PARAMS, "missing `uri`".to_string()))?;
                // The document changes from one moment to the next: never kept.
                let contents =
                    self.read_resource(uri).ok_or((INVALID_PARAMS, format!("resource not found: {uri}")))?.map_err(|e| (INTERNAL_ERROR, e))?;
                let mut r = json!({"contents": contents});
                merge(&mut r, cache(0, "private"));
                r
            }
            other => return Err((METHOD_NOT_FOUND, format!("method not found: {other}"))),
        };
        merge(&mut result, json!({"resultType": "complete"}));
        if let Some(o) = result.as_object_mut() {
            let meta = o.entry("_meta").or_insert_with(|| json!({}));
            if let Some(m) = meta.as_object_mut() {
                m.insert(META_SERVER_INFO.into(), server_info());
            }
        }
        Ok(result)
    }

    fn request(&mut self, method: &str, params: &Value) -> Result<Value, (i64, String)> {
        match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION);
                let version = if SUPPORTED_VERSIONS.contains(&asked) { asked } else { PROTOCOL_VERSION };
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}, "resources": {}},
                    "serverInfo": server_info(),
                    "instructions": self.instructions(),
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tool_definitions()})),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).ok_or((INVALID_PARAMS, "missing tool `name`".to_string()))?;
                let args = params.get("arguments").cloned().unwrap_or(Value::Null);
                Ok(call_tool(self.backend.as_mut(), name, &args).to_value())
            }
            "resources/list" => Ok(json!({"resources": Self::resources()})),
            "resources/templates/list" => Ok(json!({"resourceTemplates": []})),
            "resources/read" => {
                let uri = params.get("uri").and_then(Value::as_str).ok_or((INVALID_PARAMS, "missing `uri`".to_string()))?;
                let contents =
                    self.read_resource(uri).ok_or((RESOURCE_NOT_FOUND, format!("resource not found: {uri}")))?.map_err(|e| (INTERNAL_ERROR, e))?;
                Ok(json!({"contents": contents}))
            }
            other => Err((METHOD_NOT_FOUND, format!("method not found: {other}"))),
        }
    }
}

/// Add `extra`'s fields to the object `v`.
fn merge(v: &mut Value, extra: Value) {
    if let (Some(o), Value::Object(e)) = (v.as_object_mut(), extra) {
        o.extend(e);
    }
}

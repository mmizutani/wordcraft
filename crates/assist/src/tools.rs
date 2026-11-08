//! The tools an assistant's model calls: the MCP server's own ([`wordcraft_mcp::tools`]), for
//! the one shared document the assistant holds a copy of. They run in that copy through the
//! control channel's methods for a participant's copy ([`hosted::call`]), so the assistant is
//! held to its role like any participant with a key, and to the files File › Options › Agents lets
//! agents use.

use std::sync::{Arc, OnceLock};

use serde_json::{Value, json};
use wordcraft_doc::change::Anchor;
use wordcraft_engine::room::{Replica, Room, RoomError, hosted};
use wordcraft_mcp::tools::{ToolResult, call_tool, tool_definitions};

use crate::backend::{Cancel, Source, SourceKind, ToolHost, ToolOutput, ToolSpec, Versions, cut};

/// The MCP tools an assistant's model gets: those that read and edit one document. Not the
/// ones that open, save or switch documents, drive the window or share the document.
pub const TOOLS: &[&str] = &["inspect_document", "get_text", "get_changes", "list_commands", "select_text", "type_text", "execute", "batch"];

/// Most bytes of a tool's output the model gets back; the rest is cut, and the output says so.
pub const MAX_OUTPUT: usize = 64 << 10;

/// Most commands one `batch` call runs.
pub const MAX_BATCH: usize = 256;

/// The assistant's tools, as function tools: [`TOOLS`] from the MCP server's list, without the
/// `doc` argument (an assistant's copy holds the shared document only).
pub fn specs() -> &'static [ToolSpec] {
    static SPECS: OnceLock<Vec<ToolSpec>> = OnceLock::new();
    SPECS.get_or_init(|| {
        let all = tool_definitions();
        let all = all.as_array().map(Vec::as_slice).unwrap_or_default();
        TOOLS
            .iter()
            .filter_map(|name| all.iter().find(|t| t.get("name").and_then(Value::as_str) == Some(*name)))
            .map(|t| {
                let mut parameters = t.get("inputSchema").cloned().unwrap_or_else(|| json!({"type": "object", "properties": {}}));
                if let Some(p) = parameters.get_mut("properties").and_then(Value::as_object_mut) {
                    p.remove("doc");
                }
                let name = t.get("name").and_then(Value::as_str).unwrap_or_default();
                let description = own_description(name).or_else(|| t.get("description").and_then(Value::as_str)).unwrap_or_default();
                ToolSpec { name: name.to_string(), description: description.to_string(), parameters }
            })
            .collect()
    })
}

/// What the assistant's own tools say where the MCP server's speak of other open documents
/// (`doc`; opening, switching and closing documents), which an assistant's copy can't reach:
/// a model told it could would spend its turn on calls that can only fail.
fn own_description(name: &str) -> Option<&'static str> {
    match name {
        "execute" => Some(
            "Run one command by id with params, e.g. {command:\"format.bold\"} or {command:\"insert.table\", params:{rows:3, cols:4}}. \
             Commands work on the shared document at your copy's selection. Ones that open, save, switch or close documents, or \
             drive the window, are refused here.",
        ),
        "batch" => Some(
            "Run commands in order, e.g. {commands:[{command:\"caret.docEnd\"}, {command:\"text.insert\", params:{text:\"!\"}}]}; \
             stops at the first error unless keepGoing.",
        ),
        _ => None,
    }
}

/// The assistant's copy of the shared document, which its model's tool calls work on.
pub struct CopyTools {
    copy: Replica,
    /// The turn's stop flag, which a `batch` checks between its commands.
    cancel: Option<Cancel>,
}

impl CopyTools {
    /// Join `room` with the invite `key` handed out for the assistant: a copy of its own.
    pub fn join(room: &Arc<Room>, key: &str) -> Result<CopyTools, RoomError> {
        Ok(CopyTools { copy: room.join_with(key)?, cancel: None })
    }

    /// The same tools, with a `batch` that stops between its commands once `cancel` is set.
    pub fn stopped_by(mut self, cancel: Cancel) -> CopyTools {
        self.cancel = Some(cancel);
        self
    }

    /// The copy.
    pub fn copy(&self) -> &Replica {
        &self.copy
    }

    /// The copy, to bring up to date or look at.
    pub fn copy_mut(&mut self) -> &mut Replica {
        &mut self.copy
    }
}

/// The copy as the MCP tools reach a backend: the control channel's methods for a participant's
/// copy.
struct Hosted<'a>(&'a mut Replica);

impl wordcraft_mcp::Backend for Hosted<'_> {
    fn call(&mut self, method: &str, mut params: Value) -> Result<Value, String> {
        let command = params.get("command").and_then(Value::as_str).unwrap_or(method);
        if matches!(command, "document.text" | "document.inspect" | "document.paragraph" | "document.selection" | "document.changes") {
            let args = if method == "engine.execute" {
                let request = params.as_object_mut().ok_or("command arguments must be an object")?;
                request.entry("params").or_insert_with(|| json!({}))
            } else {
                &mut params
            };
            if !args.is_object() {
                *args = json!({});
            }
            if let Some(args) = args.as_object_mut() {
                args.insert("maxInputBytes".into(), json!(MAX_OUTPUT));
            }
        }
        hosted::call(self.0, method, &params)
    }
    fn has_ui(&self) -> bool {
        false
    }
    fn describe(&self) -> String {
        "the assistant's copy of the shared document".into()
    }
}

impl ToolHost for CopyTools {
    fn tools(&self) -> &[ToolSpec] {
        specs()
    }

    fn call(&mut self, name: &str, args: &Value) -> ToolOutput {
        if !TOOLS.contains(&name) {
            return ToolOutput::error(format!("unknown tool `{}`: {}", crate::backend::label(name), TOOLS.join(", ")));
        }
        let args = match args {
            Value::Object(_) => args.clone(),
            Value::Null => json!({}),
            _ => return ToolOutput::error("the arguments must be an object"),
        };
        // What the copy had before: the call's changes are the ones after it in its name (a
        // batch makes one per command; the person's may come between).
        let (sent, had) = (self.copy.stats().sent, self.copy.received());
        let out = if name == "batch" {
            self.batch(&args)
        } else {
            let result = call_tool(&mut Hosted(&mut self.copy), name, &args);
            output(&result)
        };
        let changed = self.copy.stats().sent > sent;
        let changes = changed.then(|| Versions { first: had.saturating_add(1), last: self.copy.received() });
        ToolOutput { changes, ..out }
    }

    fn select(&mut self, anchor: Anchor, focus: Anchor) -> bool {
        // What the hub took since, the person's edits among it, first: within the files the
        // assistant can use now, as its tool calls are, since an edit of its own made again
        // runs its command again (#31).
        let reach = self.copy.room().reach(self.copy.id());
        reach.confine(|| {
            self.copy.receive();
            self.copy.run("select.range", &json!({"anchor": anchor, "focus": focus})).is_ok()
        })
    }
}

impl CopyTools {
    /// The MCP server's `batch`, one command at a time: it stops at the first error (unless
    /// `keepGoing`) and once the turn is stopped, and holds what it gives back to about
    /// [`MAX_OUTPUT`] as it goes, so a batch of big results never piles up in memory. Every
    /// command gets an entry; the results get what the entries leave of the budget, in order.
    fn batch(&mut self, args: &Value) -> ToolOutput {
        /// The most an entry without its result takes, with its command id cut to 48 characters.
        const ENTRY: usize = 80;
        let Some(commands) = args.get("commands").and_then(Value::as_array) else {
            return ToolOutput::error("`commands`: a list of {\"command\": id, \"params\": {…}}");
        };
        if commands.len() > MAX_BATCH {
            return ToolOutput::error(format!("`commands`: at most {MAX_BATCH} in one batch"));
        }
        let keep_going = args.get("keepGoing").and_then(Value::as_bool).unwrap_or(false);
        let mut room = MAX_OUTPUT.saturating_sub(ENTRY.saturating_mul(commands.len().saturating_add(1)));
        let mut out = Vec::new();
        let mut ok = true;
        for c in commands {
            if self.cancel.as_ref().is_some_and(Cancel::is_set) {
                out.push(json!({"stopped": "the person stopped this turn: the commands from here on didn't run"}));
                ok = false;
                break;
            }
            let id = c.get("command").and_then(Value::as_str).unwrap_or_default();
            let params = c.get("params").cloned().unwrap_or_else(|| json!({}));
            let result = call_tool(&mut Hosted(&mut self.copy), "execute", &json!({"command": id, "params": params}));
            let detail = fitted(output(&result).text, room);
            room = room.saturating_sub(detail.as_ref().map_or(0, |d| d.to_string().len()));
            let shown: String = crate::backend::label(id).chars().take(48).collect();
            let mut entry = json!({"command": shown, "ok": !result.is_error});
            if let (Some(d), Some(e)) = (detail, entry.as_object_mut()) {
                e.insert(if result.is_error { "error" } else { "result" }.into(), d);
            }
            out.push(entry);
            // As the MCP server's `batch`: an error stops it and fails the call, unless it keeps
            // going, when each command's entry says how it went.
            if result.is_error && !keep_going {
                ok = false;
                break;
            }
        }
        ToolOutput { ok, text: Value::Array(out).to_string(), changes: None }
    }
}

/// A command's result for a batch entry, in at most `room` bytes as JSON: as it is when it
/// fits, else cut as text, else none.
fn fitted(text: String, room: usize) -> Option<Value> {
    if room < 256 {
        return None;
    }
    let value = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
    if value.to_string().len() <= room {
        return Some(value);
    }
    let text = match value {
        Value::String(s) => s,
        other => other.to_string(),
    };
    // Escaping can lengthen the text: halve the cut until it fits.
    let mut keep = room.saturating_sub(16);
    while keep >= 64 {
        let cut_text = Value::String(cut(text.clone(), keep) + "…[cut]");
        if cut_text.to_string().len() <= room {
            return Some(cut_text);
        }
        keep /= 2;
    }
    None
}

/// A tool result as the model gets it: its text, at most [`MAX_OUTPUT`] bytes.
fn output(result: &ToolResult) -> ToolOutput {
    let mut text = String::new();
    let mut total = 0usize;
    for c in &result.content {
        let omitted;
        let chunk = match c.get("type").and_then(Value::as_str) {
            Some("text") => c.get("text").and_then(Value::as_str).unwrap_or_default(),
            Some(other) => {
                omitted = format!("[{} left out]", crate::backend::label(other));
                &omitted
            }
            None => continue,
        };
        total = total.saturating_add(chunk.len());
        let mut end = MAX_OUTPUT.saturating_sub(text.len()).min(chunk.len());
        while !chunk.is_char_boundary(end) {
            end -= 1;
        }
        text.push_str(chunk.get(..end).unwrap_or_default());
    }
    if total > text.len() {
        let left = total.saturating_sub(text.len());
        text.push_str(&format!("\n…[cut: {left} more bytes; ask for less, e.g. get_text, document.paragraph or a narrower list_commands query]"));
    }
    ToolOutput { ok: !result.is_error, text, changes: None }
}

/// What a call does, for its row in the pane: the tool, and the command it runs.
pub fn title(name: &str, args: &Value) -> String {
    let command = args.get("command").and_then(Value::as_str);
    let n = args.get("commands").and_then(Value::as_array).map(Vec::len);
    let name = crate::backend::label(name);
    match (command, n) {
        (Some(c), _) => format!("{name} {}", crate::backend::label(c)),
        (None, Some(n)) => format!("{name} ({n} commands)"),
        _ => name,
    }
}

/// The kind of call a tool makes (ACP's kinds).
pub fn kind(name: &str) -> &'static str {
    match name {
        "inspect_document" | "get_text" | "get_changes" => "read",
        "list_commands" => "search",
        "select_text" | "type_text" | "execute" | "batch" => "edit",
        _ => "other",
    }
}

/// What a call of WordCraft's own tools reads, for the pane's Sources: the shared document, and
/// the paragraph when it names one. A call that changes the document reads nothing here.
pub fn sources(name: &str, args: &Value) -> Vec<Source> {
    let command = args.get("command").and_then(Value::as_str).unwrap_or_default();
    if !matches!(kind(name), "read" | "search") && !command.starts_with("document.") {
        return Vec::new();
    }
    let path = args.pointer("/params/path").and_then(Value::as_array).filter(|p| !p.is_empty() && p.iter().all(Value::is_u64));
    vec![Source { kind: SourceKind::Document, title: title(name, args), target: path.map(|p| Value::from(p.clone()).to_string()) }]
}

/// A call's result in a line or two, for its row in the pane.
pub fn summary(out: &ToolOutput) -> String {
    let first: String = out.text.lines().take(2).collect::<Vec<_>>().join(" ");
    let mut s = crate::backend::label(&first);
    if s.chars().count() >= 160 {
        s = s.chars().take(160).collect::<String>() + "…";
    }
    s
}

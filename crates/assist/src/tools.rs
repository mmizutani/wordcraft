//! The tools an assistant's model calls: the MCP server's own ([`wordcraft_mcp::tools`]), for
//! the one shared document the assistant holds a copy of. They run in that copy through the
//! control channel's methods for a participant's copy ([`hosted::call`]), so the assistant is
//! held to its role like any participant with a key, and reaches no file.

use std::sync::{Arc, OnceLock};

use serde_json::{Value, json};
use wordcraft_engine::room::{Replica, Room, RoomError, hosted};
use wordcraft_mcp::tools::{ToolResult, call_tool, tool_definitions};

use crate::backend::{ToolHost, ToolOutput, ToolSpec, cut};

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
                ToolSpec {
                    name: t.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
                    description: t.get("description").and_then(Value::as_str).unwrap_or_default().to_string(),
                    parameters,
                }
            })
            .collect()
    })
}

/// The assistant's copy of the shared document, which its model's tool calls work on.
pub struct CopyTools {
    copy: Replica,
}

impl CopyTools {
    /// Join `room` with the invite `key` handed out for the assistant: a copy of its own.
    pub fn join(room: &Arc<Room>, key: &str) -> Result<CopyTools, RoomError> {
        Ok(CopyTools { copy: room.join_with(key)? })
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
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
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
        if name == "batch" && args.get("commands").and_then(Value::as_array).is_some_and(|c| c.len() > MAX_BATCH) {
            return ToolOutput::error(format!("`commands`: at most {MAX_BATCH} in one batch"));
        }
        let sent = self.copy.stats().sent;
        let result = call_tool(&mut Hosted(&mut self.copy), name, &args);
        let changed = self.copy.stats().sent > sent;
        output(&result, changed.then(|| self.copy.received()))
    }
}

/// A tool result as the model gets it: its text, at most [`MAX_OUTPUT`] bytes.
fn output(result: &ToolResult, version: Option<u64>) -> ToolOutput {
    let mut text = String::new();
    for c in &result.content {
        match c.get("type").and_then(Value::as_str) {
            Some("text") => text.push_str(c.get("text").and_then(Value::as_str).unwrap_or_default()),
            Some(other) => text.push_str(&format!("[{other} left out]")),
            None => {}
        }
    }
    if text.len() > MAX_OUTPUT {
        let total = text.len();
        text = cut(text, MAX_OUTPUT);
        text.push_str(&format!(
            "\n…[cut: {} more bytes; ask for less, e.g. get_text, document.paragraph or a narrower list_commands query]",
            total.saturating_sub(text.len())
        ));
    }
    ToolOutput { ok: !result.is_error, text, version }
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

/// A call's result in a line or two, for its row in the pane.
pub fn summary(out: &ToolOutput) -> String {
    let first: String = out.text.lines().take(2).collect::<Vec<_>>().join(" ");
    let mut s = crate::backend::label(&first);
    if s.chars().count() >= 160 {
        s = s.chars().take(160).collect::<String>() + "…";
    }
    s
}

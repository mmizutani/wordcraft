//! ACP agents (Claude Code, Codex, Pi or any other) as assistant backends ([`Acp`]): WordCraft
//! is the Agent Client Protocol's client, and the agent a process it starts, speaking JSON-RPC
//! 2.0 over its stdin and stdout, one message per line (<https://agentclientprotocol.com>).
//!
//! The agent runs its own model loop with its own sign-in. WordCraft offers it no file and no
//! terminal: it starts in a new empty folder of its own, and every request for the computer's
//! files or a terminal is refused. It reaches the shared document as the participant the
//! conversation joined as, through WordCraft's MCP server.
//!
//! The agent starts with the conversation's first turn, so Stop can interrupt it (an adapter
//! run through `npx` may first be downloaded) and a failure is that turn's error; it starts
//! again, with a new session, on the turn after it stopped. It ends with the conversation.
//!
//! Desktop only: the web build starts no process.

pub mod fake_agent;
mod rpc;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::backend::{
    Answer, Backend, BackendInfo, Event, MAX_OPTIONS, MAX_PLAN, PermissionKind, PermissionOption, PermissionPolicy, PermissionRequest, PlanEntry,
    PlanStatus, Priority, StopReason, ToolCall, ToolStatus, Turn, TurnCtx, label,
};
pub use rpc::{AUTH_REQUIRED, MAX_LINE, METHOD_NOT_FOUND, STDERR_TAIL};
use rpc::{Incoming, Message, Process, RpcError, classify};

/// The ACP version WordCraft speaks.
pub const PROTOCOL_VERSION: u64 = 1;

/// Most tool calls of one turn whose rows are kept to merge their updates into.
const MAX_CALLS: usize = 1024;

/// How long `authenticate` may take: the person signs in, maybe in a browser.
const AUTH_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How long the agent has to exit once its input is closed, before it's killed.
const EXIT_GRACE: Duration = Duration::from_secs(2);

/// How long an agent that didn't stop when asked has to exit before it's killed.
const STUCK_GRACE: Duration = Duration::from_millis(200);

/// How often a wait for the agent looks at the turn's Stop.
const TICK: Duration = Duration::from_millis(50);

/// The id of the answer that declines to sign in, which no agent's method may have.
const NOT_NOW: &str = "wordcraft-not-now";

/// The id of the row that shows the agent starting.
pub const START_ROW: &str = "wordcraft-start";

/// How an agent is started: its program and arguments, no shell between.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    /// The program: an absolute path, or a name the system looks up on `PATH`.
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

impl Launch {
    /// `program` with `args`.
    pub fn new(program: impl Into<PathBuf>, args: impl IntoIterator<Item = impl Into<OsString>>) -> Launch {
        Launch { program: program.into(), args: args.into_iter().map(Into::into).collect() }
    }
}

/// An ACP agent's set-up: what it is, how it's started and how long it may take.
#[derive(Clone, Debug)]
pub struct AgentSpec {
    /// Its id, as `assist.backend {id}` names it (`acp:claude-code`…).
    pub id: String,
    /// Its name in the picker.
    pub label: String,
    /// The name it joins the shared document under ("Claude Code"…).
    pub author: String,
    pub launch: Launch,
    /// Environment variables for its process, on top of WordCraft's own environment less
    /// WordCraft's `WORDCRAFT_*` variables, which never reach an agent unless set here.
    pub env: Vec<(String, String)>,
    /// How long it may take to start: to answer `initialize` and `session/new` (`npx`
    /// downloads the adapter the first time).
    pub start_timeout: Duration,
    /// How long a stopped turn waits for the agent to say it stopped, before the agent is
    /// stopped too (it starts again with the next turn).
    pub cancel_timeout: Duration,
}

impl AgentSpec {
    /// An agent called `name`, started with `launch`; its id is `acp:` and the name in lower
    /// case, with dashes for spaces.
    pub fn new(name: &str, launch: Launch) -> AgentSpec {
        let name = label(name.trim());
        let slug: String = name.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
        AgentSpec {
            id: format!("acp:{slug}"),
            label: name.clone(),
            author: name,
            launch,
            env: Vec::new(),
            start_timeout: Duration::from_secs(120),
            cancel_timeout: Duration::from_secs(10),
        }
    }
}

/// An agent's way to sign in, from its `authMethods`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthMethod {
    pub id: String,
    pub name: String,
    pub description: String,
}

/// The agent running for a conversation.
struct Running {
    process: Process,
    /// Its folder: new and empty, removed when it stops.
    folder: Folder,
    session: String,
}

/// An ACP agent as a backend.
pub struct Acp {
    spec: AgentSpec,
    running: Option<Running>,
    /// The note of the conversation's first turn, which every session the agent starts gets.
    note: Option<String>,
    /// Its tool calls in the current turn, to merge their updates into.
    calls: HashMap<String, ToolCall>,
}

impl Acp {
    /// A backend that runs the agent `spec` sets up.
    pub fn new(spec: AgentSpec) -> Acp {
        Acp { spec, running: None, note: None, calls: HashMap::new() }
    }

    /// The agent's name, for messages.
    fn name(&self) -> &str {
        &self.spec.label
    }

    /// The MCP servers the agent gets in `session/new`.
    fn mcp_servers(&self) -> Vec<Value> {
        Vec::new()
    }

    /// Start the agent, shown as a row of the turn: whether it started (not when the turn was
    /// stopped meanwhile).
    fn start_shown(&mut self, ctx: &mut TurnCtx<'_>) -> Result<bool, String> {
        let row = ToolCall {
            id: START_ROW.into(),
            title: format!("Start {}", self.name()),
            kind: "other".into(),
            status: ToolStatus::InProgress,
            detail: String::new(),
            version: None,
        };
        ctx.emit(Event::Tool(row.clone()));
        let started = self.start(ctx);
        let (status, detail) = match &started {
            Ok(Some(_)) => (ToolStatus::Completed, String::new()),
            Ok(None) => (ToolStatus::Failed, "Stopped".to_string()),
            Err(e) => (ToolStatus::Failed, e.clone()),
        };
        ctx.emit(Event::Tool(ToolCall { status, detail, ..row }));
        let Some((running, agent)) = started? else { return Ok(false) };
        self.running = Some(running);
        if let Some(name) = agent {
            ctx.emit(Event::Model { name });
        }
        Ok(true)
    }

    /// Start the agent: its process in a new empty folder, `initialize`, and a session (signing
    /// in first when it asks for that and the person picks a way), with what it says it is.
    /// `None` when the turn was stopped meanwhile.
    fn start(&mut self, ctx: &mut TurnCtx<'_>) -> Result<Option<(Running, Option<String>)>, String> {
        let name = self.name().to_string();
        let folder = Folder::new().map_err(|e| format!("{name} couldn't start: no folder for it ({e})"))?;
        let cwd = folder.path().to_str().ok_or_else(|| format!("{name} couldn't start: the temporary folder's path isn't text"))?.to_string();
        let env = agent_env(std::env::vars_os(), &self.spec.env);
        let mut process = Process::spawn(&self.spec.launch, folder.path(), env).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("{name} isn't installed: `{}` wasn't found", label(&self.spec.launch.program.to_string_lossy()))
            } else {
                format!("{name} couldn't start: {e}")
            }
        })?;
        let mut until = Instant::now() + self.spec.start_timeout;
        let init = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
            "clientInfo": {"name": "wordcraft", "title": "WordCraft", "version": env!("CARGO_PKG_VERSION")},
        });
        let id = process.request("initialize", init)?;
        let Some(answer) = wait_for(&mut process, id, until, ctx, &name)? else { return Ok(None) };
        let init = answer.map_err(|e| format!("{name} couldn't start: {}", e.message))?;
        let version = init.get("protocolVersion").and_then(Value::as_u64);
        if version != Some(PROTOCOL_VERSION) {
            let theirs = version.map_or_else(|| "an unknown version".to_string(), |v| format!("version {v}"));
            return Err(format!("{name} speaks {theirs} of the Agent Client Protocol, and WordCraft speaks version {PROTOCOL_VERSION}"));
        }
        let agent = agent_info(&init);
        let methods = auth_methods(&init);
        let new_session = json!({"cwd": cwd, "mcpServers": self.mcp_servers()});
        let mut signed_in = false;
        loop {
            let id = process.request("session/new", new_session.clone())?;
            let Some(answer) = wait_for(&mut process, id, until, ctx, &name)? else { return Ok(None) };
            match answer {
                Ok(result) => {
                    let session = result.get("sessionId").and_then(Value::as_str).filter(|s| !s.is_empty() && s.len() <= 1024);
                    let session = session.ok_or_else(|| format!("{name} started no session"))?.to_string();
                    return Ok(Some((Running { process, folder, session }, agent)));
                }
                Err(e) if e.code == AUTH_REQUIRED && !signed_in => {
                    if !self.sign_in(&mut process, &methods, ctx)? {
                        return Ok(None);
                    }
                    signed_in = true;
                    // Time spent signing in leaves the session a full startup budget.
                    until = Instant::now() + self.spec.start_timeout;
                }
                Err(e) if e.code == AUTH_REQUIRED => return Err(format!("{name} still needs you to sign in: {}", e.message)),
                Err(e) => return Err(format!("{name} couldn't start a session: {}", e.message)),
            }
        }
    }

    /// The agent needs the person to sign in: in the window, ask which of its `methods`, then
    /// `authenticate`. Whether it signed in; `false` when the turn was stopped. With nobody to
    /// ask, or when the person declines, it's an error that names the agent's ways to sign in.
    fn sign_in(&self, process: &mut Process, methods: &[AuthMethod], ctx: &mut TurnCtx<'_>) -> Result<bool, String> {
        let name = self.name();
        if methods.is_empty() {
            return Err(format!(
                "{name} needs you to sign in, and offers no way to do it from WordCraft: sign in with {name} itself, then try again"
            ));
        }
        let ways = methods.iter().map(|m| m.name.as_str()).collect::<Vec<_>>().join(", ");
        let declined = || format!("{name} needs you to sign in ({ways}): sign in from the assistant pane, or with {name} itself, then try again");
        if ctx.policy != PermissionPolicy::Ask {
            return Err(declined());
        }
        let options = methods
            .iter()
            .map(|m| PermissionOption { id: m.id.clone(), name: m.name.clone(), kind: PermissionKind::AllowOnce })
            .chain([PermissionOption { id: NOT_NOW.into(), name: "Not now".into(), kind: PermissionKind::RejectOnce }]);
        let method = match ctx.ask(PermissionRequest::bounded(&format!("{name} needs you to sign in"), options)) {
            Answer::Selected { option } if option != NOT_NOW => option,
            _ if ctx.is_cancelled() => return Ok(false),
            _ => return Err(declined()),
        };
        let id = process.request("authenticate", json!({"methodId": method}))?;
        match wait_for(process, id, Instant::now() + AUTH_TIMEOUT, ctx, name)? {
            None => Ok(false),
            Some(Ok(_)) => Ok(true),
            Some(Err(e)) => Err(format!("{name} couldn't sign you in: {}", e.message)),
        }
    }

    /// Stop the agent, giving it `grace` to exit, and remove its folder. It starts again with
    /// the next turn.
    fn stop_agent(&mut self, grace: Duration) {
        if let Some(mut running) = self.running.take() {
            running.process.stop(grace);
            running.folder.remove();
        }
    }

    /// Show tool call `update` (a `tool_call`, a `tool_call_update` or a permission request's
    /// `toolCall`): its row, with what this turn knew of it before.
    fn show_call(&mut self, update: &Value, ctx: &mut TurnCtx<'_>) {
        let Some(id) = update.get("toolCallId").and_then(Value::as_str).filter(|i| !i.is_empty()) else { return };
        let mut call = self.calls.get(id).cloned().unwrap_or_else(|| ToolCall {
            id: id.to_string(),
            title: String::new(),
            kind: "other".into(),
            status: ToolStatus::Pending,
            detail: String::new(),
            version: None,
        });
        if let Some(title) = update.get("title").and_then(Value::as_str).filter(|t| !t.trim().is_empty()) {
            call.title = title.to_string();
        }
        if let Some(kind) = update.get("kind").and_then(Value::as_str).filter(|k| !k.is_empty()) {
            call.kind = kind.to_string();
        }
        if let Some(status) = update.get("status").and_then(Value::as_str).and_then(tool_status) {
            call.status = status;
        }
        if let Some(content) = update.get("content").and_then(Value::as_array) {
            call.detail = tool_detail(content);
        }
        if call.title.is_empty() {
            call.title = "Tool call".into();
        }
        let Event::Tool(call) = Event::Tool(call).bounded() else { return };
        if self.calls.len() < MAX_CALLS || self.calls.contains_key(&call.id) {
            self.calls.insert(call.id.clone(), call.clone());
        }
        ctx.emit(Event::Tool(call));
    }

    /// Show what `session/update` says.
    fn update(&mut self, params: &Value, ctx: &mut TurnCtx<'_>) {
        let Some(update) = params.get("update") else { return };
        let text = || update.get("content").map(content_text).unwrap_or_default();
        match update.get("sessionUpdate").and_then(Value::as_str) {
            Some("agent_message_chunk") => {
                let text = text();
                if !text.is_empty() {
                    ctx.say(text);
                }
            }
            Some("agent_thought_chunk") => {
                let text = text();
                if !text.is_empty() {
                    ctx.emit(Event::Thought { text });
                }
            }
            Some("tool_call" | "tool_call_update") => self.show_call(update, ctx),
            Some("plan") => {
                let entries = update.get("entries").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
                ctx.emit(Event::Plan { entries: entries.iter().filter_map(plan_entry).take(MAX_PLAN).collect() });
            }
            // The person's prompt echoed, modes, commands, usage…: nothing to show.
            _ => {}
        }
    }

    /// Answer the agent's request `method` (`id`, `params`) during a turn: a permission request
    /// goes to the person or the conversation's policy (cancelled once the turn was stopped);
    /// anything else (files, terminals) is refused.
    fn answer_request(&mut self, id: &Value, method: &str, params: &Value, stopped: bool, ctx: &mut TurnCtx<'_>) -> Result<(), String> {
        let cancelled = json!({"outcome": {"outcome": "cancelled"}});
        if method != "session/request_permission" {
            log::info!("assistant: {} asked for `{}`, which WordCraft doesn't offer", self.spec.label, label(method));
            return self.respond(id, Err((METHOD_NOT_FOUND, "Method not found")));
        }
        if stopped {
            return self.respond(id, Ok(cancelled));
        }
        let call = params.get("toolCall").cloned().unwrap_or(Value::Null);
        self.show_call(&call, ctx);
        let known = call.get("toolCallId").and_then(Value::as_str).and_then(|i| self.calls.get(i)).map(|c| c.title.clone());
        let title = known.unwrap_or_else(|| "Do something".into());
        let options = params.get("options").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
        let request = PermissionRequest::bounded(&title, options.iter().take(MAX_OPTIONS.saturating_mul(4)).filter_map(permission_option));
        if request.options.is_empty() {
            // Nothing to answer with.
            return self.respond(id, Ok(cancelled));
        }
        match ctx.ask(request) {
            Answer::Selected { option } => self.respond(id, Ok(json!({"outcome": {"outcome": "selected", "optionId": option}}))),
            Answer::Cancelled => self.respond(id, Ok(cancelled)),
        }
    }

    /// Answer the agent's request `id`.
    fn respond(&mut self, id: &Value, answer: Result<Value, (i64, &str)>) -> Result<(), String> {
        let Some(running) = self.running.as_mut() else { return Ok(()) };
        match answer {
            Ok(result) => running.process.respond(id, result),
            Err((code, message)) => running.process.respond_error(id, code, message),
        }
    }

    /// Wait for the answer to `session/prompt` (`id`), showing what the agent sends meanwhile
    /// and answering its requests. Stop sends `session/cancel` and waits a while for the agent
    /// to say it stopped; an agent that doesn't is stopped.
    fn finish_turn(&mut self, id: i64, ctx: &mut TurnCtx<'_>) -> Result<StopReason, String> {
        let name = self.name().to_string();
        let mut stopped_at: Option<Instant> = None;
        loop {
            if stopped_at.is_some_and(|at| at.elapsed() >= self.spec.cancel_timeout) {
                log::info!("assistant: {name} didn't say it stopped; it is stopped");
                self.stop_agent(STUCK_GRACE);
                return Ok(StopReason::Cancelled);
            }
            let Some(running) = self.running.as_mut() else { return Err(format!("{name} isn't running")) };
            if stopped_at.is_none() && ctx.is_cancelled() {
                stopped_at = Some(Instant::now());
                let session = running.session.clone();
                // A failed send means the agent is gone, which the next read says.
                let _ = running.process.notify("session/cancel", json!({"sessionId": session}));
            }
            let message = match running.process.next(TICK) {
                None => continue,
                Some(Incoming::Closed) => return Err(running.process.why_gone(&name)),
                Some(Incoming::Junk(why)) => {
                    log::debug!("assistant: {name} wrote {why}");
                    continue;
                }
                Some(Incoming::Message(m)) => m,
            };
            match classify(&message) {
                Message::Response { id: answered, result } if answered.as_i64() == Some(id) => {
                    return match result {
                        Ok(r) => Ok(stop_reason(r.get("stopReason").and_then(Value::as_str))),
                        Err(_) if stopped_at.is_some() => Ok(StopReason::Cancelled),
                        Err(e) if e.code == AUTH_REQUIRED => Err(format!("{name} needs you to sign in again: {}", e.message)),
                        Err(e) => Err(format!("{name}: {}", e.message)),
                    };
                }
                Message::Notification { method: "session/update", params } => self.update(params, ctx),
                Message::Request { id: asked, method, params } => self.answer_request(asked, method, params, stopped_at.is_some(), ctx)?,
                Message::Response { .. } | Message::Notification { .. } | Message::Invalid => {}
            }
        }
    }
}

impl Backend for Acp {
    fn info(&self) -> BackendInfo {
        BackendInfo { id: self.spec.id.clone(), label: self.spec.label.clone(), author: self.spec.author.clone(), in_process_tools: false }
    }

    fn turn(&mut self, turn: &Turn, ctx: &mut TurnCtx<'_>) -> Result<StopReason, String> {
        self.calls.clear();
        if turn.note.is_some() {
            self.note.clone_from(&turn.note);
        }
        // An agent that stopped since the last turn starts again.
        if self.running.as_mut().is_some_and(|r| !r.process.is_running()) {
            self.stop_agent(STUCK_GRACE);
        }
        let fresh = self.running.is_none();
        if fresh && !self.start_shown(ctx)? {
            return Ok(StopReason::Cancelled);
        }
        // A new session gets the note, which the agent's model reads first.
        let note = if fresh { self.note.as_deref() } else { None };
        let prompt: Vec<Value> = [note, turn.context.as_deref(), Some(turn.prompt.as_str())]
            .into_iter()
            .flatten()
            .filter(|t| !t.trim().is_empty())
            .map(|t| json!({"type": "text", "text": t}))
            .collect();
        let Some(running) = self.running.as_mut() else { return Err(format!("{} isn't running", self.spec.label)) };
        let session = running.session.clone();
        let ended = match running.process.request("session/prompt", json!({"sessionId": session, "prompt": prompt})) {
            Ok(id) => self.finish_turn(id, ctx),
            Err(e) => Err(format!("{}: {e}", self.spec.label)),
        };
        if ended.is_err() && self.running.as_mut().is_some_and(|r| !r.process.is_running()) {
            self.stop_agent(STUCK_GRACE);
        }
        ended
    }

    fn end(&mut self) {
        self.stop_agent(EXIT_GRACE);
    }
}

/// Wait until `process` answers request `id`, before `until`, refusing whatever it asks
/// meanwhile. `None` when the turn was stopped first.
fn wait_for(process: &mut Process, id: i64, until: Instant, ctx: &TurnCtx<'_>, name: &str) -> Result<Option<Result<Value, RpcError>>, String> {
    loop {
        if ctx.is_cancelled() {
            return Ok(None);
        }
        if Instant::now() >= until {
            return Err(format!("{name} didn't answer in time"));
        }
        let message = match process.next(TICK) {
            None => continue,
            Some(Incoming::Closed) => return Err(process.why_gone(name)),
            Some(Incoming::Junk(why)) => {
                log::debug!("assistant: {name} wrote {why}");
                continue;
            }
            Some(Incoming::Message(m)) => m,
        };
        match classify(&message) {
            Message::Response { id: answered, result } if answered.as_i64() == Some(id) => return Ok(Some(result.cloned())),
            Message::Request { id: asked, .. } => process.respond_error(asked, METHOD_NOT_FOUND, "Method not found")?,
            Message::Response { .. } | Message::Notification { .. } | Message::Invalid => {}
        }
    }
}

/// An agent's environment: `base` (WordCraft's) without WordCraft's own variables, which may
/// hold keys, and `extra` on top.
fn agent_env(base: impl IntoIterator<Item = (OsString, OsString)>, extra: &[(String, String)]) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> =
        base.into_iter().filter(|(k, _)| !k.to_string_lossy().to_ascii_uppercase().starts_with("WORDCRAFT_")).collect();
    env.extend(extra.iter().map(|(k, v)| (OsString::from(k), OsString::from(v))));
    env
}

/// What the agent says it is, from `initialize`'s answer: "Claude Code 1.2".
fn agent_info(init: &Value) -> Option<String> {
    let info = init.get("agentInfo")?;
    let title = |key: &str| info.get(key).and_then(Value::as_str).map(label).filter(|n| !n.trim().is_empty());
    let name = title("title").or_else(|| title("name"))?;
    Some(match title("version") {
        Some(v) => label(&format!("{name} {v}")),
        None => name,
    })
}

/// The agent's ways to sign in that it runs itself: never one that needs a terminal.
fn auth_methods(init: &Value) -> Vec<AuthMethod> {
    let methods = init.get("authMethods").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    methods
        .iter()
        .filter(|m| m.get("type").and_then(Value::as_str).is_none_or(|t| t == "agent"))
        .filter_map(|m| {
            let id = m.get("id").and_then(Value::as_str).filter(|i| !i.is_empty() && i.len() <= 256 && *i != NOT_NOW)?;
            let name = m.get("name").and_then(Value::as_str).map(label).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| label(id));
            let description = m.get("description").and_then(Value::as_str).map(label).unwrap_or_default();
            Some(AuthMethod { id: id.to_string(), name, description })
        })
        .take(MAX_OPTIONS.saturating_sub(1))
        .collect()
}

/// The text of a content block: its text, or a short placeholder for what isn't text.
fn content_text(block: &Value) -> String {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => block.get("text").and_then(Value::as_str).unwrap_or_default().to_string(),
        Some("resource_link") => {
            let name = block.get("name").and_then(Value::as_str).or_else(|| block.get("uri").and_then(Value::as_str)).unwrap_or("a link");
            format!("[{}]", label(name))
        }
        Some("image") => "[image]".into(),
        Some("audio") => "[audio]".into(),
        Some("resource") => "[resource]".into(),
        _ => String::new(),
    }
}

/// Most characters of a tool call's detail.
const MAX_DETAIL: usize = 300;

/// A tool call's content in a line or two: its text, or which file a diff changes.
fn tool_detail(content: &[Value]) -> String {
    let mut parts = Vec::new();
    for c in content.iter().take(8) {
        match c.get("type").and_then(Value::as_str) {
            Some("content") => parts.push(c.get("content").map(content_text).unwrap_or_default()),
            Some("diff") => parts.push(format!("Changes {}", c.get("path").and_then(Value::as_str).unwrap_or("a file"))),
            Some("terminal") => parts.push("Runs in a terminal".into()),
            _ => {}
        }
    }
    let text = parts.iter().flat_map(|p| p.lines()).map(str::trim).filter(|l| !l.is_empty()).take(2).collect::<Vec<_>>().join(" ");
    let text = label(&text);
    if text.chars().count() > MAX_DETAIL { text.chars().take(MAX_DETAIL).chain(['…']).collect() } else { text }
}

fn tool_status(s: &str) -> Option<ToolStatus> {
    Some(match s {
        "pending" => ToolStatus::Pending,
        "in_progress" => ToolStatus::InProgress,
        "completed" => ToolStatus::Completed,
        "failed" => ToolStatus::Failed,
        _ => return None,
    })
}

/// One of the agent's plan entries; `None` without content.
fn plan_entry(e: &Value) -> Option<PlanEntry> {
    let content = e.get("content").and_then(Value::as_str).map(label).filter(|c| !c.trim().is_empty())?;
    let priority = match e.get("priority").and_then(Value::as_str) {
        Some("high") => Priority::High,
        Some("low") => Priority::Low,
        _ => Priority::Medium,
    };
    let status = match e.get("status").and_then(Value::as_str) {
        Some("in_progress") => PlanStatus::InProgress,
        Some("completed") => PlanStatus::Completed,
        _ => PlanStatus::Pending,
    };
    Some(PlanEntry { content, priority, status })
}

/// One of the options of the agent's permission request; `None` when it can't be answered.
fn permission_option(o: &Value) -> Option<PermissionOption> {
    let id = o.get("optionId").and_then(Value::as_str).filter(|i| !i.is_empty())?;
    let kind = match o.get("kind").and_then(Value::as_str)? {
        "allow_once" => PermissionKind::AllowOnce,
        "allow_always" => PermissionKind::AllowAlways,
        "reject_once" => PermissionKind::RejectOnce,
        "reject_always" => PermissionKind::RejectAlways,
        _ => return None,
    };
    let name = o.get("name").and_then(Value::as_str).filter(|n| !n.trim().is_empty()).unwrap_or(id);
    Some(PermissionOption { id: id.to_string(), name: name.to_string(), kind })
}

/// The stop reason `session/prompt` answered; an unknown one ends the turn as finished.
fn stop_reason(s: Option<&str>) -> StopReason {
    match s {
        Some("max_tokens") => StopReason::MaxTokens,
        Some("max_turn_requests") => StopReason::MaxTurnRequests,
        Some("refusal") => StopReason::Refusal,
        Some("cancelled") => StopReason::Cancelled,
        _ => StopReason::EndTurn,
    }
}

/// A new empty folder of an agent's own under the system's temporary folder (on Unix only its
/// owner may enter it), removed with everything in it at the end.
struct Folder(Option<PathBuf>);

impl Folder {
    fn new() -> std::io::Result<Folder> {
        let base = std::env::temp_dir();
        let mut last = std::io::Error::other("no name was free");
        for _ in 0..16 {
            let mut bytes = [0u8; 12];
            getrandom::fill(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
            let name: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            let path = base.join(format!("wordcraft-agent-{name}"));
            // Only Unix sets the folder's mode, so elsewhere the builder isn't changed.
            #[cfg_attr(not(unix), allow(unused_mut))]
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            match builder.create(&path) {
                Ok(()) => return Ok(Folder(Some(path))),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = e,
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }

    fn path(&self) -> &Path {
        self.0.as_deref().unwrap_or_else(|| Path::new(""))
    }

    fn remove(&mut self) {
        if let Some(path) = self.0.take()
            && let Err(e) = std::fs::remove_dir_all(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("assistant: the agent's folder {} wasn't removed: {e}", path.display());
        }
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        self.remove();
    }
}

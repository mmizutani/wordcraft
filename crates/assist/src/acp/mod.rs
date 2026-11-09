//! ACP agents (Claude Code, Codex, Pi or any other) as assistant backends ([`Acp`]): WordCraft
//! is the Agent Client Protocol's client, and the agent a process it starts, speaking JSON-RPC
//! 2.0 over its stdin and stdout, one message per line (<https://agentclientprotocol.com>).
//!
//! The agent runs its own model loop with its own sign-in. WordCraft offers it no file and no
//! terminal: it starts in a new empty folder of its own, and every request for the computer's
//! files or a terminal is refused. It reaches the shared document as the participant the
//! conversation joined as ([`Reach`]): `session/new` gives it WordCraft's MCP server
//! (`wordcraft-cli mcp --connect`), which joins the room at its own port ([`RoomPort`]) with
//! the invite's key, and so works on a copy of its own, held to its role, reaching no file. An
//! agent whose adapter doesn't pass MCP servers on (Pi) gets the invite's key and the room's
//! port in its environment instead, for `wordcraft-cli call` in its own shell. The window's own
//! key reaches the agent by no path: not in `session/new`, not in its environment (WordCraft's
//! `WORDCRAFT_*` variables are left out), and the room's port doesn't take it.
//!
//! The agent starts with the conversation's first turn, so Stop can interrupt it (an adapter
//! run through `npx` may first be downloaded) and a failure is that turn's error; it starts
//! again, with a new session, on the turn after it stopped. It ends with the conversation.
//!
//! Desktop only: the web build starts no process.

mod config;
pub mod fake_agent;
pub mod presets;
mod room_port;
mod rpc;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::ffi::OsString;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wordcraft_mcp::Backend as _;

use crate::backend::{
    Answer, Backend, BackendInfo, Event, MAX_OPTIONS, MAX_PLAN, MAX_SOURCES, Offered, PermissionKind, PermissionOption, PermissionPolicy,
    PermissionRequest, PlanEntry, PlanStatus, Priority, Seat, Source, SourceKind, StopReason, ToolCall, ToolStatus, Turn, TurnCtx, Usage, label,
};
use config::{Config, Select};
pub use room_port::RoomPort;
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

/// The variable that holds the key an agent joins the shared document with, when it reaches it
/// through its shell ([`Reach::Shell`]).
pub const KEY_VAR: &str = "WORDCRAFT_CONTROL_KEY";

/// The variable that holds the room's address, with [`KEY_VAR`].
pub const ADDR_VAR: &str = "WORDCRAFT_CONTROL_ADDR";

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

/// How an agent reaches the shared document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Reach {
    /// It doesn't: no tools (a test of the protocol alone).
    #[default]
    Nothing,
    /// Through WordCraft's MCP server, which `session/new` gives it: `cli mcp --connect room`
    /// (the room's own port, [`RoomPort`]), with the invite's key in the server's environment.
    /// It joins as the conversation's participant, with its role, and reaches only the files File ›
    /// Options › Agents lets agents use.
    Mcp {
        /// `wordcraft-cli`, an absolute path.
        cli: PathBuf,
        /// Where the room listens: `127.0.0.1` and a port.
        room: SocketAddr,
    },
    /// Through its own shell, for an agent whose adapter doesn't pass MCP servers on (Pi): the
    /// invite's key in [`KEY_VAR`] and the room's address in [`ADDR_VAR`] in its environment,
    /// which its shell's commands inherit, and the first turn's note says how to run
    /// `cli call`. `session/new` gives it WordCraft's MCP server too, for when it does.
    Shell {
        /// `wordcraft-cli`, an absolute path.
        cli: PathBuf,
        /// Where the room listens: `127.0.0.1` and a port.
        room: SocketAddr,
    },
}

impl Reach {
    /// `wordcraft-cli` as the agent is given it, and the room: an absolute path in text, and
    /// 127.0.0.1, where WordCraft sends keys. `None` for [`Reach::Nothing`].
    fn checked(&self) -> Result<Option<(&str, SocketAddr)>, String> {
        let (Reach::Mcp { cli, room } | Reach::Shell { cli, room }) = self else { return Ok(None) };
        let command = cli.to_str().filter(|_| cli.is_absolute()).ok_or_else(|| {
            format!("WordCraft's command line tool, {}, isn't an absolute path the agent can be given", label(&cli.to_string_lossy()))
        })?;
        if room.ip() != Ipv4Addr::LOCALHOST {
            return Err(format!("the shared document's room listens on {room}, not on 127.0.0.1, where WordCraft sends keys"));
        }
        Ok(Some((command, *room)))
    }

    /// WordCraft's MCP server for an agent that joins with invite `key`, as `session/new`'s
    /// `mcpServers` has it: none for [`Reach::Nothing`].
    pub fn mcp_servers(&self, key: &str) -> Result<Vec<Value>, String> {
        let Some((command, room)) = self.checked()? else { return Ok(Vec::new()) };
        Ok(vec![json!({
            "name": "wordcraft",
            "command": command,
            "args": ["mcp", "--connect", room.to_string()],
            "env": [{"name": KEY_VAR, "value": key}],
        })])
    }

    /// What the agent's own environment gets for invite `key`: the key and the room's address
    /// for [`Reach::Shell`], nothing otherwise.
    pub fn env(&self, key: &str) -> Result<Vec<(String, String)>, String> {
        match self.checked()? {
            Some((_, room)) if matches!(self, Reach::Shell { .. }) => Ok(vec![(KEY_VAR.into(), key.into()), (ADDR_VAR.into(), room.to_string())]),
            _ => Ok(Vec::new()),
        }
    }

    /// What the first turn's note adds: for [`Reach::Shell`], how to reach the document with
    /// `wordcraft-cli call` from the agent's shell.
    pub fn note(&self) -> Option<String> {
        let Reach::Shell { cli, .. } = self else { return None };
        let cli = shell_word(&cli.to_string_lossy());
        Some(format!(
            "If you have no `wordcraft` tools, reach the document with your shell: `{cli} call <method> '<JSON params>'` prints the \
             result as JSON, or fails with the error (your environment holds the document's address and your key: never print, show \
             or pass them on). Read the text with `{cli} call document.text`, its structure and version with `{cli} call \
             document.inspect`, and what others changed since version N with `{cli} call document.changes '{{\"since\": N}}'`. Run \
             a command with `{cli} call <command id> '<params>'`: select with `select.text '{{\"text\": \"…\"}}'` and type over \
             the selection with `text.insert '{{\"text\": \"…\", \"raw\": true}}'`; find commands with `{cli} call \
             engine.commands '{{\"search\": \"…\"}}'`. Params given as `-` are read from standard input, which saves quoting."
        ))
    }
}

/// `text` as one word of a POSIX shell's command line: as it is when that is safe, else in
/// single quotes.
fn shell_word(text: &str) -> String {
    let plain = !text.is_empty() && text.chars().all(|c| c.is_alphanumeric() || "/._-+:@%".contains(c));
    if plain { text.to_string() } else { format!("'{}'", text.replace('\'', "'\\''")) }
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
    /// The exact PATH used for discovery and this child, including trusted desktop fallbacks.
    pub path: Option<OsString>,
    /// How long it may take to start: to answer `initialize` and `session/new` (`npx`
    /// downloads the adapter the first time).
    pub start_timeout: Duration,
    /// How long a stopped turn waits for the agent to say it stopped, before the agent is
    /// stopped too (it starts again with the next turn).
    pub cancel_timeout: Duration,
    /// How it reaches the shared document.
    pub reach: Reach,
    /// How to sign in when it needs that and offers no way WordCraft can run ("run `claude`
    /// in a terminal and sign in, then try again").
    pub sign_in: String,
    /// An automatic npx package whose download and execution need a person before each spawn.
    /// Explicit custom commands leave this unset.
    pub setup: Option<String>,
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
            path: None,
            start_timeout: Duration::from_secs(120),
            cancel_timeout: Duration::from_secs(10),
            reach: Reach::Nothing,
            sign_in: "sign in with the agent itself, then try again".into(),
            setup: None,
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

/// Most messages from the agent read between two turns before the next one goes on.
const MAX_CATCH_UP: usize = 4096;

/// The agent running for a conversation.
struct Running {
    process: Process,
    /// Its folder: new and empty, removed when it stops.
    folder: Folder,
    session: String,
    /// Whether it takes `resource` blocks in a prompt (`embeddedContext`).
    embedded_context: bool,
    /// The session's config options, as the agent last said them.
    config: Config,
    /// What the model and the level were before WordCraft changed them: Default goes back to it.
    before: (Option<String>, Option<String>),
    /// Whether a prompt went to this session. A turn can stop before its first one, while a
    /// choice is set, and the next turn then has what the first would have sent: the note, which
    /// the agent can't work without, and what it offers, for the menus.
    prompted: bool,
}

/// An ACP agent as a backend.
pub struct Acp {
    spec: AgentSpec,
    /// The conversation's seat in the shared document: the agent joins with its invite's key.
    seat: Option<Seat>,
    running: Option<Running>,
    /// The note of the conversation's first turn, which every session the agent starts gets.
    note: Option<String>,
    /// Its tool calls in the current turn, to merge their updates into.
    calls: HashMap<String, ToolCall>,
    /// The shared document's version when each call that is still running was first seen: its
    /// changes are the assistant's after it.
    started: HashMap<String, u64>,
}

impl Running {
    /// The config option for the model or the level, and what it was before WordCraft changed it.
    fn slot(&mut self, is_model: bool) -> (Option<&mut Select>, &mut Option<String>) {
        if is_model { (self.config.model.as_mut(), &mut self.before.0) } else { (self.config.thought.as_mut(), &mut self.before.1) }
    }
}

impl Acp {
    /// A backend that runs the agent `spec` sets up.
    pub fn new(spec: AgentSpec) -> Acp {
        Acp { spec, seat: None, running: None, note: None, calls: HashMap::new(), started: HashMap::new() }
    }

    /// The agent's name, for messages.
    fn name(&self) -> &str {
        &self.spec.label
    }

    /// How the agent reaches the document with the seat's key: the MCP servers it gets in
    /// `session/new`, and what its environment gets.
    fn reach(&self) -> Result<(Vec<Value>, Vec<(String, String)>), String> {
        let key = self.seat.as_ref().map(|s| s.key.as_str()).unwrap_or_default();
        if key.is_empty() && self.spec.reach != Reach::Nothing {
            return Err(format!("{} has no seat in the shared document", self.name()));
        }
        let why = |e: String| format!("{} can't reach the document: {e}", self.name());
        Ok((self.spec.reach.mcp_servers(key).map_err(why)?, self.spec.reach.env(key).map_err(why)?))
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
            changes: None,
            sources: Vec::new(),
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
            ctx.emit(Event::Agent { name });
        }
        Ok(true)
    }

    /// Start the agent: its process in a new empty folder, `initialize`, and a session (signing
    /// in first when it asks for that and the person picks a way), with what it says it is.
    /// `None` when the turn was stopped meanwhile.
    fn start(&mut self, ctx: &mut TurnCtx<'_>) -> Result<Option<(Running, Option<String>)>, String> {
        let name = self.name().to_string();
        let (mcp_servers, reach_env) = self.reach()?;
        if !self.approve_setup(ctx)? || ctx.is_cancelled() {
            return Ok(None);
        }
        let folder = Folder::new().map_err(|e| format!("{name} couldn't start: no folder for it ({e})"))?;
        let cwd = folder.path().to_str().ok_or_else(|| format!("{name} couldn't start: the temporary folder's path isn't text"))?.to_string();
        // The key and the room's address come from the seat only, never from the spec's own.
        let mut extra: Vec<(String, String)> =
            self.spec.env.iter().filter(|(k, _)| ![KEY_VAR, ADDR_VAR].contains(&k.to_ascii_uppercase().as_str())).cloned().collect();
        extra.extend(reach_env);
        let mut env = agent_env(std::env::vars_os(), &extra);
        if let Some(path) = &self.spec.path {
            env.retain(|(key, _)| if cfg!(windows) { !key.to_string_lossy().eq_ignore_ascii_case("PATH") } else { key != "PATH" });
            env.push(("PATH".into(), path.clone()));
        }
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
        let embedded_context = init.pointer("/agentCapabilities/promptCapabilities/embeddedContext").and_then(Value::as_bool).unwrap_or(false);
        let methods = auth_methods(&init);
        let new_session = json!({"cwd": cwd, "mcpServers": mcp_servers});
        let mut signed_in = false;
        loop {
            let id = process.request("session/new", new_session.clone())?;
            let Some(answer) = wait_for(&mut process, id, until, ctx, &name)? else { return Ok(None) };
            match answer {
                Ok(result) => {
                    let session = result.get("sessionId").and_then(Value::as_str).filter(|s| !s.is_empty() && s.len() <= 1024);
                    let session = session.ok_or_else(|| format!("{name} started no session"))?.to_string();
                    let running =
                        Running { process, folder, session, embedded_context, config: Config::parse(&result), before: (None, None), prompted: false };
                    return Ok(Some((running, agent)));
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

    /// Built-in automatic setup downloads and runs code, so only a person may allow it.
    /// This is asked before every new process, never remembered across agent sessions.
    fn approve_setup(&self, ctx: &mut TurnCtx<'_>) -> Result<bool, String> {
        let Some(package) = &self.spec.setup else { return Ok(true) };
        let declined = || {
            format!("{} needs your approval to download and run {package}; approve in the assistant pane, or install its adapter first", self.name())
        };
        if ctx.policy != PermissionPolicy::Ask {
            return Err(declined());
        }
        let program = self.spec.launch.program.file_name().unwrap_or_default().to_string_lossy();
        let options = [
            PermissionOption { id: "wordcraft-setup".into(), name: "Download and run".into(), kind: PermissionKind::AllowOnce },
            PermissionOption { id: NOT_NOW.into(), name: "Not now".into(), kind: PermissionKind::RejectOnce },
        ];
        match ctx.ask(PermissionRequest::bounded(&format!("Download and run {package} using {program}?"), options)) {
            Answer::Selected { option } if option == "wordcraft-setup" => Ok(true),
            _ if ctx.is_cancelled() => Ok(false),
            _ => Err(declined()),
        }
    }

    /// The agent needs the person to sign in: in the window, ask which of its `methods`, then
    /// `authenticate`. Whether it signed in; `false` when the turn was stopped. With nobody to
    /// ask, or when the person declines, it's an error that names the agent's ways to sign in.
    fn sign_in(&self, process: &mut Process, methods: &[AuthMethod], ctx: &mut TurnCtx<'_>) -> Result<bool, String> {
        let name = self.name();
        let how = &self.spec.sign_in;
        if methods.is_empty() {
            return Err(format!("{name} needs you to sign in, and offers no way to do it from WordCraft: {how}"));
        }
        let ways = methods.iter().map(|m| m.name.as_str()).collect::<Vec<_>>().join(", ");
        let declined = || format!("{name} needs you to sign in ({ways}): sign in from the assistant pane, or {how}");
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

    /// Set what the person chose for this turn, model first: its answer may change the levels.
    /// A value it no longer offers is skipped, with a note, and so is one it refuses: neither stops
    /// the turn. Nothing chosen goes back to what WordCraft changed, once the agent takes it.
    /// Whether anything changed.
    fn apply_choice(&mut self, turn: &Turn, ctx: &mut TurnCtx<'_>) -> Result<bool, String> {
        let mut changed = false;
        for (is_model, wanted) in [(true, turn.model.as_ref()), (false, turn.effort.as_ref())] {
            let name = self.name().to_string();
            let until = Instant::now() + self.spec.start_timeout;
            let Some(running) = self.running.as_mut() else { break };
            let session = running.session.clone();
            let (Some(option), before) = running.slot(is_model) else { continue };
            let Some(target) = wanted.or(before.as_ref()).cloned() else { continue };
            if !option.offers(&target) {
                if wanted.is_some() {
                    ctx.emit(Event::Note { text: format!("{name} doesn't offer {target} now: it goes on with {}.", option.current) });
                }
                continue;
            }
            let mut taken = target == option.current;
            if !taken {
                if wanted.is_some() && before.is_none() {
                    *before = Some(option.current.clone());
                }
                let params = json!({"sessionId": session, "configId": option.id, "value": target});
                let asked = running.process.request("session/set_config_option", params)?;
                let answered = self.answer_to(asked, until, ctx);
                let Some(running) = self.running.as_mut() else { break };
                match answered {
                    Ok(Some(Ok(answer))) => {
                        if answer.get("configOptions").is_some() {
                            running.config = Config::parse(&answer);
                        } else if let (Some(option), _) = running.slot(is_model) {
                            // The protocol says it answers with all of them; this one didn't.
                            option.current = target;
                        }
                        taken = true;
                        changed = true;
                    }
                    Ok(Some(Err(e))) => ctx.emit(Event::Note { text: format!("{name} didn't change to {target}: {}", e.message) }),
                    stopped_or_stuck => {
                        // It may still take the value: what it runs isn't known, so the next turn
                        // says what it wants whatever was held.
                        if let (Some(option), _) = running.slot(is_model) {
                            option.current.clear();
                        }
                        return stopped_or_stuck.map(|_| changed);
                    }
                }
            }
            if taken
                && wanted.is_none()
                && let Some(running) = self.running.as_mut()
            {
                *running.slot(is_model).1 = None;
            }
        }
        Ok(changed)
    }

    /// Tell the pane what the session's config options offer and which model runs. What is offered
    /// says what runs with nothing chosen: what Default goes back to, or what the agent has.
    fn show_config(&self, ctx: &TurnCtx<'_>, offer: bool) {
        let Some(running) = &self.running else { return };
        if offer {
            let offered = running.config.offered();
            let (model, effort) = running.before.clone();
            ctx.emit(Event::Options(Offered { model: model.or(offered.model.clone()), effort: effort.or(offered.effort.clone()), ..offered }));
        }
        if let Some((name, effort)) = running.config.line() {
            ctx.emit(Event::Model { name, effort, asked: false });
        }
    }

    /// Wait for the answer to request `id`, before `until`, doing what a turn does with what the
    /// agent sends meanwhile. `None` when the turn was stopped first.
    fn answer_to(&mut self, id: i64, until: Instant, ctx: &mut TurnCtx<'_>) -> Result<Option<Result<Value, RpcError>>, String> {
        let name = self.name().to_string();
        loop {
            if ctx.is_cancelled() {
                return Ok(None);
            }
            if Instant::now() >= until {
                return Err(format!("{name} didn't answer in time"));
            }
            let Some(running) = self.running.as_mut() else { return Err(format!("{name} isn't running")) };
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
                Message::Response { id: answered, result } if answered.as_i64() == Some(id) => return Ok(Some(result.cloned())),
                Message::Notification { method: "session/update", params } => self.update(params, ctx),
                Message::Request { id: asked, method, params } => self.answer_request(asked, method, params, false, ctx)?,
                Message::Response { .. } | Message::Notification { .. } | Message::Invalid => {}
            }
        }
    }

    /// Take what the agent sent since the last turn, doing what a turn does with it: its config
    /// options may have changed, which decides what the next prompt needs set first. At most
    /// [`MAX_CATCH_UP`] messages, and not past a Stop: an agent that keeps writing would otherwise
    /// hold the turn here, where nothing else looks at either. What is left is read in the turn.
    fn catch_up(&mut self, ctx: &mut TurnCtx<'_>) -> Result<(), String> {
        for _ in 0..MAX_CATCH_UP {
            if ctx.is_cancelled() {
                return Ok(());
            }
            let Some(running) = self.running.as_mut() else { return Ok(()) };
            let message = match running.process.next(Duration::ZERO) {
                None | Some(Incoming::Closed) => return Ok(()),
                Some(Incoming::Junk(_)) => continue,
                Some(Incoming::Message(m)) => m,
            };
            match classify(&message) {
                Message::Notification { method: "session/update", params } => self.update(params, ctx),
                Message::Request { id, method, params } => self.answer_request(id, method, params, false, ctx)?,
                Message::Response { .. } | Message::Notification { .. } | Message::Invalid => {}
            }
        }
        Ok(())
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
            changes: None,
            sources: Vec::new(),
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
        // An update says only what changed: what it leaves out still stands.
        for source in call_sources(update, &call.kind) {
            if !call.sources.contains(&source) && call.sources.len() < MAX_SOURCES {
                call.sources.push(source);
            }
        }
        // What the assistant changed in the document while the call ran (through WordCraft's MCP
        // server or `wordcraft-cli call`) is the call's: its row links to it. Kinds that change
        // nothing are left out, so a change made beside them isn't put on them.
        if call.changes.is_none() && !matches!(call.kind.as_str(), "read" | "search" | "think" | "fetch") {
            match call.status {
                ToolStatus::Pending | ToolStatus::InProgress => {
                    if self.started.len() < MAX_CALLS
                        && !self.started.contains_key(&call.id)
                        && let Some(v) = ctx.document_version()
                    {
                        self.started.insert(call.id.clone(), v);
                    }
                }
                ToolStatus::Completed | ToolStatus::Failed => {
                    if let Some(v) = self.started.remove(&call.id) {
                        call.changes = ctx.changes_since(v);
                    }
                }
            }
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
            Some("config_option_update") => {
                if let Some(running) = self.running.as_mut() {
                    running.config = Config::parse(update);
                }
                self.show_config(ctx, true);
            }
            Some("usage_update") => {
                if let Some(usage) = usage_update(update) {
                    ctx.emit(Event::Usage(usage));
                }
            }
            // The person's prompt echoed, modes, commands…: nothing to show.
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

    fn begin(&mut self, seat: &Seat) -> Result<(), String> {
        // The agent itself starts with the first turn, which Stop can interrupt.
        self.seat = Some(seat.clone());
        Ok(())
    }

    fn turn(&mut self, turn: &Turn, ctx: &mut TurnCtx<'_>) -> Result<StopReason, String> {
        self.calls.clear();
        self.started.clear();
        if let Some(note) = &turn.note {
            self.note = Some(match self.spec.reach.note() {
                Some(more) => format!("{note}\n\n{more}"),
                None => note.clone(),
            });
        }
        if let Some((anchor, focus)) = turn.selection
            && let Some((_, room)) = self.spec.reach.checked()?
        {
            let seat = self.seat.as_ref().ok_or("the assistant has no seat in the shared document")?;
            let mut remote = wordcraft_mcp::Remote::connect_with_key(&room.to_string(), &seat.key).map_err(|e| e.to_string())?;
            remote.call("select.range", json!({"anchor": anchor, "focus": focus}))?;
        }
        // An agent that stopped since the last turn starts again.
        if self.running.as_mut().is_some_and(|r| !r.process.is_running()) {
            self.stop_agent(STUCK_GRACE);
        }
        let fresh = self.running.is_none();
        if fresh && !self.start_shown(ctx)? {
            return Ok(StopReason::Cancelled);
        }
        if !fresh {
            self.catch_up(ctx)?;
        }
        let changed = self.apply_choice(turn, ctx)?;
        if ctx.is_cancelled() {
            return Ok(StopReason::Cancelled);
        }
        let first = self.running.as_ref().is_some_and(|r| !r.prompted);
        self.show_config(ctx, first || changed);
        // A new session gets the note, which the agent's model reads first.
        let note = if first { self.note.as_deref() } else { None };
        let text = |t: &str| json!({"type": "text", "text": t});
        let before = [note, turn.context.as_deref()].into_iter().flatten().filter(|t| !t.trim().is_empty()).map(text);
        let Some(running) = self.running.as_mut() else { return Err(format!("{} isn't running", self.spec.label)) };
        let embedded = running.embedded_context;
        let sources = turn.knowledge.iter().enumerate().map(|(n, source)| {
            if embedded {
                let name: String = source.name.chars().map(|c| if c.is_alphanumeric() || c == '.' { c } else { '-' }).collect();
                let framed = source.as_text();
                json!({"type": "resource", "resource": {"uri": format!("wordcraft:source/{n}/{name}"), "mimeType": "text/plain", "text": framed}})
            } else {
                text(&source.as_text())
            }
        });
        let asked = (!turn.prompt.trim().is_empty()).then(|| text(&turn.prompt));
        let prompt: Vec<Value> = before.chain(sources).chain(asked).collect();
        let session = running.session.clone();
        let ended = match running.process.request("session/prompt", json!({"sessionId": session, "prompt": prompt})) {
            Ok(id) => {
                running.prompted = true;
                self.finish_turn(id, ctx)
            }
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

/// What a `usage_update` says: how much of the session's context is in use and how much it holds,
/// and what the session has cost, as the agent gave it. `None` when it says nothing usable.
fn usage_update(update: &Value) -> Option<Usage> {
    let number = |key: &str| update.get(key).and_then(Value::as_u64);
    let cost = update.get("cost").and_then(|cost| {
        let amount = cost.get("amount").filter(|a| a.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0))?;
        let currency = cost.get("currency").and_then(Value::as_str).map(label).filter(|c| !c.trim().is_empty())?;
        Some(format!("{amount} {currency}"))
    });
    let usage = Usage { context_used: number("used"), context_size: number("size"), cost, ..Usage::default() };
    (usage != Usage::default()).then_some(usage)
}

/// What a tool call says it read: its `locations`, and where a fetch went (`rawInput` holds the
/// address, by convention: no field of the protocol does), or a link in its content. Junk is left
/// out.
fn call_sources(update: &Value, kind: &str) -> Vec<Source> {
    let mut out = Vec::new();
    let locations = update.get("locations").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    for location in locations.iter().take(MAX_SOURCES) {
        let Some(path) = location.get("path").and_then(Value::as_str).filter(|p| !p.trim().is_empty()) else { continue };
        let title = match location.get("line").and_then(Value::as_u64) {
            Some(line) => format!("{path}:{line}"),
            None => path.to_string(),
        };
        out.push(Source { kind: SourceKind::File, title, target: Some(path.to_string()) });
    }
    if kind == "fetch" {
        let raw = |key: &str| update.pointer(&format!("/rawInput/{key}")).and_then(Value::as_str);
        let content = update.get("content").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
        let linked = content.iter().take(8).find_map(|c| {
            let block = c.get("content")?;
            (block.get("type").and_then(Value::as_str) == Some("resource_link")).then(|| block.get("uri").and_then(Value::as_str)).flatten()
        });
        let address = raw("url").or_else(|| raw("uri")).or(linked).filter(|a| crate::answer::opens(a));
        out.extend(address.map(|a| Source { kind: SourceKind::Web, title: a.to_string(), target: Some(a.to_string()) }));
    }
    out
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

#[cfg(unix)]
fn check_trusted_ancestors(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let uid = rustix::process::geteuid().as_raw();
    for parent in path.ancestors() {
        let metadata = std::fs::metadata(parent)?;
        if !metadata.is_dir() || metadata.mode() & 0o022 != 0 || (metadata.uid() != uid && metadata.uid() != 0) {
            return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "the agent's folder must have private, trusted ancestors"));
        }
    }
    Ok(())
}

/// A new empty folder of an agent's own, removed with everything in it at the end.
/// Unix uses a private cache ancestry so ancestor configuration cannot come from other users.
struct Folder(Option<PathBuf>);

impl Folder {
    fn new() -> std::io::Result<Folder> {
        #[cfg(unix)]
        let base = {
            let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).ok_or_else(|| std::io::Error::other("no home folder for the agent"))?;
            let cache = if cfg!(target_os = "macos") { "Library/Caches/WordCraft/agents" } else { ".cache/wordcraft/agents" };
            PathBuf::from(home).join(cache)
        };
        #[cfg(not(unix))]
        let base = std::env::temp_dir();
        Self::in_base(&base)
    }

    #[cfg(unix)]
    fn private_base(base: &Path) -> std::io::Result<PathBuf> {
        use std::os::unix::fs::DirBuilderExt;
        if !base.is_absolute() {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "the agent's cache folder must be absolute"));
        }
        let parent = base.ancestors().find(|p| p.exists()).ok_or_else(|| std::io::Error::other("no existing parent for the agent's folder"))?;
        check_trusted_ancestors(&parent.canonicalize()?)?;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(base)?;
        let base = base.canonicalize()?;
        check_trusted_ancestors(&base)?;
        Ok(base)
    }

    fn in_base(base: &Path) -> std::io::Result<Folder> {
        #[cfg(unix)]
        let base = Self::private_base(base)?;
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

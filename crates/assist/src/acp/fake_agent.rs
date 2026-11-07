//! A scripted ACP agent, for tests and for trying the assistant without a real agent: it speaks
//! the agent's side of the Agent Client Protocol on its stdin and stdout and does what its
//! [`Script`] says, turn by turn ([`run`]). A test starts it by running its own executable again
//! with the script, as JSON, in [`SCRIPT_VAR`] (see `acp::tests::fake_agent`).
//!
//! It is as careful as the rest: what it reads is bounded, and it never panics.

use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use wordcraft_mcp::server::{BoundedLine, read_bounded_line};

use super::MAX_LINE;

/// The environment variable that holds a fake agent's script, as JSON.
pub const SCRIPT_VAR: &str = "WORDCRAFT_FAKE_ACP_AGENT";

/// The id of the one session a fake agent opens.
pub const SESSION: &str = "fake-session";

/// How long the fake waits for the MCP server it calls.
const MCP_TIMEOUT: Duration = Duration::from_secs(30);

/// What a fake agent does.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Script {
    /// Its title in `agentInfo` ("Fake Agent" when empty).
    pub title: String,
    /// The protocol version it answers `initialize` with (1 when unset).
    pub protocol: Option<u64>,
    /// The ids of its ways to sign in, each called "Sign in with <id>".
    pub auth_methods: Vec<String>,
    /// Whether `session/new` needs `authenticate` first.
    pub needs_auth: bool,
    /// Whether it never answers anything.
    pub silent: bool,
    /// The turns, in order: the n-th `session/prompt` does the n-th list, and one after the
    /// last says nothing.
    pub turns: Vec<Vec<Step>>,
    /// Turns by prompt: a prompt whose last text block is one of these does its list instead.
    pub replies: BTreeMap<String, Vec<Step>>,
}

/// One thing a fake agent does in a turn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "camelCase")]
pub enum Step {
    /// Say `text` (an `agent_message_chunk`).
    Say { text: String },
    /// Think `text` (an `agent_thought_chunk`).
    Think { text: String },
    /// Start a tool call (`tool_call`), pending.
    Tool {
        id: String,
        title: String,
        #[serde(default)]
        kind: String,
    },
    /// Update a tool call (`tool_call_update`): its status, and its output's text.
    Update {
        id: String,
        status: String,
        #[serde(default)]
        text: String,
    },
    /// Its plan: one entry per string, the first in progress.
    Plan { entries: Vec<String> },
    /// Ask permission for tool call `id` (options "allow" and "reject"), then say what came
    /// back: "permission: allow", "permission: reject" or "permission: cancelled".
    Ask { id: String, title: String },
    /// Wait for `session/cancel`, then end the turn cancelled.
    Hold,
    /// Never answer again, nor read: an agent that is stuck.
    Hang,
    /// Write `stderr` to stderr and exit with `code`, in the middle of the turn.
    Crash { code: i32, stderr: String },
    /// Write what isn't JSON-RPC, or isn't what the protocol says: a JSON array, bytes that
    /// aren't UTF-8, a line longer than WordCraft reads, an answer to a request never made…
    Junk,
    /// Ask the client `method` with `params`, then say what came back: "<method>: ok" or
    /// "<method>: error <code>".
    Request {
        method: String,
        #[serde(default)]
        params: Value,
    },
    /// Say, as JSON, what it was given: its folder (`cwd`, `cwdEmpty`), what `session/new`
    /// had (`session`), its `WORDCRAFT_*` environment (`env`), the prompt's text blocks
    /// (`prompt`) and its process id (`pid`).
    Report,
    /// Make `calls` on the first MCP server `session/new` gave, started the way an agent starts
    /// it (its command, args and env), and say each result on a line of its own: "<tool>: ok
    /// <text>" or "<tool>: error <why>" (and "mcp: error <why>" when there's no server).
    Mcp { calls: Vec<Call> },
    /// Run `program` with `args` (and `input` on its standard input) as an agent's shell tool
    /// runs a command, in the agent's folder and with its environment, and say what came of it:
    /// "run: ok <stdout>", "run: exit <code> <stderr>" or "run: error <why>".
    Run {
        program: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        input: Option<String>,
    },
    /// End the turn with stop reason `reason` now.
    Stop { reason: String },
    /// Answer the prompt with error `code` and `message`.
    Fail { code: i64, message: String },
    /// Wait `ms` milliseconds (at most a minute).
    Sleep { ms: u64 },
}

/// One MCP tool call ([`Step::Mcp`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Call {
    pub tool: String,
    #[serde(default)]
    pub args: Value,
}

impl Call {
    /// Call `tool` with `args`.
    pub fn new(tool: &str, args: Value) -> Call {
        Call { tool: tool.into(), args }
    }
}

/// Be the agent `script` says, on stdin and stdout, until stdin ends: the exit code.
pub fn run(script: &Script) -> i32 {
    let (tx, rx) = channel();
    let reader = std::thread::Builder::new().name("fake agent stdin".into()).spawn(move || {
        let mut input = std::io::stdin().lock();
        loop {
            match read_bounded_line(&mut input, MAX_LINE) {
                Ok(BoundedLine::Line(line)) => {
                    if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(&line)
                        && tx.send(v).is_err()
                    {
                        return;
                    }
                }
                Ok(BoundedLine::TooLong) => {}
                Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {}
                Ok(BoundedLine::Eof) | Err(_) => return,
            }
        }
    });
    if reader.is_err() {
        return 70;
    }
    let mut agent = Agent { script, rx, authenticated: false, session: Value::Null, prompt: Vec::new(), turn: 0, next_id: 1, cancelled: false };
    agent.serve()
}

struct Agent<'a> {
    script: &'a Script,
    rx: Receiver<Value>,
    authenticated: bool,
    /// What `session/new` had.
    session: Value,
    /// The current prompt's text blocks.
    prompt: Vec<String>,
    turn: usize,
    next_id: i64,
    /// Whether the client sent `session/cancel` in this turn.
    cancelled: bool,
}

impl Agent<'_> {
    fn serve(&mut self) -> i32 {
        while let Ok(m) = self.rx.recv() {
            if self.script.silent {
                continue;
            }
            if let Some(code) = self.handle(&m) {
                return code;
            }
        }
        0
    }

    /// Do what message `m` asks: an exit code when the agent exits.
    fn handle(&mut self, m: &Value) -> Option<i32> {
        let method = m.get("method").and_then(Value::as_str).unwrap_or_default();
        let params = m.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = m.get("id").filter(|_| !method.is_empty()) else {
            if method == "session/cancel" {
                self.cancelled = true;
            }
            return None;
        };
        match method {
            "initialize" => {
                let title = if self.script.title.is_empty() { "Fake Agent" } else { self.script.title.as_str() };
                let methods: Vec<Value> = self
                    .script
                    .auth_methods
                    .iter()
                    .map(|m| json!({"id": m, "name": format!("Sign in with {m}"), "description": "A sign-in that always works"}))
                    .collect();
                send(&json!({"jsonrpc": "2.0", "id": id, "result": {
                    "protocolVersion": self.script.protocol.unwrap_or(1),
                    "agentCapabilities": {"loadSession": false, "promptCapabilities": {"image": false, "audio": false, "embeddedContext": false}},
                    "authMethods": methods,
                    "agentInfo": {"name": "fake-agent", "title": title, "version": "1.0"},
                }}));
            }
            "authenticate" => {
                let way = params.get("methodId").and_then(Value::as_str).unwrap_or_default();
                if self.script.auth_methods.iter().any(|m| m == way) {
                    self.authenticated = true;
                    reply(id, json!({}));
                } else {
                    fail(id, -32602, "no such way to sign in");
                }
            }
            "session/new" if self.script.needs_auth && !self.authenticated => fail(id, -32000, "Authentication required"),
            "session/new" => {
                self.session = params;
                reply(id, json!({"sessionId": SESSION}));
            }
            "session/prompt" => return self.prompt(id, &params),
            _ => fail(id, -32601, "Method not found"),
        }
        None
    }

    /// Something that came while a turn runs: a cancel, or (refused) a request.
    fn aside(&mut self, m: &Value) {
        let method = m.get("method").and_then(Value::as_str).unwrap_or_default();
        match m.get("id") {
            Some(id) if !method.is_empty() => fail(id, -32600, "a turn is running"),
            None if method == "session/cancel" => self.cancelled = true,
            _ => {}
        }
    }

    /// Take what came meanwhile.
    fn drain(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            self.aside(&m);
        }
    }

    /// Ask the client `method`: its answer, or the error's code; `None` once stdin ended.
    fn request(&mut self, method: &str, params: Value) -> Option<Result<Value, i64>> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let m = self.rx.recv().ok()?;
            if m.get("method").is_none() && m.get("id").and_then(Value::as_i64) == Some(id) {
                return Some(match m.get("error") {
                    Some(e) => Err(e.get("code").and_then(Value::as_i64).unwrap_or(0)),
                    None => Ok(m.get("result").cloned().unwrap_or(Value::Null)),
                });
            }
            self.aside(&m);
        }
    }

    /// Run a turn, then answer prompt `id`: an exit code when the agent exits.
    fn prompt(&mut self, id: &Value, params: &Value) -> Option<i32> {
        self.cancelled = false;
        let blocks = params.get("prompt").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
        self.prompt = blocks.iter().filter_map(|b| b.get("text").and_then(Value::as_str)).map(str::to_string).collect();
        let last = self.prompt.last().map(|t| t.trim().to_string()).unwrap_or_default();
        let steps = self.script.replies.get(&last).or_else(|| self.script.turns.get(self.turn)).cloned().unwrap_or_default();
        self.turn = self.turn.saturating_add(1);
        for step in steps {
            self.drain();
            if self.cancelled {
                break;
            }
            match step {
                Step::Say { text } => say(&text),
                Step::Think { text } => update(json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": text}})),
                Step::Tool { id: call, title, kind } => {
                    let kind = if kind.is_empty() { "other".to_string() } else { kind };
                    update(json!({"sessionUpdate": "tool_call", "toolCallId": call, "title": title, "kind": kind, "status": "pending"}));
                }
                Step::Update { id: call, status, text } => update(json!({
                    "sessionUpdate": "tool_call_update", "toolCallId": call, "status": status,
                    "content": [{"type": "content", "content": {"type": "text", "text": text}}],
                })),
                Step::Plan { entries } => {
                    let entries: Vec<Value> = entries
                        .iter()
                        .enumerate()
                        .map(|(i, e)| {
                            let status = if i == 0 { "in_progress" } else { "pending" };
                            json!({"content": e, "priority": "medium", "status": status})
                        })
                        .collect();
                    update(json!({"sessionUpdate": "plan", "entries": entries}));
                }
                Step::Ask { id: call, title } => {
                    let options = json!([
                        {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                        {"optionId": "reject", "name": "Reject", "kind": "reject_once"},
                    ]);
                    let asked = json!({"sessionId": SESSION, "toolCall": {"toolCallId": call, "title": title}, "options": options});
                    let said = match self.request("session/request_permission", asked)? {
                        Ok(r) => {
                            let outcome = r.get("outcome");
                            match outcome.and_then(|o| o.get("outcome")).and_then(Value::as_str) {
                                Some("selected") => outcome.and_then(|o| o.get("optionId")).and_then(Value::as_str).unwrap_or("?").to_string(),
                                Some("cancelled") => "cancelled".into(),
                                _ => format!("unknown answer {r}"),
                            }
                        }
                        Err(code) => format!("error {code}"),
                    };
                    say(&format!("permission: {said}"));
                }
                Step::Hold => {
                    while !self.cancelled {
                        let m = self.rx.recv().ok()?;
                        self.aside(&m);
                    }
                }
                Step::Hang => loop {
                    std::thread::sleep(Duration::from_secs(3600));
                },
                Step::Crash { code, stderr } => {
                    let mut err = std::io::stderr().lock();
                    let _ = err.write_all(stderr.as_bytes()).and_then(|()| err.write_all(b"\n")).and_then(|()| err.flush());
                    return Some(code);
                }
                Step::Junk => junk(),
                Step::Request { method, params } => {
                    let said = match self.request(&method, params)? {
                        Ok(_) => "ok".to_string(),
                        Err(code) => format!("error {code}"),
                    };
                    say(&format!("{method}: {said}"));
                }
                Step::Report => say(&self.report().to_string()),
                Step::Mcp { calls } => match self.mcp(&calls) {
                    Ok(said) => say(&said),
                    Err(why) => say(&format!("mcp: error {why}\n")),
                },
                Step::Run { program, args, input } => say(&format!("{}\n", run_program(&program, &args, input.as_deref()))),
                Step::Stop { reason } => {
                    reply(id, json!({"stopReason": reason}));
                    return None;
                }
                Step::Fail { code, message } => {
                    fail(id, code, &message);
                    return None;
                }
                Step::Sleep { ms } => std::thread::sleep(Duration::from_millis(ms.min(60_000))),
            }
        }
        self.drain();
        let reason = if self.cancelled { "cancelled" } else { "end_turn" };
        reply(id, json!({"stopReason": reason}));
        None
    }

    /// What it was given.
    fn report(&self) -> Value {
        let cwd = std::env::current_dir().ok();
        let empty = cwd.as_ref().and_then(|d| std::fs::read_dir(d).ok()).map(|mut entries| entries.next().is_none());
        let env: Map<String, Value> = std::env::vars_os()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
            .filter(|(k, _)| k.to_ascii_uppercase().starts_with("WORDCRAFT_") && k != SCRIPT_VAR)
            .map(|(k, v)| (k, Value::String(v)))
            .collect();
        json!({
            "cwd": cwd.map(|d| d.to_string_lossy().into_owned()),
            "cwdEmpty": empty,
            "session": self.session,
            "env": env,
            "prompt": self.prompt,
            "pid": std::process::id(),
        })
    }

    /// Make `calls` on the first MCP server of the session, as an agent does: what each gave.
    fn mcp(&self, calls: &[Call]) -> Result<String, String> {
        let server = self.session.get("mcpServers").and_then(Value::as_array).and_then(|s| s.first()).ok_or("the session has no MCP server")?;
        let command = server.get("command").and_then(Value::as_str).ok_or("the MCP server has no command")?;
        let argv: Vec<&str> = server.get("args").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        let mut cmd = Command::new(command);
        cmd.args(argv).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        for e in server.get("env").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default() {
            if let (Some(name), Some(value)) = (e.get("name").and_then(Value::as_str), e.get("value").and_then(Value::as_str)) {
                cmd.env(name, value);
            }
        }
        let mut child = cmd.spawn().map_err(|e| format!("the MCP server didn't start: {e}"))?;
        let result = talk_mcp(&mut child, calls);
        drop(child.stdin.take());
        let until = Instant::now() + Duration::from_secs(5);
        while matches!(child.try_wait(), Ok(None)) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = child.kill();
        let _ = child.wait();
        result
    }
}

/// Most bytes of a program's output [`Step::Run`] reads, from each of its stdout and stderr.
const MAX_OUTPUT: u64 = 1 << 20;

/// How long a program [`Step::Run`] runs may take.
const RUN_TIMEOUT: Duration = Duration::from_secs(60);

/// Run `program` with `args`, `input` on its stdin: what came of it, for [`Step::Run`].
fn run_program(program: &str, args: &[String], input: Option<&str>) -> String {
    use std::io::Read;
    let child = Command::new(program).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return format!("run: error {e}"),
    };
    if let Some(mut stdin) = child.stdin.take() {
        // On a thread of its own, so a program that doesn't read its input can't hold this one
        // up; it ends the input when it's written.
        let input = input.unwrap_or_default().to_string();
        std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
    }
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(pipe) = pipe {
                let _ = pipe.take(MAX_OUTPUT).read_to_end(&mut bytes);
            }
            String::from_utf8_lossy(&bytes).trim().to_string()
        })
    };
    let out = read(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = read(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let until = Instant::now() + RUN_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err("it didn't end in time".to_string());
            }
            Err(e) => break Err(e.to_string()),
        }
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    match status {
        Ok(s) if s.success() => format!("run: ok {out}"),
        Ok(s) => format!("run: exit {} {err}", s.code().map_or_else(|| "by signal".to_string(), |c| c.to_string())),
        Err(why) => format!("run: error {why}"),
    }
}

/// MCP over `child`'s stdin and stdout: initialize, then make `calls`, saying what each gave.
fn talk_mcp(child: &mut Child, calls: &[Call]) -> Result<String, String> {
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else { return Err("no pipes to the MCP server".into()) };
    let (tx, lines) = channel::<Value>();
    std::thread::Builder::new()
        .name("fake agent mcp".into())
        .spawn(move || {
            let mut out = std::io::BufReader::new(stdout);
            while let Ok(BoundedLine::Line(line)) = read_bounded_line(&mut out, MAX_LINE) {
                if let Ok(v) = serde_json::from_str::<Value>(&line)
                    && tx.send(v).is_err()
                {
                    return;
                }
            }
        })
        .map_err(|e| e.to_string())?;
    let answer = |stdin: &mut ChildStdin, id: i64, method: &str, params: Value| -> Result<Value, String> {
        write_line(stdin, &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let until = Instant::now() + MCP_TIMEOUT;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            let m = match lines.recv_timeout(left) {
                Ok(m) => m,
                Err(RecvTimeoutError::Timeout) => return Err(format!("the MCP server didn't answer {method}")),
                Err(RecvTimeoutError::Disconnected) => return Err(format!("the MCP server stopped before it answered {method}")),
            };
            if m.get("id").and_then(Value::as_i64) == Some(id) {
                return match m.get("error") {
                    Some(e) => Err(e.get("message").and_then(Value::as_str).unwrap_or("an error").to_string()),
                    None => Ok(m.get("result").cloned().unwrap_or(Value::Null)),
                };
            }
        }
    };
    let init = json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "fake-agent", "version": "1.0"}});
    answer(&mut stdin, 1, "initialize", init)?;
    write_line(&mut stdin, &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
    let mut said = String::new();
    for (id, call) in (2_i64..).zip(calls) {
        let (outcome, text) = match answer(&mut stdin, id, "tools/call", json!({"name": call.tool, "arguments": call.args})) {
            Ok(result) => {
                let content = result.get("content").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
                let text: Vec<&str> = content.iter().filter_map(|c| c.get("text").and_then(Value::as_str)).collect();
                let failed = result.get("isError").and_then(Value::as_bool).unwrap_or(false);
                (if failed { "error" } else { "ok" }, text.join(" "))
            }
            Err(why) => ("error", why),
        };
        let text: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).take(2000).collect();
        said.push_str(&format!("{}: {outcome} {text}\n", call.tool));
    }
    Ok(said)
}

fn write_line(to: &mut impl Write, message: &Value) -> Result<(), String> {
    to.write_all(format!("{message}\n").as_bytes()).and_then(|()| to.flush()).map_err(|e| e.to_string())
}

/// Write `message` to stdout, a line.
fn send(message: &Value) {
    raw(format!("{message}\n").as_bytes());
}

fn raw(bytes: &[u8]) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(bytes).and_then(|()| out.flush());
}

fn reply(id: &Value, result: Value) {
    send(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
}

fn fail(id: &Value, code: i64, message: &str) {
    send(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}));
}

fn update(update: Value) {
    send(&json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": SESSION, "update": update}}));
}

fn say(text: &str) {
    update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}));
}

/// Lines a client must survive.
fn junk() {
    raw(b"[1,2,3]\n\"a string\"\nnot JSON at all\n\xff\xfe\xfd not UTF-8\n{}\n{\"id\": \"x\"}\n");
    let mut long = vec![b'x'; MAX_LINE.saturating_add(16)];
    long.push(b'\n');
    raw(&long);
    let mut deep = vec![b'['; 100_000];
    deep.push(b'\n');
    raw(&deep);
    let title = "t".repeat(100_000);
    for m in [
        json!({"jsonrpc": "2.0", "id": 987_654, "result": {"stopReason": "refusal"}}),
        json!({"jsonrpc": "2.0", "id": 987_655, "error": "not an object"}),
        json!({"jsonrpc": "2.0", "method": "session/update"}),
        json!({"jsonrpc": "2.0", "method": "session/update", "params": "nope"}),
        json!({"jsonrpc": "2.0", "method": "session/update", "params": {"update": {"sessionUpdate": "tool_call"}}}),
        json!({"jsonrpc": "2.0", "method": "session/update", "params": {"update": {"sessionUpdate": "tool_call", "toolCallId": "junk", "title": title, "status": 7}}}),
        json!({"jsonrpc": "2.0", "method": "session/update", "params": {"update": {"sessionUpdate": "plan", "entries": "nope"}}}),
        json!({"jsonrpc": "2.0", "method": "session/update", "params": {"update": {"sessionUpdate": "plan", "entries": [{"content": 5}, {}]}}}),
        json!({"jsonrpc": "2.0", "method": "session/update", "params": {"update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": 42}}}}),
        json!({"jsonrpc": "2.0", "method": "session/update", "params": {"update": {"sessionUpdate": "no_such_update"}}}),
        json!({"jsonrpc": "2.0", "method": "session/request_permission", "id": null, "params": {"options": "nope"}}),
        json!({"jsonrpc": "2.0", "method": 5, "id": 3}),
    ] {
        send(&m);
    }
}

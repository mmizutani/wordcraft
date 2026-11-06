//! #22, end to end: an ACP agent (the scripted fake: this test executable, run again) gets
//! WordCraft's MCP server (this crate's `wordcraft-cli mcp --connect`) in `session/new`, with
//! the conversation's invite key, and edits the shared document through it as the
//! conversation's participant, reaching it through the room's own port.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wordcraft_assist::acp::fake_agent::{Call, SCRIPT_VAR, Script, Step};
use wordcraft_assist::acp::{Acp, AgentSpec, Launch, Reach, RoomPort};
use wordcraft_assist::{Assistant, Options};
use wordcraft_doc::{Document, StoryRef};
use wordcraft_engine::Session;
use wordcraft_engine::room::{Lobby, Replica, Role, Room};

const WAIT: Duration = Duration::from_secs(120);

/// The fake agent: when this executable runs again with a script in [`SCRIPT_VAR`], this test
/// is the agent, and the process ends with it.
#[test]
fn fake_agent() {
    let Ok(script) = std::env::var(SCRIPT_VAR) else { return };
    let script: Script = serde_json::from_str(&script).unwrap();
    std::process::exit(wordcraft_assist::acp::fake_agent::run(&script));
}

/// The window: its session, showing "one two three", shared under the name Minoru through a
/// lobby with the room's own port.
struct Window {
    session: Session,
    room: Arc<Room>,
    own: Replica,
    lobby: Lobby,
    port: RoomPort,
}

impl Window {
    fn new() -> Window {
        let mut session = Session::new(Document::from_text("one two three"));
        session.author = "Minoru".into();
        let (room, own) = Room::host(&mut session, "Minoru").unwrap();
        let lobby = Lobby::default();
        lobby.open(room.clone());
        let port = RoomPort::open(&lobby).unwrap();
        Window { session, room, own, lobby, port }
    }

    /// Frames until the window has every change the room took.
    fn settle(&mut self) {
        let until = Instant::now() + WAIT;
        loop {
            let _ = self.own.lend(&mut self.session, Replica::sync);
            if !self.own.has_unconfirmed() && self.own.received() == self.room.version() {
                return;
            }
            assert!(Instant::now() < until, "the window doesn't catch up");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// The text with every tracked change accepted.
    fn accepted(&self) -> String {
        let mut s = Session::new(self.session.doc.clone());
        s.run("review.acceptAll", &json!({})).unwrap();
        s.doc.plain_text(StoryRef::Body)
    }

    fn authors(&self) -> Vec<String> {
        self.session.doc.revisions.iter().map(|r| r.author.clone()).collect()
    }

    /// The fake agent running `script`, reaching this window's room through WordCraft's MCP
    /// server.
    fn agent(&self, script: &Script) -> AgentSpec {
        let exe = std::env::current_exe().unwrap();
        let mut spec = AgentSpec::new("Fake Agent", Launch::new(exe, ["--exact", "acp::fake_agent", "--nocapture", "--test-threads", "1", "-q"]));
        spec.env.push((SCRIPT_VAR.into(), serde_json::to_string(script).unwrap()));
        spec.reach = Reach::Mcp { cli: PathBuf::from(env!("CARGO_BIN_EXE_wordcraft-cli")), room: self.port.addr() };
        spec.start_timeout = WAIT;
        spec
    }

    fn start(&self, script: &Script, setting: Option<Role>) -> Assistant {
        let options = Options { document: "Notes".into(), setting, ..Options::default() };
        Assistant::start(&self.room, Box::new(Acp::new(self.agent(script))), options).unwrap()
    }
}

/// Ask `prompt` and wait for the turn to end: the turn's answer.
fn ask(a: &mut Assistant, prompt: &str) -> String {
    a.ask(prompt, None).unwrap();
    assert!(a.wait_idle(WAIT), "the turn didn't end");
    a.transcript().last_answer().unwrap_or_default()
}

/// The agent's turn: replace "two" with "2" through WordCraft's MCP server.
fn rewrite_two() -> Vec<Step> {
    vec![Step::Mcp { calls: vec![Call::new("select_text", json!({"text": "two"})), Call::new("type_text", json!({"text": "2"}))] }]
}

/// One request on a new connection to the room's port, with `key`: the reply.
fn request(port: &RoomPort, key: &str, method: &str) -> Value {
    let mut s = TcpStream::connect(port.addr()).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    writeln!(s, "{}", json!({"id": 1, "key": key, "method": method})).unwrap();
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
fn an_agents_edit_through_wordcrafts_mcp_server_is_a_tracked_change_in_its_name() {
    let mut w = Window::new();
    let script = Script { turns: vec![rewrite_two(), vec![Step::Report]], ..Script::default() };
    let mut a = w.start(&script, None);
    assert_eq!(a.role(), Role::Suggester);
    let answer = ask(&mut a, "Write two as a digit");
    assert!(answer.contains("select_text: ok") && answer.contains("type_text: ok"), "{answer}");
    w.settle();
    assert_eq!(w.accepted(), "one 2 three");
    let authors = w.authors();
    assert!(!authors.is_empty() && authors.iter().all(|a| a == "Fake Agent"), "{authors:?}");

    // What the agent was given: WordCraft's MCP server with the invite's key, and nothing else
    // of WordCraft's.
    let report: Value = serde_json::from_str(&ask(&mut a, "Report")).unwrap();
    let servers = &report["session"]["mcpServers"];
    assert_eq!(
        servers,
        &json!([{
            "name": "wordcraft",
            "command": env!("CARGO_BIN_EXE_wordcraft-cli"),
            "args": ["mcp", "--connect", w.port.addr().to_string()],
            "env": [{"name": "WORDCRAFT_CONTROL_KEY", "value": a.key()}],
        }])
    );
    assert_eq!(report["env"], json!({}));
    assert_eq!(report["cwdEmpty"], json!(true));
    a.end();
}

#[test]
fn with_the_setting_at_editor_the_agents_edit_is_untracked() {
    let mut w = Window::new();
    let mut a = w.start(&Script { turns: vec![rewrite_two()], ..Script::default() }, Some(Role::Editor));
    assert_eq!(a.role(), Role::Editor);
    let answer = ask(&mut a, "Write two as a digit");
    assert!(answer.contains("type_text: ok"), "{answer}");
    w.settle();
    assert_eq!(w.session.doc.final_text(StoryRef::Body), "one 2 three");
    assert!(w.authors().is_empty(), "{:?}", w.authors());
    a.end();
}

#[test]
fn the_agent_reaches_no_file_and_no_other_document() {
    let mut w = Window::new();
    let elsewhere = std::env::temp_dir().join(format!("wordcraft-acp-e2e-{}.docx", std::process::id()));
    let calls = vec![
        Call::new("execute", json!({"command": "file.saveAs", "params": {"path": elsewhere}})),
        Call::new("execute", json!({"command": "file.open", "params": {"path": "/etc/hosts"}})),
        Call::new("execute", json!({"command": "file.new"})),
        Call::new("execute", json!({"command": "text.insert", "params": {"text": "x"}, "doc": "Another document"})),
        Call::new("execute", json!({"command": "mailings.recipients", "params": {"path": "/etc/passwd"}})),
        Call::new("get_text", json!({})),
    ];
    let mut a = w.start(&Script { turns: vec![vec![Step::Mcp { calls }]], ..Script::default() }, None);
    let answer = ask(&mut a, "Look around");
    let lines: Vec<&str> = answer.lines().collect();
    assert_eq!(lines.len(), 6, "{answer}");
    for line in &lines[..3] {
        assert!(line.starts_with("execute: error") && line.contains("aren't part of a shared document"), "{line}");
    }
    assert!(lines[3].starts_with("execute: error") && lines[3].contains("the shared document only"), "{}", lines[3]);
    assert!(lines[4].starts_with("execute: error") && lines[4].contains("filesystem access is not granted"), "{}", lines[4]);
    assert!(lines[5].starts_with("get_text: ok") && lines[5].contains("one two three"), "{}", lines[5]);
    assert!(!elsewhere.exists());
    w.settle();
    assert_eq!(w.session.doc.final_text(StoryRef::Body), "one two three");
    a.end();
}

#[test]
fn ending_the_conversation_takes_the_agents_key_back() {
    let mut w = Window::new();
    let script = Script { turns: vec![vec![Step::Mcp { calls: vec![Call::new("get_text", json!({}))] }]], ..Script::default() };
    let mut a = w.start(&script, None);
    assert!(ask(&mut a, "Read").starts_with("get_text: ok"));
    let key = a.key().to_string();
    assert_eq!(request(&w.port, &key, "document.inspect")["ok"], json!(true));
    a.end();
    assert_eq!(request(&w.port, &key, "document.inspect")["error"], json!("unauthorized"));
    assert!(w.lobby.join_with(&key).is_err());
    w.settle();
}

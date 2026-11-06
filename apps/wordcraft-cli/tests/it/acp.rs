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
use wordcraft_assist::acp::presets::{AgentPrefs, AgentSettings};
use wordcraft_assist::acp::{Acp, AgentSpec, Launch, Reach, RoomPort};
use wordcraft_assist::{Answer, Assistant, Host, Options, PermissionPolicy, Readiness};
use wordcraft_doc::{Document, StoryRef};
use wordcraft_engine::Session;
use wordcraft_engine::room::{Lobby, Replica, Role, Room};

const WAIT: Duration = Duration::from_secs(120);

/// How this executable runs as the fake agent.
const AS_AGENT: [&str; 6] = ["--exact", "acp::fake_agent", "--nocapture", "--test-threads", "1", "-q"];

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
        let mut spec = AgentSpec::new("Fake Agent", Launch::new(exe, AS_AGENT));
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
fn each_turn_selects_the_persons_range_in_the_agents_mcp_copy() {
    let mut w = Window::new();
    let type_text = |text: &str| vec![Step::Mcp { calls: vec![Call::new("type_text", json!({"text": text}))] }];
    let script = Script { turns: vec![type_text("2"), type_text("3")], ..Script::default() };
    let mut a = w.start(&script, None);
    for (selected, expected) in [("two", "one 2 three"), ("three", "one 2 3")] {
        w.own.lend(&mut w.session, |c| c.run("select.text", &json!({"text": selected}))).unwrap().unwrap();
        let anchor = w.session.doc.anchor_at(&w.session.sel.anchor).unwrap();
        let focus = w.session.doc.anchor_at(&w.session.sel.focus).unwrap();
        a.ask_at("Rewrite the selected text", None, Some((anchor, focus))).unwrap();
        assert!(a.wait_idle(WAIT), "the turn didn't end");
        let answer = a.transcript().last_answer().unwrap_or_default();
        assert!(answer.contains("type_text: ok"), "{answer}");
        w.settle();
        assert_eq!(w.accepted(), expected);
    }
    assert!(w.authors().iter().all(|author| author == "Fake Agent"));
    a.end();
}

#[test]
fn the_selected_range_follows_edits_while_the_agent_waits_for_sign_in() {
    let mut w = Window::new();
    let script = Script {
        needs_auth: true,
        auth_methods: vec!["browser".into()],
        turns: vec![vec![Step::Mcp { calls: vec![Call::new("type_text", json!({"text": "3"}))] }]],
        ..Script::default()
    };
    w.own.lend(&mut w.session, |copy| copy.run("select.text", &json!({"text": "three"}))).unwrap().unwrap();
    let anchor = w.session.doc.anchor_at(&w.session.sel.anchor).unwrap();
    let focus = w.session.doc.anchor_at(&w.session.sel.focus).unwrap();
    let options = Options { permissions: PermissionPolicy::Ask, ..Options::default() };
    let mut a = Assistant::start(&w.room, Box::new(Acp::new(w.agent(&script))), options).unwrap();
    a.ask_at("Rewrite the selected text", None, Some((anchor, focus))).unwrap();
    let until = Instant::now() + WAIT;
    loop {
        a.poll();
        if a.pending_permission().is_some() {
            break;
        }
        assert!(Instant::now() < until, "the sign-in question never arrived");
        std::thread::sleep(Duration::from_millis(2));
    }
    w.own
        .lend(&mut w.session, |copy| {
            copy.run("caret.docStart", &json!({}))?;
            copy.run("text.insert", &json!({"text": "Oh, "}))
        })
        .unwrap()
        .unwrap();
    w.settle();
    assert_eq!(w.accepted(), "Oh, one two three");
    assert_eq!(request(&w.port, a.key(), "document.inspect")["ok"], true, "the agent's copy has received the edit before sign-in finishes");
    let id = a.pending_permission().unwrap().id;
    assert!(a.answer(id, Answer::Selected { option: "browser".into() }));
    assert!(a.wait_idle(WAIT), "the turn did not end");
    assert!(a.transcript().last_answer().unwrap_or_default().contains("type_text: ok"));
    w.settle();
    assert_eq!(w.accepted(), "Oh, one two 3");
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

/// #24: the fake agent set up as the custom agent in the agents' settings, offered and made by
/// its id as the pane does, edits the document end to end.
#[test]
fn the_fake_agent_runs_end_to_end_as_the_custom_agent() {
    let mut w = Window::new();
    let dir = std::env::temp_dir().join(format!("wordcraft-acp-custom-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let script = Script { turns: vec![rewrite_two()], ..Script::default() };
    let mut settings = AgentSettings::default();
    settings.agents.insert(
        "custom".into(),
        AgentPrefs {
            name: "Fake Agent".into(),
            command: std::env::current_exe().unwrap().to_string_lossy().into_owned(),
            args: AS_AGENT.map(String::from).to_vec(),
            env: [(SCRIPT_VAR.to_string(), serde_json::to_string(&script).unwrap())].into(),
        },
    );
    settings.save(&dir).unwrap();
    let host =
        Host { settings: Some(dir.clone()), room: Some(w.port.addr()), cli: Some(PathBuf::from(env!("CARGO_BIN_EXE_wordcraft-cli"))), fake: false };
    let offer = wordcraft_assist::offers::offers(&host).into_iter().find(|o| o.id == "acp:custom").unwrap();
    assert_eq!((offer.label.as_str(), &offer.readiness), ("Fake Agent", &Readiness::Ready));
    let backend = wordcraft_assist::offers::make("acp:custom", &host).unwrap();
    assert_eq!(backend.info().author, "Fake Agent");
    let mut a = Assistant::start(&w.room, backend, Options { document: "Notes".into(), ..Options::default() }).unwrap();
    let answer = ask(&mut a, "Write two as a digit");
    assert!(answer.contains("type_text: ok"), "{answer}");
    w.settle();
    assert_eq!(w.accepted(), "one 2 three");
    assert!(w.authors().iter().all(|a| a == "Fake Agent") && !w.authors().is_empty(), "{:?}", w.authors());
    a.end();
    let _ = std::fs::remove_dir_all(&dir);
}

/// #25: Pi's adapter passes no MCP server on, so Pi reaches the document with its own shell
/// and `wordcraft-cli call`, with its invite's key and the room's address in its environment.
/// The fake agent, set up as Pi's command in the agents' settings, does just that.
#[test]
fn pi_edits_through_wordcraft_cli_call_in_its_shell_with_its_invites_key() {
    let mut w = Window::new();
    let dir = std::env::temp_dir().join(format!("wordcraft-acp-pi-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cli = env!("CARGO_BIN_EXE_wordcraft-cli").to_string();
    let run = |args: &[&str], input: Option<&str>| Step::Run {
        program: cli.clone(),
        args: args.iter().map(|a| a.to_string()).collect(),
        input: input.map(str::to_string),
    };
    let edit = vec![
        run(&["call", "select.text", r#"{"text": "two"}"#], None),
        run(&["call", "text.insert", "-"], Some(r#"{"text": "2", "raw": true}"#)),
        run(&["call", "file.saveAs", r#"{"path": "/tmp/stolen.docx"}"#], None),
    ];
    let script = Script { turns: vec![vec![Step::Report], edit], ..Script::default() };
    let mut settings = AgentSettings::default();
    let pi = AgentPrefs {
        command: std::env::current_exe().unwrap().to_string_lossy().into_owned(),
        args: AS_AGENT.map(String::from).to_vec(),
        env: [(SCRIPT_VAR.to_string(), serde_json::to_string(&script).unwrap()), ("WORDCRAFT_CONTROL_KEY".to_string(), "not-from-here".to_string())]
            .into(),
        ..AgentPrefs::default()
    };
    settings.agents.insert("pi".into(), pi);
    settings.save(&dir).unwrap();
    let host = Host { settings: Some(dir.clone()), room: Some(w.port.addr()), cli: Some(PathBuf::from(&cli)), fake: false };
    let backend = wordcraft_assist::offers::make("acp:pi", &host).unwrap();
    assert_eq!(backend.info().author, "Pi");
    let mut a = Assistant::start(&w.room, backend, Options { document: "Notes".into(), ..Options::default() }).unwrap();

    // Its environment has the invite's key and the room's address, and the note says how to use
    // them.
    let report: Value = serde_json::from_str(&ask(&mut a, "Report")).unwrap();
    assert_eq!(report["env"], json!({"WORDCRAFT_CONTROL_KEY": a.key(), "WORDCRAFT_CONTROL_ADDR": w.port.addr().to_string()}));
    let note = report["prompt"][0].as_str().unwrap();
    assert!(note.contains(cli.as_str()) && note.contains(" call document.inspect"), "{note}");

    let answer = ask(&mut a, "Write two as a digit");
    // Each run's line; a reply printed as JSON goes on over lines of its own.
    let lines: Vec<&str> = answer.lines().filter(|l| l.starts_with("run: ")).collect();
    assert_eq!(lines.len(), 3, "{answer}");
    assert!(lines[0].starts_with("run: ok") && lines[1].starts_with("run: ok"), "{answer}");
    assert!(lines[2].starts_with("run: exit 1") && lines[2].contains("aren't part of a shared document"), "{}", lines[2]);
    w.settle();
    assert_eq!(w.accepted(), "one 2 three");
    assert!(w.authors().iter().all(|a| a == "Pi") && !w.authors().is_empty(), "{:?}", w.authors());
    let key = a.key().to_string();
    a.end();
    assert_eq!(request(&w.port, &key, "document.inspect")["error"], json!("unauthorized"));
    let _ = std::fs::remove_dir_all(&dir);
}

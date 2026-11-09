//! #27, the gate of the built-in assistant (#15): a person, ChatGPT and an ACP agent edit one
//! document at once, with fakes only (no network, no real agent).
//!
//! - The person is the window: its session, the room's owner copy, lent to that copy each frame
//!   as the app does (`Replica::lend`), with a key of their typing every few frames at the caret.
//!   Each frame is timed ([`Window::frame`]): none may wait for an assistant.
//! - ChatGPT is the ChatGPT backend, signed in at a fake of ChatGPT's sign-in and Responses API
//!   on this machine ([`OpenAi`]), whose scripted answers call WordCraft's tools.
//! - The agent is the ACP backend running the scripted fake agent (this executable run again,
//!   `acp::fake_agent`), which edits through WordCraft's MCP server (this crate's
//!   `wordcraft-cli mcp --connect`, at the room's own port), as in `acp.rs`.

use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use wordcraft_assist::acp::fake_agent::{Call, SCRIPT_VAR, Script, Step};
use wordcraft_assist::acp::{Acp, AgentSpec, Launch, Reach, RoomPort};
use wordcraft_assist::chatgpt::{ChatGpt, Endpoints, SCOPES, SignIn, SignInOptions};
use wordcraft_assist::{Assistant, Entry, Options, StopReason, ToolStatus};
use wordcraft_doc::{Document, StoryRef};
use wordcraft_engine::Session;
use wordcraft_engine::cmd::review::changes_by;
use wordcraft_engine::room::{Lobby, Replica, Role, Room};

use crate::acp::AS_AGENT;

const WAIT: Duration = Duration::from_secs(120);

/// The longest a frame of the window may take. An assistant holds its turn for much longer
/// ([`HELD`]) in these tests: a frame that waited for one would take at least that.
const FRAME_BUDGET: Duration = Duration::from_secs(1);

/// How long ChatGPT's answer and the agent's turn hold, at the least, while the window goes on.
const HELD: Duration = Duration::from_secs(3);

/// The time between two frames.
const FRAME_GAP: Duration = Duration::from_millis(4);

/// Frames between two keys of the person's typing.
const KEY_EVERY: usize = 6;

/// The document, written by Minoru.
const TEXT: &str = "The studio opens at nine.\nIts printers are old but they work.\nVisitors are welcome.";
/// The paragraph ChatGPT rewrites, and what it makes of it.
const OLD: &str = "Its printers are old but they work.";
const NEW: &str = "Its printers are old, yet they print well.";
/// What the person types at the end of the first paragraph meanwhile.
const TYPED: &str = " Come early, and bring your own paper if you can.";
/// The section the agent adds at the end.
const HEADING: &str = "Opening hours";
const HOURS: &str = "Monday to Saturday, nine to six.";

/// The person's window: its session, showing [`TEXT`], shared under the name Minoru through a
/// lobby with the room's own port, and what the person is typing.
struct Window {
    session: Session,
    room: Arc<Room>,
    own: Replica,
    _lobby: Lobby,
    port: RoomPort,
    /// What the person still has to type, a key every [`KEY_EVERY`] frames.
    typing: VecDeque<char>,
    frames: usize,
    /// The frames the person typed a key in, and the frames an assistant's change came in.
    keys_at: Vec<usize>,
    theirs_at: Vec<usize>,
}

impl Window {
    fn new() -> Window {
        let mut session = Session::new(Document::from_text(TEXT));
        session.author = "Minoru".into();
        let (room, own) = Room::host(&mut session, "Minoru").unwrap();
        let lobby = Lobby::default();
        lobby.open(room.clone());
        let port = RoomPort::open(&lobby).unwrap();
        let mut w =
            Window { session, room, own, _lobby: lobby, port, typing: VecDeque::new(), frames: 0, keys_at: Vec::new(), theirs_at: Vec::new() };
        // The caret at the end of the first paragraph.
        w.run("select.text", json!({"text": "The studio opens at nine."}));
        w.run("caret.right", json!({}));
        w
    }

    /// Run a command of the person's, through the room, as the window does.
    fn run(&mut self, id: &str, params: Value) -> Value {
        self.own
            .lend(&mut self.session, |c| {
                let r = c.run(id, &params);
                c.send();
                r
            })
            .unwrap()
            .unwrap_or_else(|e| panic!("{id}: {e}"))
    }

    /// Start typing `text`, a key every few frames.
    fn type_slowly(&mut self, text: &str) {
        self.typing.extend(text.chars());
    }

    /// One frame of the window: what the room took since the last frame comes in, the next key
    /// of the person's typing when one is due goes out, and each conversation's events are
    /// taken, as the app's frame does. Timed: nothing in it may wait for an assistant.
    fn frame(&mut self, chats: &mut [&mut Assistant]) {
        let started = Instant::now();
        let key = if self.frames.is_multiple_of(KEY_EVERY) { self.typing.pop_front() } else { None };
        let theirs = self.theirs();
        self.own
            .lend(&mut self.session, |c| {
                c.sync();
                if let Some(key) = key {
                    c.run("text.insert", &json!({"text": key.to_string()})).unwrap();
                    c.send();
                }
            })
            .unwrap();
        for a in chats.iter_mut() {
            a.poll();
        }
        let took = started.elapsed();
        assert!(took < FRAME_BUDGET, "frame {} took {took:?}: the window waited", self.frames);
        if key.is_some() {
            self.keys_at.push(self.frames);
        }
        if self.theirs() != theirs {
            self.theirs_at.push(self.frames);
        }
        self.frames += 1;
        std::thread::sleep(FRAME_GAP);
    }

    /// What shows that a change of another participant's came in: how many of theirs the
    /// window's copy made, and the document's revision records (a copy that reads the whole
    /// document again counts none of the changes in it). Not the records alone: an author's later
    /// changes in the same second reuse its record and add none.
    fn theirs(&self) -> (u64, usize) {
        (self.own.stats().received, self.session.doc.revisions.len())
    }

    /// Whether the window has every change the room took, and the room every change the window
    /// sent.
    fn caught_up(&self) -> bool {
        !self.own.has_unconfirmed() && self.own.received() == self.room.version()
    }

    fn text(&self) -> String {
        self.session.doc.final_text(StoryRef::Body)
    }

    /// The text with every tracked change accepted.
    fn accepted(&self) -> String {
        let mut s = Session::new(self.session.doc.clone());
        s.run("review.acceptAll", &json!({})).unwrap();
        s.doc.plain_text(StoryRef::Body)
    }

    /// The authors of the document's tracked changes, each once.
    fn authors(&self) -> Vec<String> {
        let mut s = Session::new(self.session.doc.clone());
        let changes = s.run("review.changes", &json!({})).unwrap();
        let mut all: Vec<String> = changes.as_array().unwrap().iter().map(|c| c["author"].as_str().unwrap_or("").to_string()).collect();
        all.sort();
        all.dedup();
        all
    }

    /// The paragraphs' styles, as `document.inspect` names them.
    fn styles(&self) -> Vec<String> {
        let mut s = Session::new(self.session.doc.clone());
        let doc = s.run("document.inspect", &json!({})).unwrap();
        doc["blocks"].as_array().unwrap().iter().map(|b| b["style"].as_str().unwrap_or("").to_string()).collect()
    }

    /// Whether the window holds what the hub holds.
    fn same_as_the_hub(&self) -> bool {
        self.session.doc == self.room.document().1
    }
}

/// The document's three editors: the person's window, ChatGPT signed in at a fake, and the fake
/// agent reaching the window's room through WordCraft's MCP server, each joined as a suggester.
struct Editors {
    window: Window,
    chatgpt: Assistant,
    agent: Assistant,
}

impl Editors {
    /// ChatGPT signed in at `openai`, and the fake agent running `script`.
    fn new(openai: &OpenAi, script: &Script) -> Editors {
        let window = Window::new();
        let options = || Options { document: "Notes".into(), ..Options::default() };
        let backend = ChatGpt::with_endpoints(openai.folder.path(), openai.endpoints.clone());
        let chatgpt = timed("starting ChatGPT", || Assistant::start(&window.room, Box::new(backend), options())).unwrap();
        let mut spec = AgentSpec::new("Fake Agent", Launch::new(std::env::current_exe().unwrap(), AS_AGENT));
        spec.env.push((SCRIPT_VAR.into(), serde_json::to_string(script).unwrap()));
        spec.reach = Reach::Mcp { cli: PathBuf::from(env!("CARGO_BIN_EXE_wordcraft-cli")), room: window.port.addr() };
        spec.start_timeout = WAIT;
        let agent = timed("starting the agent", || Assistant::start(&window.room, Box::new(Acp::new(spec)), options())).unwrap();
        assert_eq!((chatgpt.name(), agent.name()), ("ChatGPT", "Fake Agent"));
        assert!(chatgpt.tracked() && agent.tracked(), "both suggesters: their edits are tracked changes");
        Editors { window, chatgpt, agent }
    }

    fn frame(&mut self) {
        self.window.frame(&mut [&mut self.chatgpt, &mut self.agent]);
    }

    /// Frames until `done` holds (at most [`WAIT`]).
    fn until(&mut self, what: &str, done: impl Fn(&Editors) -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done(self) {
            assert!(Instant::now() < deadline, "waited for {what}");
            self.frame();
        }
    }

    /// Frames for `time` at the least.
    fn for_a_while(&mut self, time: Duration) {
        let until = Instant::now() + time;
        self.until("the time to pass", |_| Instant::now() >= until);
    }

    /// Frames until both turns ended, the person typed everything, and the window caught up
    /// with the room.
    fn settle(&mut self) {
        self.until("everyone to finish", |e| !e.chatgpt.is_busy() && !e.agent.is_busy() && e.window.typing.is_empty() && e.window.caught_up());
    }

    /// The person asks ChatGPT, from the window: never waiting for it.
    fn ask_chatgpt(&mut self, prompt: &str) {
        timed("asking ChatGPT", || self.chatgpt.ask(prompt, None)).unwrap();
    }

    /// The person asks the agent, from the window: never waiting for it.
    fn ask_agent(&mut self, prompt: &str) {
        timed("asking the agent", || self.agent.ask(prompt, None)).unwrap();
    }

    /// The window ends both conversations: never waiting for them.
    fn end(mut self) {
        timed("ending ChatGPT's conversation", || self.chatgpt.end());
        timed("ending the agent's conversation", || self.agent.end());
    }
}

/// Run `f`, a call the window makes in a frame, and check it took no longer than a frame may.
fn timed<T>(what: &str, f: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let r = f();
    assert!(started.elapsed() < FRAME_BUDGET, "{what} took {:?}: the window waited", started.elapsed());
    r
}

/// The turn's answer so far: what the assistant said since its last prompt.
fn answer(a: &Assistant) -> String {
    a.transcript().last_answer().unwrap_or_default()
}

/// The last turn's error, if it had one.
fn error(a: &Assistant) -> Option<String> {
    a.transcript().entries().rev().take_while(|e| !matches!(e, Entry::Asked { .. })).find_map(|e| match e {
        Entry::Error { message } => Some(message.clone()),
        _ => None,
    })
}

/// How the last turn stopped, when it stopped other than by finishing.
fn stopped(a: &Assistant) -> Option<StopReason> {
    a.transcript().entries().rev().take_while(|e| !matches!(e, Entry::Asked { .. })).find_map(|e| match e {
        Entry::Stopped { stop } => Some(*stop),
        _ => None,
    })
}

/// The last turn's tool calls that completed.
fn completed(a: &Assistant) -> usize {
    a.transcript()
        .entries()
        .rev()
        .take_while(|e| !matches!(e, Entry::Asked { .. }))
        .filter(|e| matches!(e, Entry::Tool(c) if c.status == ToolStatus::Completed))
        .count()
}

/// The agent adds the section's heading at the end, through its MCP server.
fn heading() -> Step {
    Step::Mcp {
        calls: vec![
            Call::new("execute", json!({"command": "caret.docEnd"})),
            Call::new("execute", json!({"command": "text.newParagraph"})),
            Call::new("type_text", json!({"text": HEADING})),
            Call::new("execute", json!({"command": "para.style", "params": {"style": "Heading 1"}})),
        ],
    }
}

/// Then the section's paragraph after it.
fn hours() -> Step {
    Step::Mcp {
        calls: vec![
            Call::new("execute", json!({"command": "caret.docEnd"})),
            Call::new("execute", json!({"command": "text.newParagraph"})),
            Call::new("type_text", json!({"text": HOURS})),
        ],
    }
}

/// The agent reads the document through its MCP server.
fn read() -> Step {
    Step::Mcp { calls: vec![Call::new("get_text", json!({}))] }
}

/// What a `get_text` the agent made said the document's text is.
fn agent_read(answer: &str) -> String {
    let said = answer.trim().strip_prefix("get_text: ok ").unwrap_or_else(|| panic!("{answer}"));
    let v: Value = serde_json::from_str(said).unwrap_or_else(|e| panic!("{e}: {said}"));
    v["text"].as_str().unwrap().to_string()
}

/// Each MCP call of the agent's turn went through.
fn all_ok(answer: &str) {
    let said: Vec<&str> = answer.lines().filter(|l| l.starts_with("execute: ") || l.starts_with("type_text: ")).collect();
    assert!(!said.is_empty() && said.iter().all(|l| l.contains(": ok")), "{answer}");
}

/// The agent's script: turns by prompt.
fn script(replies: &[(&str, Vec<Step>)]) -> Script {
    Script { replies: replies.iter().map(|(k, v)| (k.to_string(), v.clone())).collect::<BTreeMap<_, _>>(), ..Script::default() }
}

const ADD_SECTION: &str = "Add a section on opening hours";
const TIGHTEN: &str = "Tighten the second paragraph";
const READ: &str = "Read the document";

#[test]
fn a_person_chatgpt_and_an_agent_edit_one_document_at_once() {
    let go = Arc::new(AtomicBool::new(false));
    let openai = OpenAi::start(vec![
        // The rewrite: select the paragraph, then type over it once the test lets it.
        vec![sse(created()), sse(call("1", "select_text", &json!({"text": OLD}))), sse(done())],
        vec![sse(created()), Piece::Gate(go.clone()), sse(call("2", "type_text", &json!({"text": NEW}))), sse(done())],
        says("I rewrote the second paragraph."),
        // Reading the document at the end.
        vec![sse(created()), sse(call("3", "get_text", &json!({}))), sse(done())],
        says("Read it."),
    ]);
    let section = vec![heading(), Step::Sleep { ms: 300 }, hours(), Step::Say { text: "Added the section.".into() }];
    let mut e = Editors::new(&openai, &script(&[(ADD_SECTION, section), (READ, vec![read()])]));

    // Both turns start, and the person types while they run; ChatGPT's rewrite comes in halfway
    // through the typing.
    e.ask_chatgpt(TIGHTEN);
    e.ask_agent(ADD_SECTION);
    e.window.type_slowly(TYPED);
    e.until("half the typing", |e| e.window.typing.len() <= TYPED.len() / 2);
    go.store(true, Ordering::Relaxed);
    e.settle();
    assert_eq!((error(&e.chatgpt), error(&e.agent)), (None, None));
    assert_eq!(completed(&e.chatgpt), 2, "select_text and type_text");
    all_ok(&answer(&e.agent));
    assert!(answer(&e.agent).ends_with("Added the section."), "{}", answer(&e.agent));
    // They edited at once: the person typed while the assistants' changes came in.
    let (first, last) = (e.window.theirs_at[0], e.window.theirs_at[e.window.theirs_at.len() - 1]);
    assert!(e.window.keys_at.iter().any(|k| first < *k && *k < last), "keys at {:?}, their changes at {:?}", e.window.keys_at, e.window.theirs_at);

    // The window has what the hub has: everyone's edits, and every key the person typed, once.
    let w = &e.window;
    let all = format!("The studio opens at nine.{TYPED}\n{NEW}\nVisitors are welcome.\n{HEADING}\n{HOURS}");
    assert!(w.same_as_the_hub());
    assert_eq!(w.accepted(), all);
    assert_eq!(w.text(), all, "the final text leaves the tracked deletion out");
    assert_eq!(w.styles(), ["Normal", "Normal", "Normal", "Heading1", "Normal"]);
    // Each assistant's edits are tracked changes in its name; the person's aren't tracked.
    assert_eq!(w.authors(), ["ChatGPT", "Fake Agent"]);
    let (by_chatgpt, by_agent) = (changes_by(&w.session, "ChatGPT"), changes_by(&w.session, "Fake Agent"));
    assert!(by_chatgpt >= 2 && by_agent >= 2, "{by_chatgpt} {by_agent}");

    // The person's Undo takes back only their own typing, however many steps it took, and
    // leaves both assistants' changes; Redo puts it back.
    let mut undone = 0;
    while e.window.session.can_undo() {
        assert!(undone < 200, "Undo never ends");
        timed("Undo", || e.window.run("edit.undo", json!({})));
        undone += 1;
    }
    e.settle();
    assert!(undone >= 2, "what the person typed after an assistant's change came in is a step of its own: {undone}");
    assert_eq!(e.window.accepted(), format!("The studio opens at nine.\n{NEW}\nVisitors are welcome.\n{HEADING}\n{HOURS}"));
    assert_eq!((changes_by(&e.window.session, "ChatGPT"), changes_by(&e.window.session, "Fake Agent")), (by_chatgpt, by_agent));
    while e.window.session.can_redo() {
        e.window.run("edit.redo", json!({}));
    }
    e.settle();
    assert_eq!(e.window.accepted(), all);
    assert!(e.window.same_as_the_hub());

    // Accept ChatGPT's changes and reject the agent's: the rewrite stays, the section goes, and
    // the person's typing is left as it was.
    e.window.run("review.acceptAll", json!({"author": "ChatGPT"}));
    e.window.run("review.rejectAll", json!({"author": "Fake Agent"}));
    e.settle();
    let reviewed = format!("The studio opens at nine.{TYPED}\n{NEW}\nVisitors are welcome.");
    assert_eq!(e.window.text(), reviewed);
    assert!(e.window.authors().is_empty(), "{:?}", e.window.authors());
    assert_eq!(e.window.styles(), ["Normal", "Normal", "Normal"]);
    assert!(e.window.same_as_the_hub());

    // Every participant ends with the hub's document: each assistant reads its own copy.
    e.ask_chatgpt(READ);
    e.ask_agent(READ);
    e.settle();
    assert_eq!(chatgpt_read(&openai, "call_3"), reviewed);
    assert_eq!(agent_read(&answer(&e.agent)), reviewed);
    e.end();
}

#[test]
fn stopping_chatgpt_mid_turn_leaves_the_agent_and_the_person_working() {
    let openai = OpenAi::start(vec![
        // An answer that starts, then says nothing more until it's stopped.
        vec![sse(created()), sse(delta("Let me look at the second paragraph.")), Piece::Hold],
        // The next turn rewrites the paragraph.
        vec![sse(created()), sse(call("1", "select_text", &json!({"text": OLD}))), sse(done())],
        vec![sse(created()), sse(call("2", "type_text", &json!({"text": NEW}))), sse(done())],
        says("Done."),
    ]);
    // The agent's turn takes longer than ChatGPT's answer holds.
    let sleep = u64::try_from((HELD * 2).as_millis()).unwrap();
    let section = vec![heading(), Step::Sleep { ms: sleep }, hours(), Step::Say { text: "Added the section.".into() }];
    let mut e = Editors::new(&openai, &script(&[(ADD_SECTION, section)]));

    e.ask_chatgpt(TIGHTEN);
    e.ask_agent(ADD_SECTION);
    e.window.type_slowly(TYPED);
    e.until("ChatGPT's answer to start", |e| answer(&e.chatgpt) == "Let me look at the second paragraph.");
    // Its answer holds; the window goes on, frame after frame, and the person types.
    e.for_a_while(HELD);
    assert!(e.chatgpt.is_busy() && e.agent.is_busy());
    // The person stops ChatGPT: its turn ends at once, without waiting for the server, and the
    // agent's goes on.
    let stop = Instant::now();
    timed("Stop", || e.chatgpt.cancel());
    e.until("ChatGPT's turn to end", |e| !e.chatgpt.is_busy());
    assert!(stop.elapsed() < HELD, "{:?}", stop.elapsed());
    assert_eq!(stopped(&e.chatgpt), Some(StopReason::Cancelled));
    assert!(e.agent.is_busy(), "the agent's turn goes on");
    e.settle();
    assert_eq!(error(&e.agent), None);
    all_ok(&answer(&e.agent));
    let section = format!("\n{HEADING}\n{HOURS}");
    assert_eq!(e.window.accepted(), format!("The studio opens at nine.{TYPED}\n{OLD}\nVisitors are welcome.{section}"));
    assert_eq!(e.window.authors(), ["Fake Agent"]);
    assert!(e.window.same_as_the_hub());

    // ChatGPT's conversation goes on: asked again, it rewrites the paragraph, while the person
    // types on.
    e.window.type_slowly(" Thanks.");
    e.ask_chatgpt(TIGHTEN);
    e.settle();
    assert_eq!(error(&e.chatgpt), None);
    assert_eq!(completed(&e.chatgpt), 2);
    assert_eq!(e.window.accepted(), format!("The studio opens at nine.{TYPED} Thanks.\n{NEW}\nVisitors are welcome.{section}"));
    assert_eq!(e.window.authors(), ["ChatGPT", "Fake Agent"]);
    assert!(e.window.same_as_the_hub());
    e.end();
}

#[test]
fn killing_the_agent_mid_turn_leaves_chatgpt_and_the_person_working() {
    let go = Arc::new(AtomicBool::new(false));
    let openai = OpenAi::start(vec![
        // ChatGPT's rewrite types over the paragraph only once the test lets it: after the
        // agent was killed.
        vec![sse(created()), sse(call("1", "select_text", &json!({"text": OLD}))), sse(done())],
        vec![sse(created()), Piece::Gate(go.clone()), sse(call("2", "type_text", &json!({"text": NEW}))), sse(done())],
        says("Done."),
    ]);
    // The agent adds the heading, then holds its turn until it's stopped, which it never is:
    // it's killed.
    let section = vec![heading(), Step::Say { text: "Half done.".into() }, Step::Hold];
    let mut e = Editors::new(&openai, &script(&[("Report", vec![Step::Report]), (ADD_SECTION, section), (READ, vec![read()])]));
    e.ask_agent("Report");
    e.settle();
    let report: Value = serde_json::from_str(&answer(&e.agent)).unwrap();
    let pid = report["pid"].as_u64().unwrap();

    e.ask_chatgpt(TIGHTEN);
    e.ask_agent(ADD_SECTION);
    e.window.type_slowly(TYPED);
    e.until("the agent's heading and ChatGPT's first call", |e| answer(&e.agent).ends_with("Half done.") && completed(&e.chatgpt) == 1);
    let killed = Instant::now();
    kill(pid);
    e.until("the agent's turn to end", |e| !e.agent.is_busy());
    assert!(killed.elapsed() < HELD, "{:?}", killed.elapsed());
    let why = error(&e.agent).unwrap_or_default();
    assert!(why.starts_with("Fake Agent stopped"), "the turn says the agent stopped: {why}");
    // ChatGPT's turn goes on, and so does the window, frame after frame, the person typing.
    assert!(e.chatgpt.is_busy(), "ChatGPT's turn goes on");
    e.window.type_slowly(" Thanks.");
    e.for_a_while(HELD);
    go.store(true, Ordering::Relaxed);
    e.settle();
    assert_eq!(error(&e.chatgpt), None);
    assert_eq!(completed(&e.chatgpt), 2);
    // The heading the agent added before it was killed stays, as its tracked change; ChatGPT's
    // rewrite and everything the person typed are in.
    assert_eq!(e.window.accepted(), format!("The studio opens at nine.{TYPED} Thanks.\n{NEW}\nVisitors are welcome.\n{HEADING}"));
    assert_eq!(e.window.authors(), ["ChatGPT", "Fake Agent"]);
    assert!(e.window.same_as_the_hub());

    // The agent's conversation goes on: the next turn starts it again, and it reads what the hub
    // has.
    e.ask_agent(READ);
    e.settle();
    assert_eq!(error(&e.agent), None);
    assert_eq!(agent_read(&answer(&e.agent)), e.window.text());
    e.end();
}

#[test]
fn stopping_the_agent_mid_turn_never_makes_the_window_wait() {
    // ChatGPT isn't asked here. The agent sleeps without reading what WordCraft sends, as an
    // agent slow to stop does: it says it stopped only once it wakes, seconds after the Stop
    // (and before its cancel_timeout, 10 s, so it isn't killed).
    let openai = OpenAi::start(vec![]);
    let sleep = u64::try_from((HELD * 2).as_millis()).unwrap();
    let slow = vec![Step::Say { text: "Thinking it over.".into() }, Step::Sleep { ms: sleep }, hours()];
    let mut e = Editors::new(&openai, &script(&[(ADD_SECTION, slow)]));
    e.ask_agent(ADD_SECTION);
    e.window.type_slowly(TYPED);
    e.until("the agent to start sleeping", |e| answer(&e.agent).ends_with("Thinking it over."));
    // The window's Stop returns at once, though the agent won't answer for seconds.
    timed("Stop", || e.agent.cancel());
    // Frames go on, each within its budget, while the agent is still stopping: a Stop that
    // waited for it would have taken at least this long.
    e.for_a_while(HELD / 2);
    assert!(e.agent.is_busy(), "the agent is slow to stop, so Stop returned before its turn ended");
    e.until("the agent's turn to end", |e| !e.agent.is_busy());
    assert_eq!(stopped(&e.agent), Some(StopReason::Cancelled));
    assert_eq!(error(&e.agent), None);
    e.settle();
    // Nothing the agent would have done after Stop is in; the person's typing is.
    assert_eq!(e.window.accepted(), format!("The studio opens at nine.{TYPED}\n{OLD}\nVisitors are welcome."));
    assert!(e.window.authors().is_empty(), "{:?}", e.window.authors());
    assert!(e.window.same_as_the_hub());
    e.end();
}

/// What one conversation keeps reaches a later one on another backend: ChatGPT proposes a
/// preference, the person keeps it, and an ACP agent started afterwards is told it. Nothing is kept
/// by the proposal alone.
#[test]
fn what_is_kept_with_chatgpt_reaches_an_agent_in_a_later_conversation() {
    use wordcraft_assist::memory::Memory;
    const KEPT: &str = "Use numbered lists in summaries.";
    let openai = OpenAi::start(vec![says(&format!("Summarized. [[remember: {KEPT}]]"))]);
    let mut e = Editors::new(&openai, &script(&[(READ, vec![Step::Report])]));
    let settings = openai.folder.path();
    e.ask_chatgpt("Summarize this, as a numbered list from now on");
    e.settle();
    assert_eq!(answer(&e.chatgpt), "Summarized.");
    let proposed = e.chatgpt.transcript().entries().find_map(|p| if let Entry::Proposal { text, .. } = p { Some(text.clone()) } else { None });
    assert_eq!(proposed.as_deref(), Some(KEPT));
    assert!(Memory::load(settings).items.is_empty(), "the proposal alone keeps nothing");

    // The person's click, as the window's `assist.memory.keep` does it.
    Memory::update(settings, |m| m.add(KEPT, "2026-11-09T00:00:00Z")).unwrap();

    e.agent.set_memory(Memory::load(settings).for_model().text);
    e.ask_agent(READ);
    e.settle();
    // The report quotes the prompt, whose instructions name the marker, which is taken out of it.
    assert!(answer(&e.agent).contains(KEPT), "{}", answer(&e.agent));
    e.end();
}

/// Each change another participant makes is a frame their change came in at, a later one in the
/// same second too, which reuses its author's revision record and adds none.
#[test]
fn the_window_sees_each_change_of_theirs_even_in_the_same_second() {
    let mut w = Window::new();
    let mut helper = w.room.join("Helper", Role::Suggester).unwrap();
    helper.run("caret.docEnd", &json!({})).unwrap();
    helper.run("text.insert", &json!({"text": " One."})).unwrap();
    helper.send();
    w.frame(&mut []);
    let records = w.session.doc.revisions.len();
    // Microseconds later: the same author, kind and (almost always) second, so the same record.
    helper.run("text.insert", &json!({"text": " Two."})).unwrap();
    // Its first change comes back first: a copy sends its next one only then.
    helper.sync();
    w.frame(&mut []);
    assert_eq!(w.theirs_at, [0, 1], "revision records {records} -> {}", w.session.doc.revisions.len());
    assert!(w.text().ends_with("Visitors are welcome. One. Two."), "{}", w.text());
}

/// Kill process `pid` at once, as the system does to a program it ends.
fn kill(pid: u64) {
    #[cfg(unix)]
    let status = std::process::Command::new("kill").args(["-KILL", &pid.to_string()]).status().unwrap();
    #[cfg(windows)]
    let status = std::process::Command::new("taskkill").args(["/F", "/PID", &pid.to_string()]).status().unwrap();
    assert!(status.success(), "{status}");
}

/// What ChatGPT's `get_text` call `call` gave it, as its next request sent it back.
fn chatgpt_read(openai: &OpenAi, call: &str) -> String {
    let bodies = openai.bodies.lock().unwrap().clone();
    let output = bodies
        .iter()
        .flat_map(|b| b["input"].as_array().cloned().unwrap_or_default())
        .find(|i| i["type"] == "function_call_output" && i["call_id"] == call)
        .unwrap_or_else(|| panic!("no output for {call}"));
    let said = output["output"].as_str().unwrap();
    let v: Value = serde_json::from_str(said).unwrap_or_else(|e| panic!("{e}: {said}"));
    v["text"].as_str().unwrap().to_string()
}

// --- A fake of ChatGPT on this machine. ---

/// One piece of an answer `/v1/responses` streams.
#[derive(Clone, Debug)]
enum Piece {
    /// A server-sent event, in a chunk of its own.
    Event(Value),
    /// Say nothing more but keep-alive comments until the client goes away (at most [`WAIT`]).
    Hold,
    /// Say nothing until the test opens this gate (at most [`WAIT`]).
    Gate(Arc<AtomicBool>),
}

fn sse(e: Value) -> Piece {
    Piece::Event(e)
}

fn created() -> Value {
    json!({"type": "response.created", "response": {"id": "resp_1", "object": "response", "model": "gpt-test-2026-01-01", "status": "in_progress"}})
}

fn delta(text: &str) -> Value {
    json!({"type": "response.output_text.delta", "item_id": "msg_1", "output_index": 0, "content_index": 0, "delta": text})
}

fn message(text: &str) -> Value {
    json!({"type": "response.output_item.done", "output_index": 0, "item": {
        "id": "msg_1", "type": "message", "role": "assistant", "status": "completed", "phase": "final_answer",
        "content": [{"type": "output_text", "text": text, "annotations": []}],
    }})
}

fn call(id: &str, name: &str, args: &Value) -> Value {
    json!({"type": "response.output_item.done", "item": {
        "id": format!("fc_{id}"), "type": "function_call", "status": "completed", "call_id": format!("call_{id}"),
        "name": name, "namespace": "wordcraft", "arguments": args.to_string(),
    }})
}

fn done() -> Value {
    json!({"type": "response.completed", "response": {"id": "resp_1", "status": "completed", "usage": {"input_tokens": 10, "output_tokens": 5}}})
}

/// An answer that says `text`.
fn says(text: &str) -> Vec<Piece> {
    vec![sse(created()), sse(delta(text)), sse(message(text)), sse(done())]
}

/// A fake of ChatGPT's sign-in and Responses API on 127.0.0.1, and a settings folder signed in
/// to it ([`OpenAi::start`]): the token endpoint, the model list, and `/v1/responses`, whose
/// n-th request gets the n-th answer.
struct OpenAi {
    folder: Folder,
    endpoints: Endpoints,
    /// The bodies of the requests to `/v1/responses`, in order.
    bodies: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
}

/// What the fake's threads share.
struct Shared {
    answers: Vec<Vec<Piece>>,
    bodies: Arc<Mutex<Vec<Value>>>,
    /// The nonce the sign-in sent, which the id token carries.
    nonce: Mutex<String>,
    stop: Arc<AtomicBool>,
}

const ISSUER: &str = "https://auth.openai.com";
const CLIENT: &str = "oaiapp_gate-1";

impl OpenAi {
    /// Start the fake, with `answers` for `/v1/responses`, and sign in to it from a new settings
    /// folder, as the person does from Options › Account: the first sign-in, which registers
    /// WordCraft, with plan usage allowed.
    fn start(answers: Vec<Vec<Piece>>) -> OpenAi {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::new(Shared { answers, bodies: bodies.clone(), nonce: Mutex::new(String::new()), stop: stop.clone() });
        let serving = shared.clone();
        std::thread::spawn(move || {
            while !serving.stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let shared = serving.clone();
                        std::thread::spawn(move || serve(stream, &shared));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        let folder = Folder::new();
        let endpoints = Endpoints::local(&base);
        let options = SignInOptions { endpoints: endpoints.clone(), port: 0, wait: WAIT, consent: false };
        let mut signing = SignIn::start(folder.path(), options).unwrap();
        let url = signing.url().to_string();
        *shared.nonce.lock().unwrap() = param(&url, "nonce");
        let state = param(&url, "state");
        let (status, page) = get(signing.redirect_uri(), &format!("code=the-code&client_id={CLIENT}&state={state}"));
        assert_eq!(status, 200, "{page}");
        let account = signing.wait(WAIT).unwrap().as_ref().unwrap();
        assert!(account.plan_usage);
        OpenAi { folder, endpoints, bodies, stop }
    }
}

impl Drop for OpenAi {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The sign-in address's parameter `name` (a random value in URL-safe base64, sent as it is).
fn param(url: &str, name: &str) -> String {
    let query = url.split_once('?').map_or("", |(_, q)| q);
    query.split('&').find_map(|kv| kv.strip_prefix(&format!("{name}="))).unwrap_or_else(|| panic!("no {name} in {url}")).to_string()
}

/// The browser comes back to `redirect` with `query`: the status and the page.
fn get(redirect: &str, query: &str) -> (u16, String) {
    let (host, path) = redirect.strip_prefix("http://").unwrap().split_once('/').unwrap();
    let mut s = TcpStream::connect(host).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    write!(s, "GET /{path}?{query} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    (out.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0), out)
}

/// Answer one connection, then close it.
fn serve(mut s: TcpStream, shared: &Shared) {
    let _ = s.set_nonblocking(false);
    let _ = s.set_read_timeout(Some(Duration::from_secs(20)));
    let Some((method, path, body)) = read_request(&mut s) else { return };
    match (method.as_str(), path.as_str()) {
        ("POST", "/oauth/token") => reply(&mut s, 200, &token(shared, &String::from_utf8_lossy(&body))),
        ("GET", "/v1/models") => reply(&mut s, 200, &json!({"models": [{"slug": "gpt-test", "display_name": "GPT Test", "visibility": "list"}]})),
        ("POST", "/v1/responses") => {
            let n = {
                let mut bodies = shared.bodies.lock().unwrap();
                bodies.push(serde_json::from_slice(&body).unwrap_or(Value::Null));
                bodies.len() - 1
            };
            let answer = shared.answers.get(n).cloned().unwrap_or_else(|| says("Nothing more."));
            stream(&mut s, answer, &shared.stop);
        }
        _ => reply(&mut s, 404, &json!({"detail": "Not Found"})),
    }
}

/// The token endpoint: tokens for the client the form names, with an id token for this sign-in
/// (its signature isn't checked) and every scope asked for, plan usage among them.
fn token(shared: &Shared, form: &str) -> Value {
    let field = |name: &str| form.split('&').find_map(|kv| kv.strip_prefix(&format!("{name}="))).unwrap_or_default().to_string();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let mut claims = json!({
        "iss": ISSUER, "aud": [field("client_id")], "sub": "user-1", "exp": now + 3600, "iat": now,
        "email": "writer@example.com", "name": "A Writer", "sid": "session-1",
    });
    if field("grant_type") == "authorization_code" {
        claims["nonce"] = json!(shared.nonce.lock().unwrap().clone());
    }
    let id_token = format!("{}.{}.{}", b64url(br#"{"alg":"RS256","typ":"JWT"}"#), b64url(claims.to_string().as_bytes()), b64url(b"signature"));
    json!({
        "access_token": "access-1", "refresh_token": "refresh-1", "id_token": id_token,
        "token_type": "Bearer", "expires_in": 3600, "scope": SCOPES,
    })
}

fn reply(s: &mut TcpStream, status: u16, body: &Value) {
    let body = body.to_string();
    let head = format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    let _ = s.write_all(head.as_bytes()).and_then(|()| s.write_all(body.as_bytes())).and_then(|()| s.flush());
}

/// Stream `answer` as server-sent events, each in a chunk of its own.
fn stream(s: &mut TcpStream, answer: Vec<Piece>, stop: &AtomicBool) {
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
    if s.write_all(head.as_bytes()).is_err() {
        return;
    }
    for piece in answer {
        match piece {
            Piece::Event(e) => {
                if chunk(s, format!("data: {e}\n\n").as_bytes()).is_err() {
                    return;
                }
            }
            Piece::Gate(open) => {
                let until = Instant::now() + WAIT;
                while !open.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed) && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            Piece::Hold => {
                let until = Instant::now() + WAIT;
                while !stop.load(Ordering::Relaxed) && Instant::now() < until {
                    if chunk(s, b": keep-alive\n\n").is_err() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                return;
            }
        }
    }
    let _ = s.write_all(b"0\r\n\r\n").and_then(|()| s.flush());
}

fn chunk(s: &mut TcpStream, bytes: &[u8]) -> std::io::Result<()> {
    write!(s, "{:x}\r\n", bytes.len())?;
    s.write_all(bytes)?;
    s.write_all(b"\r\n")?;
    s.flush()
}

/// A request's method, path (without the query) and body.
fn read_request(s: &mut TcpStream) -> Option<(String, String, Vec<u8>)> {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let end = loop {
        if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        let n = s.read(&mut buf).ok().filter(|n| *n > 0)?;
        data.extend_from_slice(&buf[..n]);
    };
    let head = String::from_utf8_lossy(&data[..end]).into_owned();
    let mut first = head.lines().next()?.split(' ');
    let (method, target) = (first.next()?.to_string(), first.next()?.to_string());
    let path = target.split_once('?').map_or(target.as_str(), |(p, _)| p).to_string();
    let length = head
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = data[end..].to_vec();
    while body.len() < length {
        let n = s.read(&mut buf).ok().filter(|n| *n > 0)?;
        body.extend_from_slice(&buf[..n]);
    }
    Some((method, path, body))
}

/// URL-safe base64 without padding, as JWTs use.
fn b64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for group in bytes.chunks(3) {
        let b = [group[0], group.get(1).copied().unwrap_or(0), group.get(2).copied().unwrap_or(0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..=group.len() {
            out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
        }
    }
    out
}

/// A settings folder of its own, removed afterwards.
struct Folder(PathBuf);

impl Folder {
    fn new() -> Folder {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("wordcraft-gate-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Folder(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn b64url_is_jwt_base64() {
    assert_eq!(b64url(b""), "");
    assert_eq!(b64url(b"f"), "Zg");
    assert_eq!(b64url(b"fo"), "Zm8");
    assert_eq!(b64url(b"foo"), "Zm9v");
    assert_eq!(b64url(b"foob"), "Zm9vYg");
    assert_eq!(b64url(&[0xfb, 0xff]), "-_8");
}

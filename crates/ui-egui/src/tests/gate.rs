//! #34, the A1 gate: a person and three agents edit one long document at once, in one process,
//! with no stalls.
//!
//! - The person is the window ([`WordApp`] in a headless harness), showing a document of about
//!   190 pages (the built-in sample repeated, each copy numbered, as the export benchmark does),
//!   shared under the name Minoru. They type in the first copy's opening paragraph ("section
//!   1"), a key a frame, through the page's own keyboard input. Each frame is timed: none may
//!   wait for an agent.
//! - The agents are threads of this test, each on its own connection to the room's own port
//!   ([`RoomPort`]), which speaks the control channel's protocol and is opened by an invite's
//!   key, as an agent's MCP server reaches the document (`wordcraft-cli mcp --connect`):
//!   - a copy editor, a suggester, rewrites words in the third copy ("section 3"), its heading
//!     first;
//!   - a fact checker, a commenter, comments on the second copy ("section 2");
//!   - a formatter, an editor, centres the headings of the first copies, the third copy's first,
//!     at the same moment as the copy editor rewrites that heading.
//!
//! The person types a key only once an agent edit came in since their last one, until the
//! agents are done, so their typing and the agents' edits meet in the same frames on a runner of
//! any speed. Every call an agent makes is timed too.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wordcraft_assist::acp::RoomPort;
use wordcraft_doc::props::{Align, CharProps};
use wordcraft_doc::{Block, BlockId, Document, StoryRef};
use wordcraft_engine::Session;
use wordcraft_engine::room::Lobby;

use crate::control::{self, ControlRequest, Outcome};
use crate::{Services, WordApp};

/// Copies of the sample document's body: about 190 pages.
const COPIES: usize = 150;

/// The longest a frame of the window may take once the document is laid out. An agent's call
/// takes much less, but a frame that waited for the hub while an agent's change went in, or for
/// an agent's call to answer, would show up as a stall here.
const FRAME_BUDGET: Duration = Duration::from_secs(1);

/// The longest an agent's call may take.
const CALL_BUDGET: Duration = Duration::from_secs(10);

/// How long an agent thinks between two calls, so the agents keep working while the person
/// types.
const THINK: Duration = Duration::from_millis(150);

/// The time between two frames.
const FRAME_GAP: Duration = Duration::from_millis(4);

/// What the person types at the end of the opening paragraph, a key a frame.
const TYPED: &str = " Bring a mug; the kettle is always on.";
/// Where they type: the end of the opening paragraph of the first copy.
const OPENING_END: &str = "become a crew. [0]";

/// The copy editor's rewrites: the text, the occurrence of it in the whole document, and what it
/// becomes. The first is in the paragraph the person types in, before their caret; the second in
/// the third copy's first heading, which the formatter centres meanwhile; the rest in the third
/// copy.
const REWRITES: [(&str, u64, &str); 4] = [
    ("the old print shop", 1, "the former print shop"),
    ("a studio?", 3, "one studio?"),
    ("the obvious reason", 3, "the plain reason"),
    ("would never have seen", 3, "would not have seen"),
];

/// The fact checker's comments in the second copy: the text, its occurrence, the comment.
const COMMENTS: [(&str, u64, &str); 3] = [
    ("thirty strangers", 2, "Is it thirty? The sign-up sheet says twenty-eight."),
    ("glaze chemistry", 2, "A ceramicist, or a potter? Ask Ana."),
    ("Thursday evening", 2, "Thursdays, or Wednesdays since March?"),
];

/// The headings the formatter centres, as occurrences of their first words: the third copy's
/// first (the one the copy editor rewrites meanwhile), then the first eight copies'.
const HEADING: &str = "Why share";
const CENTRED: [u64; 8] = [3, 1, 2, 4, 5, 6, 7, 8];

/// The sample document's body `copies` times, each paragraph ending in its copy's number in
/// brackets, so no two copies share a paragraph's text (as the export benchmark does).
fn long_document(copies: usize) -> Document {
    let mut d = wordcraft_engine::sample::sample_document();
    let one: Vec<Block> = d.body.iter().map(|b| (**b).clone()).collect();
    d.body.clear();
    for i in 0..copies {
        for b in &one {
            let mut b = b.clone();
            if let Block::Para(p) = &mut b {
                let at = p.len();
                p.insert_text(at, &format!(" [{i}]"), &CharProps::default()).unwrap();
            }
            d.body.push(Arc::new(b));
        }
    }
    d
}

fn owner(a: &mut WordApp, method: &str, params: Value) -> Value {
    let (req, _reply) = ControlRequest::new(method, params);
    match control::handle(a, &egui::Context::default(), &req) {
        Outcome::Done(v) if v["ok"] == true => v["result"].clone(),
        Outcome::Done(v) => panic!("{method}: {v}"),
        Outcome::Screenshot { .. } => panic!("{method} took a screenshot"),
    }
}

/// An agent's connection to the room's port, with its invite's key on its first request.
struct Agent {
    name: &'static str,
    reader: BufReader<TcpStream>,
    out: TcpStream,
    key: Option<String>,
    id: u64,
    /// Each call's method and how long it took.
    calls: Vec<(String, Duration)>,
}

impl Agent {
    fn connect(addr: SocketAddr, name: &'static str, key: String) -> Agent {
        let out = TcpStream::connect(addr).unwrap();
        out.set_read_timeout(Some(Duration::from_secs(120))).unwrap();
        let reader = BufReader::new(out.try_clone().unwrap());
        Agent { name, reader, out, key: Some(key), id: 0, calls: Vec::new() }
    }

    /// Call `method` with `params`: its result, or its error.
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.id += 1;
        let mut req = json!({"id": self.id, "method": method, "params": params});
        if let Some(key) = self.key.take() {
            req["key"] = json!(key);
        }
        std::thread::sleep(THINK);
        let started = Instant::now();
        writeln!(self.out, "{req}").unwrap();
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        self.calls.push((method.to_string(), started.elapsed()));
        let reply: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{}: {e}: {line}", self.name));
        assert_eq!(reply["id"], json!(self.id), "{}: {reply}", self.name);
        if reply["ok"] == true { Ok(reply["result"].clone()) } else { Err(reply["error"].as_str().unwrap_or("").to_string()) }
    }

    /// Run command `command` with `params`; it must work.
    fn run(&mut self, command: &str, params: Value) -> Value {
        self.call("engine.execute", json!({"command": command, "params": params}))
            .unwrap_or_else(|e| panic!("{}: {command} {params}: {e}", self.name))
    }

    fn slowest(&self) -> (String, Duration) {
        self.calls.iter().max_by_key(|(_, t)| *t).cloned().unwrap_or_default()
    }
}

/// How the agents start together, and how many edits they made: each agent counts an edit once
/// the room has it (an agent's call answers after its copy sent the change).
#[derive(Clone)]
struct Cue {
    start: Arc<Barrier>,
    edits: Arc<AtomicUsize>,
}

impl Cue {
    fn edited(&self) {
        self.edits.fetch_add(1, Ordering::SeqCst);
    }

    fn edits(&self) -> usize {
        self.edits.load(Ordering::SeqCst)
    }
}

/// The copy editor: each rewrite selects its text and types over it (tracked, as a suggester's).
fn copy_editor(mut a: Agent, cue: Cue) -> Agent {
    cue.start.wait();
    for (text, occurrence, new) in REWRITES {
        a.run("select.text", json!({"text": text, "occurrence": occurrence}));
        a.run("text.insert", json!({"text": new}));
        cue.edited();
    }
    a
}

/// The fact checker: each comment selects its text and comments on it.
fn fact_checker(mut a: Agent, cue: Cue) -> Agent {
    cue.start.wait();
    for (text, occurrence, comment) in COMMENTS {
        a.run("select.text", json!({"text": text, "occurrence": occurrence}));
        a.run("review.newComment", json!({"text": comment}));
        cue.edited();
    }
    a
}

/// The formatter: each heading is selected by its first words and centred.
fn formatter(mut a: Agent, cue: Cue) -> Agent {
    cue.start.wait();
    for occurrence in CENTRED {
        a.run("select.text", json!({"text": HEADING, "occurrence": occurrence}));
        a.run("para.alignCenter", json!({}));
        cue.edited();
    }
    a
}

/// How often `what` is in `text`.
fn count(text: &str, what: &str) -> usize {
    text.matches(what).count()
}

/// The document's text with every tracked change accepted.
fn accepted(doc: &Document) -> String {
    let mut s = Session::new(doc.clone());
    s.run("review.acceptAll", &json!({})).unwrap();
    s.doc.plain_text(StoryRef::Body)
}

/// The authors of `doc`'s tracked changes and comments, each once, and its comments' texts.
fn authors(doc: &Document) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut s = Session::new(doc.clone());
    let names = |v: &Value| {
        let mut all: Vec<String> = v.as_array().unwrap().iter().map(|c| c["author"].as_str().unwrap_or("").to_string()).collect();
        all.sort();
        all.dedup();
        all
    };
    let changes = s.run("review.changes", &json!({})).unwrap();
    let comments = s.run("review.comments", &json!({})).unwrap();
    let texts = comments.as_array().unwrap().iter().map(|c| c["text"].as_str().unwrap_or("").to_string()).collect();
    (names(&changes), names(&comments), texts)
}

/// The plain text of paragraph `id` of `doc`'s body (empty when there is none).
fn para_text(doc: &Document, id: BlockId) -> String {
    doc.body
        .iter()
        .find_map(|b| match &**b {
            Block::Para(p) if p.id == id => Some(p.plain_text()),
            _ => None,
        })
        .unwrap_or_default()
}

/// How the headings starting with [`HEADING`] are aligned, in order.
fn heading_aligns(doc: &Document) -> Vec<Align> {
    doc.body
        .iter()
        .filter_map(|b| match &**b {
            Block::Para(p) if p.plain_text().starts_with(HEADING) => Some(doc.styles.resolve_para(&p.props).align),
            _ => None,
        })
        .collect()
}

type Window = egui_kittest::Harness<'static, WordApp>;

/// The window, showing the long document, laid out.
fn window(lobby: &Lobby) -> Window {
    let mut app = WordApp::new(Session::new(long_document(COPIES)), Services::default()).with_lobby(lobby.clone());
    app.session.author = "Minoru".into();
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1440.0, 900.0)).build_ui_state(
        |ui, app: &mut WordApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    );
    h.run_steps(3);
    let pages = h.state_mut().session.layout().pages.len();
    eprintln!("gate: {pages} pages");
    assert!(pages >= 150, "a long document: {pages} pages");
    h
}

/// The window shares its document and invites the three agents, each connecting on its own.
fn invite_agents(h: &mut Window, lobby: &Lobby) -> (RoomPort, [Agent; 3]) {
    owner(h.state_mut(), "room.share", json!({}));
    let port = RoomPort::open(lobby).unwrap();
    let addr = lobby.addr().unwrap();
    let mut invite = |name: &'static str, role: &str| {
        let key = owner(h.state_mut(), "room.invite", json!({"name": name, "role": role}))["key"].as_str().unwrap().to_string();
        let mut a = Agent::connect(addr, name, key);
        assert_eq!(a.call("room.participants", json!({})).unwrap()["you"]["name"], name);
        a
    };
    (port, [invite("Copy editor", "suggester"), invite("Fact checker", "commenter"), invite("Formatter", "editor")])
}

/// The person's caret stays where their typing left it, whatever the agents do: after the
/// `typed` keys they typed, at the end of paragraph `opening`, though the copy editor rewrites
/// words before it.
fn caret_stays(h: &Window, opening: BlockId, typed: usize, frame: usize) {
    let s = &h.state().session;
    let p = s.doc.para_at(&s.sel.focus);
    let so_far: String = TYPED.chars().take(typed).collect();
    assert_eq!(p.map(|p| p.id), Some(opening), "frame {frame}: the person's caret left their paragraph");
    assert!(
        p.is_some_and(|p| s.sel.focus.off == p.len() && p.plain_text().ends_with(&format!("{OPENING_END}{so_far}"))),
        "frame {frame}: the person's caret jumped from the end of what they typed"
    );
    assert_eq!(s.sel.anchor, s.sel.focus, "frame {frame}: the person's caret became a selection");
}

/// The agents start together, and the person types [`TYPED`] at the end of paragraph `opening`,
/// a key a frame, until they and the agents are done. While the agents work, each key waits for
/// an agent edit made since the last key, so the frame that types it brings that edit in too.
/// Each frame is timed, and the caret checked.
fn type_while_agents_work(h: &mut Window, opening: BlockId, agents: [Agent; 3]) -> Vec<Agent> {
    let cue = Cue { start: Arc::new(Barrier::new(4)), edits: Arc::new(AtomicUsize::new(0)) };
    let [ce, fc, fm] = agents;
    let threads = [
        std::thread::spawn({
            let c = cue.clone();
            move || copy_editor(ce, c)
        }),
        std::thread::spawn({
            let c = cue.clone();
            move || fact_checker(fc, c)
        }),
        std::thread::spawn({
            let c = cue.clone();
            move || formatter(fm, c)
        }),
    ];
    cue.start.wait();
    let mut seen = 0;
    let mut typing = TYPED.chars();
    let (mut typed, mut frames, mut came_in, mut slowest) = (0usize, 0usize, 0usize, Duration::ZERO);
    let began = Instant::now();
    let theirs = |h: &Window| h.state().share.as_ref().map(|s| s.copy.stats().received);
    while typed < TYPED.chars().count() || threads.iter().any(|t| !t.is_finished()) {
        assert!(began.elapsed() < Duration::from_secs(300), "the agents never finished");
        while cue.edits() == seen && threads.iter().any(|t| !t.is_finished()) {
            assert!(began.elapsed() < Duration::from_secs(300), "the agents stopped editing");
            std::thread::sleep(FRAME_GAP);
        }
        seen = cue.edits();
        let key = typing.next();
        if let Some(c) = key {
            h.event(egui::Event::Text(c.to_string()));
        }
        let before = theirs(h);
        let t = Instant::now();
        h.step();
        let took = t.elapsed();
        slowest = slowest.max(took);
        assert!(took < FRAME_BUDGET, "frame {frames} took {took:?}: the person's typing waited");
        typed += usize::from(key.is_some());
        came_in += usize::from(key.is_some() && theirs(h) != before);
        caret_stays(h, opening, typed, frames);
        frames += 1;
        std::thread::sleep(FRAME_GAP);
    }
    let agents: Vec<Agent> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    eprintln!("gate: {frames} frames, slowest {slowest:?}, {came_in} typed a key and brought agents' changes in, {:?} in all", began.elapsed());
    assert!(came_in >= 6, "agents' changes came in with the person's keys ({came_in} frames)");
    for a in &agents {
        let (method, took) = a.slowest();
        eprintln!("gate: {}'s slowest call: {method} {took:?}", a.name);
        assert!(took < CALL_BUDGET, "{}'s {method} took {took:?}", a.name);
    }
    agents
}

/// Frames until the window has everything the room took, and the room everything it sent.
fn settle(h: &mut Window) {
    for _ in 0..200 {
        h.step();
        if h.state().share_settled() {
            return;
        }
        std::thread::sleep(FRAME_GAP);
    }
    panic!("the window never caught up");
}

/// Every copy ends with the hub's document, which this returns.
fn every_copy_is_the_hub_s(h: &Window, agents: &mut [Agent]) -> Document {
    let (version, hub) = h.state().shared_room().unwrap().document();
    assert_eq!(h.state().session.doc, hub, "the window ends with the hub's document");
    let hub_seen = Session::new(hub.clone()).run("document.inspect", &json!({"text": true})).unwrap();
    for a in agents {
        assert_eq!(a.call("room.participants", json!({})).unwrap()["version"], json!(version), "{} caught up", a.name);
        let seen = a.call("document.inspect", json!({"text": true})).unwrap();
        assert_eq!(seen["blocks"], hub_seen["blocks"], "{}'s copy is the hub's", a.name);
    }
    hub
}

/// Every edit is in `doc` once, with its author.
fn every_edit_is_there_once(doc: &Document) {
    let text = doc.final_text(StoryRef::Body);
    assert_eq!(count(&text, TYPED), 1, "the person's typing, once");
    for (_, _, new) in REWRITES {
        assert_eq!(count(&text, new), 1, "the copy editor's {new:?}, once");
    }
    let (changed_by, commented_by, mut comments) = authors(doc);
    assert_eq!(changed_by, ["Copy editor"], "the copy editor's rewrites are tracked in their name, and only theirs");
    assert_eq!(commented_by, ["Fact checker"]);
    let mut wanted: Vec<String> = COMMENTS.iter().map(|(_, _, c)| c.to_string()).collect();
    wanted.sort();
    comments.sort();
    assert_eq!(comments, wanted, "each comment once");
    let aligns = heading_aligns(doc);
    assert_eq!(aligns.len(), COPIES);
    assert!(aligns.iter().take(CENTRED.len()).all(|a| *a == Align::Center), "the formatter centred the first headings: {aligns:?}");
    assert!(aligns.iter().skip(CENTRED.len()).all(|a| *a != Align::Center), "and no other");
    let both = doc.body.iter().find_map(|b| match &**b {
        Block::Para(p) if p.plain_text().contains("one studio?") => Some(p),
        _ => None,
    });
    assert!(both.is_some_and(|p| doc.styles.resolve_para(&p.props).align == Align::Center), "the heading both edited has both edits");
}

/// Undo takes back the person's typing in paragraph `opening`, and nothing of the agents' in
/// `hub`, the document before.
fn undo_takes_back_only_the_typing(h: &mut Window, opening: BlockId, hub: &Document) {
    let mut undos = 0;
    while !para_text(&h.state().session.doc, opening).ends_with(OPENING_END) {
        assert!(undos < TYPED.len(), "Undo took the typing back");
        owner(h.state_mut(), "edit.undo", json!({}));
        h.step();
        undos += 1;
    }
    settle(h);
    let after = h.state().shared_room().unwrap().document().1;
    assert!(para_text(&after, opening).ends_with(OPENING_END), "the hub has the typing taken back");
    let text = after.final_text(StoryRef::Body);
    for (_, _, new) in REWRITES {
        assert_eq!(count(&text, new), 1, "Undo left the copy editor's {new:?}");
    }
    assert_eq!(authors(&after).2.len(), COMMENTS.len(), "Undo left the comments");
    assert_eq!(heading_aligns(&after), heading_aligns(hub), "Undo left the formatting");
    assert_eq!(accepted(&after), accepted(hub).replacen(TYPED, "", 1), "Undo took back the typing only");
}

/// Saved, the document opens again with every tracked change and comment in its author's name.
fn saved_keeps_who_made_each_change(h: &mut Window) {
    let dir = std::env::temp_dir().join(format!("wordcraft-gate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("handbook.docx");
    owner(h.state_mut(), "file.saveAs", json!({"path": path.to_string_lossy()}));
    let reopened = wordcraft_engine::io::open_path(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let (changed_by, commented_by, comments) = authors(&reopened);
    assert_eq!((changed_by, commented_by), (vec!["Copy editor".to_string()], vec!["Fact checker".to_string()]));
    assert_eq!(comments.len(), COMMENTS.len());
    let text = reopened.final_text(StoryRef::Body);
    for (_, _, new) in REWRITES {
        assert_eq!(count(&text, new), 1, "the saved file has the copy editor's {new:?}");
    }
}

#[test]
fn a_person_and_three_agents_edit_a_long_document_at_once_with_no_stalls() {
    let lobby = Lobby::default();
    let mut h = window(&lobby);
    let (_port, agents) = invite_agents(&mut h, &lobby);
    // The person's caret at the end of the opening paragraph, the page holding the keyboard.
    owner(h.state_mut(), "select.text", json!({"text": OPENING_END}));
    owner(h.state_mut(), "caret.right", json!({}));
    h.state_mut().canvas.focused = true;
    h.run_steps(2);
    let opening = {
        let s = &h.state().session;
        s.doc.para_at(&s.sel.focus).unwrap().id
    };
    let mut agents = type_while_agents_work(&mut h, opening, agents);
    settle(&mut h);
    let hub = every_copy_is_the_hub_s(&h, &mut agents);
    every_edit_is_there_once(&hub);
    // The window shows the three, each where their last call left them.
    let avatars = h.state().avatars();
    let mut names: Vec<&str> = avatars.iter().map(|a| a.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["Copy editor", "Fact checker", "Formatter"]);
    assert!(avatars.iter().all(|a| a.placed), "each has a place in the document");
    assert_eq!(h.state().others_here().len(), 3);
    undo_takes_back_only_the_typing(&mut h, opening, &hub);
    saved_keeps_who_made_each_change(&mut h);
}

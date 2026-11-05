//! The assistant as a participant of the window's shared document, with the scripted backend.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use wordcraft_doc::{Document, StoryRef};
use wordcraft_engine::Session;
use wordcraft_engine::room::{Replica, Role, Room};

use crate::backend::{MAX_EVENT_TEXT, MAX_LABEL, MAX_OPTIONS, PERMISSION_TIMEOUT};
use crate::tools::{CopyTools, MAX_OUTPUT, TOOLS, specs};
use crate::{
    Answer, AssistError, Assistant, Caps, Context, Entry, Event, Fake, Options, PermissionKind, PermissionOption, PermissionPolicy,
    PermissionRequest, Step, StopReason, ToolHost, ToolStatus, Transcript, role_for,
};

const WAIT: Duration = Duration::from_secs(20);

/// The window: its session, showing "one two three", shared under the name Minoru.
struct Window {
    session: Session,
    room: Arc<Room>,
    own: Replica,
}

impl Window {
    fn new(text: &str) -> Window {
        let mut session = Session::new(Document::from_text(text));
        session.author = "Minoru".into();
        let (room, own) = Room::host(&mut session, "Minoru").unwrap();
        Window { session, room, own }
    }

    /// Frames until the window has every change the room took.
    fn settle(&mut self) {
        for _ in 0..200 {
            let _ = self.own.lend(&mut self.session, Replica::sync);
            if !self.own.has_unconfirmed() && self.own.received() == self.room.version() {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("the window doesn't catch up");
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

    fn authors(&self) -> Vec<String> {
        self.session.doc.revisions.iter().map(|r| r.author.clone()).collect()
    }

    fn start(&self, fake: Fake, options: Options) -> Assistant {
        Assistant::start(&self.room, Box::new(fake), Options { document: "Notes".into(), ..options }).unwrap()
    }
}

/// Ask `prompt` and wait for the turn to end.
fn ask(a: &mut Assistant, prompt: &str) {
    a.ask(prompt, None).unwrap();
    assert!(a.wait_idle(WAIT), "the turn didn't end");
}

/// Replace "two" with "2": select it, then type over it.
fn rewrite_two() -> Vec<Step> {
    vec![
        Step::say("Rewriting "),
        Step::say("“two”."),
        Step::tool("select_text", json!({"text": "two"})),
        Step::tool("type_text", json!({"text": "2"})),
        Step::say(" Done."),
    ]
}

fn tool_rows(t: &Transcript) -> Vec<(String, ToolStatus)> {
    t.entries()
        .filter_map(|e| match e {
            Entry::Tool(c) => Some((c.title.clone(), c.status)),
            _ => None,
        })
        .collect()
}

#[test]
fn the_assistant_s_tool_calls_are_tracked_changes_in_its_name_and_the_window_sees_them() {
    let mut w = Window::new("one two three");
    let fake = Fake::new().then(rewrite_two());
    let seen = fake.seen();
    let mut a = w.start(fake, Options::default());
    assert_eq!((a.name(), a.role(), a.tracked()), ("Test Assistant", Role::Suggester, true));
    ask(&mut a, "Use digits");
    w.settle();
    // A suggestion in its name: the window shows both the old word and the new one, marked.
    assert_eq!(w.authors(), ["Test Assistant", "Test Assistant"]);
    assert_eq!(w.accepted(), "one 2 three");
    assert_eq!(w.text(), "one 2 three", "the final text leaves the tracked deletion out");
    assert_eq!(w.session.doc, w.room.document().1);
    // The pane: the answer in one piece, the tool rows done, the model named.
    let t = a.transcript();
    assert_eq!(t.last_answer().as_deref(), Some("Rewriting “two”.\n\nDone."), "the pieces around the tool calls, in order");
    let pieces = t.entries().filter(|e| matches!(e, Entry::Answer { .. })).count();
    assert_eq!(pieces, 2, "the text before the tool calls is one piece, merged from its chunks");
    assert_eq!(tool_rows(t), [("select_text".to_string(), ToolStatus::Completed), ("type_text".to_string(), ToolStatus::Completed)]);
    let changed: Vec<Option<u64>> = t.entries().filter_map(|e| if let Entry::Tool(c) = e { Some(c.version) } else { None }).collect();
    assert!(changed[1].is_some(), "the edit says which version it made: {changed:?}");
    assert!(t.entries().any(|e| matches!(e, Entry::Model { name } if name == "scripted")));
    // The backend began with its seat and got the note on the first turn only.
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.seat, Some(("Test Assistant".to_string(), Role::Suggester)));
    let note = seen.turns[0].note.clone().unwrap();
    assert!(note.contains("Test Assistant") && note.contains("suggester") && note.contains("tracked change") && note.contains("\"Notes\""), "{note}");
    assert!(note.contains("get_changes") && note.contains("inspect_document"), "{note}");
    ask(&mut a, "And again");
    assert_eq!(seen_turns(&a, 2), 2);
}

/// How many turns the conversation had, from the transcript.
fn seen_turns(a: &Assistant, _: usize) -> usize {
    a.transcript().entries().filter(|e| matches!(e, Entry::Asked { .. })).count()
}

#[test]
fn only_the_first_turn_carries_the_note_and_the_context_reaches_the_backend() {
    let w = Window::new("one two three");
    let fake = Fake::new();
    let seen = fake.seen();
    let mut a = w.start(fake, Options::default());
    a.ask("first", Some("Selected text: two".into())).unwrap();
    assert!(a.wait_idle(WAIT));
    ask(&mut a, "second");
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.turns.len(), 2);
    assert!(seen.turns[0].note.is_some());
    assert_eq!(seen.turns[0].context.as_deref(), Some("Selected text: two"));
    assert_eq!(seen.turns[1].note, None);
    // Unscripted, the fake says what it was asked and changes nothing.
    assert!(a.transcript().last_answer().unwrap().contains("second"));
    assert_eq!(w.room.version(), 0);
}

#[test]
fn assistants_join_as_suggesters_unless_the_setting_or_the_conversation_says_otherwise() {
    // No setting: suggester.
    assert_eq!(role_for(Role::Suggester, None), Ok(Role::Suggester));
    // The setting at editor: editor; owner reads as suggester.
    assert_eq!(role_for(Role::Editor, None), Ok(Role::Editor));
    assert_eq!(role_for(Role::Owner, None), Ok(Role::Suggester));
    // A conversation's own role wins; owner is refused.
    assert_eq!(role_for(Role::Editor, Some(Role::Commenter)), Ok(Role::Commenter));
    assert_eq!(role_for(Role::Viewer, Some(Role::Suggester)), Ok(Role::Suggester));
    assert!(matches!(role_for(Role::Suggester, Some(Role::Owner)), Err(AssistError::Role(_))));

    // With the setting at editor, its edits go in untracked.
    let mut w = Window::new("one two three");
    let mut a = w.start(Fake::new().then(rewrite_two()), Options { setting: Some(Role::Editor), ..Options::default() });
    assert_eq!((a.role(), a.tracked()), (Role::Editor, false));
    ask(&mut a, "Use digits");
    w.settle();
    assert_eq!(w.text(), "one 2 three");
    assert!(w.authors().is_empty(), "no tracked change: {:?}", w.authors());

    // The conversation chooses suggester over the setting: tracked again.
    let mut w = Window::new("one two three");
    let mut a = w.start(Fake::new().then(rewrite_two()), Options { setting: Some(Role::Editor), role: Some(Role::Suggester), ..Options::default() });
    ask(&mut a, "Use digits");
    w.settle();
    assert_eq!(w.authors(), ["Test Assistant", "Test Assistant"]);

    // Owner can't be chosen: nothing joins.
    let w = Window::new("one two three");
    let refused = Assistant::start(&w.room, Box::new(Fake::new()), Options { role: Some(Role::Owner), ..Options::default() });
    assert!(matches!(refused, Err(AssistError::Role(_))));
    assert_eq!(w.room.participants().len(), 1, "only the window is in the room");
}

#[test]
fn a_viewer_changes_nothing_and_commands_outside_its_role_are_refused() {
    let mut w = Window::new("one two three");
    let steps = vec![
        Step::tool("select_text", json!({"text": "two"})),
        Step::tool("type_text", json!({"text": "2"})),
        Step::tool("execute", json!({"command": "format.bold"})),
        Step::tool("execute", json!({"command": "file.save", "params": {"path": "/tmp/x.docx"}})),
        Step::tool("execute", json!({"command": "tools.options"})),
        Step::tool("get_text", json!({})),
    ];
    let mut a = w.start(Fake::new().then(steps), Options { role: Some(Role::Viewer), ..Options::default() });
    ask(&mut a, "Try everything");
    w.settle();
    assert_eq!(w.text(), "one two three");
    assert_eq!(w.room.version(), 0, "the hub took nothing from a viewer");
    let rows = tool_rows(a.transcript());
    let status: Vec<ToolStatus> = rows.iter().map(|r| r.1).collect();
    assert_eq!(
        status,
        [ToolStatus::Completed, ToolStatus::Failed, ToolStatus::Failed, ToolStatus::Failed, ToolStatus::Failed, ToolStatus::Completed],
        "{rows:?}"
    );
    // A suggester can't save, open or change settings either.
    let mut w = Window::new("one two three");
    let steps = vec![
        Step::tool("execute", json!({"command": "file.save", "params": {"path": "/tmp/wordcraft-assist-never.docx"}})),
        Step::tool("execute", json!({"command": "file.open", "params": {"path": "/etc/hosts"}})),
        Step::tool("execute", json!({"command": "file.setAuthor", "params": {"name": "Mallory"}})),
        Step::tool("execute", json!({"command": "ui.screenshot"})),
    ];
    let mut a = w.start(Fake::new().then(steps), Options::default());
    ask(&mut a, "Escape");
    w.settle();
    assert!(tool_rows(a.transcript()).iter().all(|r| r.1 == ToolStatus::Failed), "{:?}", tool_rows(a.transcript()));
    assert!(!std::path::Path::new("/tmp/wordcraft-assist-never.docx").exists());
    assert_eq!(w.session.author, "Minoru");
}

#[test]
fn stop_mid_turn_ends_the_tool_calls_and_leaves_the_document_consistent() {
    let mut w = Window::new("one two three");
    let steps = vec![
        Step::tool("execute", json!({"command": "caret.docEnd"})),
        Step::tool("type_text", json!({"text": " four"})),
        Step::Hold,
        Step::tool("type_text", json!({"text": " five"})),
    ];
    let mut a = w.start(Fake::new().then(steps), Options::default());
    a.ask("Count on", None).unwrap();
    // Wait until the first edit is done, then stop while it holds.
    for _ in 0..2000 {
        a.poll();
        if tool_rows(a.transcript()).iter().filter(|r| r.1 == ToolStatus::Completed).count() == 2 {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    // The person types meanwhile.
    let _ = w.own.lend(&mut w.session, |c| {
        c.run("caret.docStart", &json!({})).unwrap();
        c.run("text.insert", &json!({"text": "zero "})).unwrap();
        c.send();
    });
    a.cancel();
    assert!(a.wait_idle(WAIT));
    w.settle();
    assert!(a.transcript().entries().any(|e| matches!(e, Entry::Stopped { stop: StopReason::Cancelled })));
    assert_eq!(w.accepted(), "zero one two three four", "the edit before Stop stays, the one after never happened");
    assert_eq!(w.session.doc, w.room.document().1);
    assert!(!a.is_busy());
    // The conversation goes on after a Stop.
    ask(&mut a, "Anything else?");
    assert!(a.transcript().last_answer().is_some());
}

#[test]
fn ending_the_conversation_takes_the_invite_back() {
    let mut w = Window::new("one two three");
    let fake = Fake::new().then(rewrite_two());
    let seen = fake.seen();
    let mut a = w.start(fake, Options::default());
    let key = a.key().to_string();
    ask(&mut a, "Use digits");
    // Its copy joined on its thread: the window and the assistant.
    assert_eq!(w.room.participants().len(), 2);
    a.end();
    assert!(a.has_ended());
    assert!(matches!(a.ask("more", None), Err(AssistError::Ended)));
    // The key is taken back: nobody joins with it any more, and the copy is out.
    assert!(w.room.join_with(&key).is_err());
    for _ in 0..2000 {
        if seen.lock().unwrap().ended {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(seen.lock().unwrap().ended, "the backend was ended on its thread");
    assert_eq!(w.room.participants().len(), 1);
    // What it did stays, as a suggestion.
    w.settle();
    assert_eq!(w.accepted(), "one 2 three");
    a.end();
}

#[test]
fn a_second_conversation_joins_under_a_name_of_its_own() {
    let w = Window::new("one two three");
    let mut first = w.start(Fake::new(), Options::default());
    let second = w.start(Fake::new(), Options::default());
    assert_eq!((first.name(), second.name()), ("Test Assistant", "Test Assistant (2)"));
    // Once its copy joined, the name stays the first one's after it ends (its suggestions keep
    // it): a third gets another.
    ask(&mut first, "hello");
    first.end();
    let third = w.start(Fake::new(), Options::default());
    assert_eq!(third.name(), "Test Assistant (3)");
    // A backend with no name joins as "Assistant".
    let nameless = w.start(Fake::named("  "), Options::default());
    assert_eq!(nameless.name(), "Assistant");
    // A name no participant may have (control characters) fails cleanly.
    let bad = Assistant::start(&w.room, Box::new(Fake::named("bad\u{7}name")), Options::default());
    assert!(matches!(bad, Err(AssistError::Room(_))));
    // A closed room lets nobody in.
    w.room.close();
    assert!(matches!(Assistant::start(&w.room, Box::new(Fake::new()), Options::default()), Err(AssistError::Room(_))));
}

#[test]
fn a_turn_is_refused_while_one_runs_and_prompts_are_checked() {
    let w = Window::new("one two three");
    let mut a = w.start(Fake::new().then(vec![Step::Hold]), Options::default());
    assert!(matches!(a.ask("   ", None), Err(AssistError::Prompt(_))));
    assert!(matches!(a.ask(&"x".repeat(crate::MAX_PROMPT + 1), None), Err(AssistError::Prompt(_))));
    a.ask("hold on", None).unwrap();
    assert!(matches!(a.ask("again", None), Err(AssistError::Busy)));
    a.cancel();
    assert!(a.wait_idle(WAIT));
    ask(&mut a, "now");
}

#[test]
fn a_backend_that_fails_or_panics_ends_its_turn_with_an_error_and_the_conversation_goes_on() {
    let mut w = Window::new("one two three");
    let fake = Fake::new().then(vec![Step::Fail("the model is away".into())]).then(vec![Step::Panic]).then(rewrite_two());
    let mut a = w.start(fake, Options::default());
    ask(&mut a, "one");
    assert!(a.transcript().entries().any(|e| matches!(e, Entry::Error { message } if message == "the model is away")));
    ask(&mut a, "two");
    assert!(a.transcript().entries().any(|e| matches!(e, Entry::Error { message } if message.contains("unexpectedly"))));
    ask(&mut a, "three");
    w.settle();
    assert_eq!(w.accepted(), "one 2 three");
}

#[test]
fn other_stop_reasons_are_kept() {
    let w = Window::new("x");
    let mut a = w.start(Fake::new().then(vec![Step::say("Long"), Step::Stop(StopReason::MaxTokens)]), Options::default());
    ask(&mut a, "go");
    assert!(a.transcript().entries().any(|e| matches!(e, Entry::Stopped { stop: StopReason::MaxTokens })));
    assert!(StopReason::MaxTokens.says().is_some() && StopReason::EndTurn.says().is_none());
}

#[test]
fn permission_requests_follow_the_policy_and_wait_for_the_person() {
    // Nobody to ask: rejected at once, and the transcript says so.
    let w = Window::new("x");
    let fake = Fake::new().then(vec![Step::Ask("Edit the heading".into())]);
    let seen = fake.seen();
    let mut a = w.start(fake, Options::default());
    ask(&mut a, "go");
    assert_eq!(seen.lock().unwrap().answers, [Answer::Selected { option: "reject".into() }]);
    assert!(a.transcript().entries().any(|e| matches!(e, Entry::Permission { answer: Some(Answer::Selected { option }), .. } if option == "reject")));

    // Allowed by the setting.
    let fake = Fake::new().then(vec![Step::Ask("Edit".into())]);
    let seen = fake.seen();
    let mut a = w.start(fake, Options { permissions: PermissionPolicy::Allow, ..Options::default() });
    ask(&mut a, "go");
    assert_eq!(seen.lock().unwrap().answers, [Answer::Selected { option: "allow".into() }]);

    // The window asks the person: the turn waits for their answer.
    let fake = Fake::new().then(vec![Step::Ask("Rewrite the summary".into()), Step::say("ok")]).then(vec![Step::Ask("Again".into())]);
    let seen = fake.seen();
    let mut a = w.start(fake, Options { permissions: PermissionPolicy::Ask, ..Options::default() });
    a.ask("go", None).unwrap();
    let request = wait_for_request(&mut a);
    assert_eq!(request.title, "Rewrite the summary");
    assert!(!a.answer(request.id, Answer::Selected { option: "not-offered".into() }), "an option it didn't offer is refused");
    assert!(!a.answer(request.id + 1, Answer::Cancelled), "another request's number is refused");
    assert!(a.answer(request.id, Answer::Selected { option: "allow".into() }));
    assert!(a.wait_idle(WAIT));
    assert_eq!(seen.lock().unwrap().answers, [Answer::Selected { option: "allow".into() }]);
    assert!(a.pending_permission().is_none());
    // Stop while it waits: answered cancelled.
    a.ask("again", None).unwrap();
    wait_for_request(&mut a);
    a.cancel();
    assert!(a.wait_idle(WAIT));
    assert_eq!(seen.lock().unwrap().answers.last(), Some(&Answer::Cancelled));
    assert!(PERMISSION_TIMEOUT >= Duration::from_secs(60));
}

fn wait_for_request(a: &mut Assistant) -> PermissionRequest {
    for _ in 0..5000 {
        a.poll();
        if let Some(r) = a.pending_permission() {
            return r.clone();
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("no permission request arrived");
}

#[test]
fn hostile_permission_requests_and_events_are_held_to_the_pane_s_limits() {
    let huge = "y".repeat(1 << 20);
    let options = (0..10_000).map(|i| PermissionOption { id: format!("o{i}"), name: huge.clone(), kind: PermissionKind::AllowOnce });
    let r = PermissionRequest::bounded(&huge, options);
    assert_eq!(r.options.len(), MAX_OPTIONS);
    assert!(r.title.chars().count() <= MAX_LABEL && r.options.iter().all(|o| o.name.chars().count() <= MAX_LABEL));
    let r = PermissionRequest::bounded("t", [PermissionOption { id: String::new(), name: "x".into(), kind: PermissionKind::RejectOnce }]);
    assert!(r.options.is_empty(), "an option without an id can't be answered");
    // Text is cut at a character boundary.
    let e = Event::Text { text: "é".repeat(MAX_EVENT_TEXT) }.bounded();
    let Event::Text { text } = e else { panic!() };
    assert!(text.len() <= MAX_EVENT_TEXT && text.chars().all(|c| c == 'é'));
    // A transcript fed far more than it keeps stays bounded, and keeps the newest.
    let mut t = Transcript::default();
    t.asked("go");
    for i in 0..10_000 {
        t.record(&Event::Tool(crate::ToolCall {
            id: format!("c{i}"),
            title: "t".repeat(400),
            kind: "edit".into(),
            status: ToolStatus::Completed,
            detail: "d".repeat(4000),
            version: None,
        }));
    }
    assert!(t.len() <= crate::transcript::MAX_ENTRIES);
    assert!(matches!(t.entries().last(), Some(Entry::Tool(c)) if c.id == "c9999"));
    for _ in 0..200 {
        t.record(&Event::Text { text: "z".repeat(MAX_EVENT_TEXT) });
    }
    assert!(t.entries().map(|e| if let Entry::Answer { text } = e { text.len() } else { 0 }).sum::<usize>() <= crate::transcript::MAX_BYTES);
}

#[test]
fn the_tools_are_the_mcp_server_s_for_one_document() {
    let names: Vec<&str> = specs().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, TOOLS);
    for s in specs() {
        assert!(!s.description.is_empty(), "{}", s.name);
        assert_eq!(s.parameters["type"], "object", "{}", s.name);
        assert!(s.parameters["properties"].get("doc").is_none(), "{}: a copy holds one document", s.name);
    }
    // Nothing that opens, saves or switches documents, drives the window or shares.
    for gone in ["open_document", "save_document", "new_document", "list_documents", "screenshot", "click", "key", "room", "render_page"] {
        assert!(!names.contains(&gone), "{gone}");
    }
}

#[test]
fn junk_tool_arguments_never_panic() {
    let w = Window::new("one two three");
    let key = w.room.invite("Fuzzer", Role::Suggester).unwrap();
    let mut tools = CopyTools::join(&w.room, &key).unwrap();
    let junk = [
        json!(null),
        json!({}),
        json!([]),
        json!("text"),
        json!(42),
        json!({"text": 5, "command": [], "params": "x", "commands": {"a": 1}, "since": -1, "limit": 1e300, "full": "yes"}),
        json!({"command": "text.insert", "params": {"text": {"nested": true}}}),
        json!({"commands": [null, 5, {"command": 7}, {"command": "format.bold", "params": []}], "keepGoing": "x"}),
        json!({"text": "two", "occurrence": -5}),
        json!({"text": "two", "occurrence": 1e30}),
        json!({"since": 1e30, "limit": -1}),
        json!({"query": {"q": 1}, "scope": 9}),
        json!({"doc": 999, "command": "format.bold"}),
        json!({"commands": vec![json!({"command": "caret.right"}); crate::tools::MAX_BATCH + 1]}),
    ];
    for name in TOOLS.iter().copied().chain(["open_document", "nope", "", "screenshot"]) {
        for j in &junk {
            let out = tools.call(name, j);
            assert!(out.text.len() <= MAX_OUTPUT + 200, "{name} {j}: {} bytes", out.text.len());
            assert!(!out.text.contains(" panicked: "), "{name} {j}: {}", out.text);
        }
    }
    assert!(!tools.call("nope", &json!({})).ok);
    assert!(!tools.call("execute", &json!("x")).ok);
}

/// The assistant's tool path with every command and junk params: no panic, as
/// `hostile_params_never_panic` holds for sessions.
#[test]
fn every_command_with_junk_params_through_the_assistant_s_tools_never_panics() {
    let mut s = Session::new(wordcraft_engine::sample::sample_document());
    s.author = "Minoru".into();
    let (room, _own) = Room::host(&mut s, "Minoru").unwrap();
    let key = room.invite("Fuzzer", Role::Editor).unwrap();
    let mut tools = CopyTools::join(&room, &key).unwrap();
    let junk = [json!({}), json!({"text": 5, "size": "x", "path": [], "pos": {"block": 9999, "off": 99999}}), json!({"value": -1e308, "rows": 1e9})];
    let ids: Vec<&'static str> = s.registry.all().iter().map(|c| c.id).collect();
    let ran = AtomicUsize::new(0);
    for id in ids {
        // Speech reaches the machine's speakers (the engine's own fuzz test holds it silent).
        if id.starts_with("readAloud.") || id == "review.readAloud" || id.starts_with("speech.") {
            continue;
        }
        for j in &junk {
            let out = tools.call("execute", &json!({"command": id, "params": j}));
            assert!(!out.text.contains(" panicked: "), "{id} {j}: {}", out.text);
            ran.fetch_add(1, Ordering::Relaxed);
        }
        // The copy stays a copy of the room's document.
        if tools.copy().stats().read_again > 50 {
            panic!("{id}: the copy keeps reading the document again");
        }
    }
    assert!(ran.load(Ordering::Relaxed) > 1000);
    let copy = tools.copy_mut();
    copy.sync();
    let (_, hub) = room.document();
    assert!(copy.has_unconfirmed() || copy.session().doc == hub, "the copy holds the hub's document");
}

#[test]
fn the_context_holds_the_selection_its_neighbours_and_the_outline() {
    let mut s = Session::new(Document::from_text("Title\nFirst paragraph.\nSecond paragraph.\nThird paragraph.\nFourth paragraph.\nLast."));
    s.run("caret.docStart", &json!({})).unwrap();
    s.run("para.style", &json!({"style": "Heading 1"})).unwrap();
    s.run("select.text", &json!({"text": "Third paragraph."})).unwrap();
    let caps = Caps::default();
    let c = Context::of(&s, &caps);
    assert_eq!(c.selection, "Third paragraph.");
    assert_eq!(c.before, ["Title", "First paragraph.", "Second paragraph."], "the heading counts too");
    assert_eq!(c.after, ["Fourth paragraph.", "Last."]);
    assert_eq!(c.outline, [(1, "Title".to_string())]);
    let text = c.to_text(&caps);
    assert!(text.contains("Selected text:\n<<<\nThird paragraph.\n>>>"), "{text}");
    assert!(text.contains("- Title"), "{text}");
    // A caret: no selection.
    s.run("caret.docEnd", &json!({})).unwrap();
    let c = Context::of(&s, &caps);
    assert!(c.selection.is_empty());
    assert!(c.to_text(&caps).contains("nothing is selected"));
}

#[test]
fn the_context_of_a_huge_document_stays_under_its_caps() {
    let para = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor. ".repeat(40);
    let text: Vec<String> = (0..3000).map(|i| if i % 40 == 0 { format!("Heading {i}") } else { format!("{i} {para}") }).collect();
    let mut s = Session::new(Document::from_text(&text.join("\n")));
    // Every 40th paragraph a heading: 75 of them.
    for i in (0..3000).step_by(40) {
        s.run("select.text", &json!({"text": format!("Heading {i}")})).unwrap();
        s.run("para.style", &json!({"style": "Heading 2"})).unwrap();
    }
    s.run("select.all", &json!({})).unwrap();
    let caps = Caps::default();
    let started = std::time::Instant::now();
    let c = Context::of(&s, &caps);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(c.selection_cut);
    assert!(c.selection.len() <= caps.selection_bytes);
    assert_eq!(c.outline.len(), caps.headings);
    assert!(c.outline_cut);
    let out = c.to_text(&caps);
    assert!(out.len() <= caps.total_bytes);
    // Every cap holds with a selection in the middle and tiny caps too, and nothing is cut inside
    // a character.
    let mut s = Session::new(Document::from_text(&"日本語のテキスト。".repeat(5000)));
    s.run("select.all", &json!({})).unwrap();
    let tiny = Caps { selection_bytes: 10, total_bytes: 50, heading_bytes: 1, paragraph_bytes: 1, ..Caps::default() };
    let c = Context::of(&s, &tiny);
    assert!(c.selection.len() <= 10 && c.selection.chars().all(|ch| "日本語のテキスト。".contains(ch)));
    let out = c.to_text(&tiny);
    assert!(out.len() <= 50);
    let zero = Caps { selection_bytes: 0, selection_paragraphs: 0, around: 0, paragraph_bytes: 0, headings: 0, heading_bytes: 0, total_bytes: 0 };
    assert_eq!(Context::of(&s, &zero).to_text(&zero), "");
}

#[test]
fn the_note_says_what_each_role_may_do() {
    let seat = |role| crate::Seat { name: "Claude Code".into(), role, key: "k".into(), document: String::new() };
    assert!(crate::note(&seat(Role::Viewer), true).contains("not change it"));
    assert!(crate::note(&seat(Role::Commenter), true).contains("comments"));
    assert!(crate::note(&seat(Role::Suggester), false).contains("tracked change"));
    assert!(crate::note(&seat(Role::Editor), true).contains("untracked"));
    assert!(crate::note(&seat(Role::Suggester), false).contains("MCP server"));
    assert!(!crate::note(&seat(Role::Suggester), true).contains('k') || !crate::note(&seat(Role::Suggester), true).contains("\"k\""));
    assert!(format!("{:?}", seat(Role::Editor)).contains("Claude Code") && !format!("{:?}", seat(Role::Editor)).contains("\"k\""));
}

#[test]
fn the_conversation_thread_and_its_copy_can_be_moved_to_other_threads() {
    fn send<T: Send>() {}
    send::<Assistant>();
    send::<Replica>();
    send::<Box<dyn crate::Backend>>();
    let _ = Value::Null;
}

//! The window's assistant with the scripted one ([`Fake`]): what a prompt does in the document,
//! Stop, roles, the setting, and the pane.

use std::time::Duration;

use serde_json::{Value, json};
use wordcraft_assist::{Answer, Entry, Fake, Step, StopReason};
use wordcraft_doc::{Document, StoryRef};
use wordcraft_engine::Session;

use crate::{Services, WordApp};

/// A window showing `text`, written by Minoru.
fn window(text: &str) -> WordApp {
    let mut a = WordApp::new(Session::new(Document::from_text(text)), Services::default());
    a.session.author = "Minoru".into();
    a
}

/// Frames until the assistant's turn ended and the window has every change the room took.
fn settle(a: &mut WordApp) {
    let ctx = egui::Context::default();
    for _ in 0..4000 {
        a.sync_share(&ctx);
        super::tick(a);
        if !a.conversation().is_some_and(|c| c.is_busy()) && a.share_settled() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the assistant doesn't finish");
}

/// Replace "two" with "2": select it, then type over it, saying so.
fn rewrite_two() -> Vec<Step> {
    vec![
        Step::say("Rewriting “two”."),
        Step::tool("select_text", json!({"text": "two"})),
        Step::tool("type_text", json!({"text": "2"})),
        Step::say(" Done."),
    ]
}

/// The text with every tracked change accepted.
fn accepted(a: &WordApp) -> String {
    let mut s = Session::new(a.session.doc.clone());
    s.run("review.acceptAll", &json!({})).unwrap();
    s.doc.plain_text(StoryRef::Body)
}

fn authors(a: &WordApp) -> Vec<String> {
    a.session.doc.revisions.iter().map(|r| r.author.clone()).collect()
}

fn ask(a: &mut WordApp, prompt: &str, role: Option<&str>) -> Value {
    let params = match role {
        Some(r) => json!({"prompt": prompt, "role": r}),
        None => json!({"prompt": prompt}),
    };
    let r = a.execute("assist.ask", params).unwrap();
    settle(a);
    r
}

#[test]
fn a_prompt_makes_tracked_changes_in_the_assistant_s_name() {
    let mut a = window("one two three");
    // A frame asks what is offered: the scripted assistant is, in tests.
    super::tick(&mut a);
    assert_eq!(a.session.why_disabled("assist.ask"), None);
    a.assist_with(Box::new(Fake::new().then(rewrite_two())));
    let r = ask(&mut a, "Use digits", None);
    assert_eq!(r, json!({"asked": true, "assistant": "Test Assistant", "role": "suggester", "tracked": true}));
    // The document was shared for it, and it is a participant there.
    assert!(a.shows_shared_document());
    let room = a.shared_room().unwrap();
    let names: Vec<String> = room.participants().iter().map(|p| p.name.clone()).collect();
    assert!(names.contains(&"Test Assistant".to_string()), "{names:?}");
    assert_eq!(accepted(&a), "one 2 three");
    assert!(a.session.doc.plain_text(StoryRef::Body).contains("two"), "the deletion is a tracked change");
    assert_eq!(authors(&a), ["Test Assistant", "Test Assistant"]);
    // The pane's conversation: the prompt, the answer, the tool rows.
    let c = a.conversation().unwrap();
    let entries: Vec<&Entry> = c.transcript().entries().collect();
    assert!(matches!(entries.first(), Some(Entry::Asked { text }) if text == "Use digits"), "{entries:?}");
    assert_eq!(c.transcript().last_answer().as_deref(), Some("Rewriting “two”.\n\nDone."));
    assert_eq!(entries.iter().filter(|e| matches!(e, Entry::Tool(_))).count(), 2);
}

#[test]
fn stop_ends_the_turn_and_the_next_prompt_goes_on() {
    let mut a = window("one two three");
    a.assist_with(Box::new(Fake::new().then(vec![Step::say("Thinking it over"), Step::Hold]).then(vec![Step::say("Back.")])));
    let disabled = |a: &WordApp| a.session.why_disabled("assist.cancel");
    assert!(disabled(&a).is_some(), "nothing to stop yet");
    a.execute("assist.ask", json!({"prompt": "Take your time"})).unwrap();
    super::tick(&mut a);
    assert!(a.conversation().unwrap().is_busy());
    assert_eq!(disabled(&a), None);
    assert!(a.session.why_disabled("assist.ask").is_some(), "one prompt at a time");
    assert_eq!(a.execute("assist.cancel", json!({})).unwrap(), json!({"stopping": true}));
    settle(&mut a);
    let c = a.conversation().unwrap();
    assert!(c.transcript().entries().any(|e| matches!(e, Entry::Stopped { stop: StopReason::Cancelled })));
    assert!(disabled(&a).is_some());
    ask(&mut a, "Still there?", None);
    assert_eq!(a.conversation().unwrap().transcript().last_answer().as_deref(), Some("Back."));
}

#[test]
fn another_role_starts_a_new_conversation() {
    let mut a = window("one two three");
    a.execute("assist.backend", json!({"id": wordcraft_assist::FAKE_ID})).unwrap();
    let first = ask(&mut a, "Hello", None);
    assert_eq!(first["assistant"], "Test Assistant");
    let again = ask(&mut a, "Again", Some("suggester"));
    assert_eq!(again["assistant"], "Test Assistant");
    assert!(a.assist.next.is_none());
    assert!(!a.conversation().unwrap().has_ended());
    let viewer = ask(&mut a, "Read only", Some("viewer"));
    assert_eq!(viewer["role"], "viewer");
    assert_eq!(viewer["tracked"], false);
    assert_eq!(viewer["assistant"], "Test Assistant (2)");
    assert_eq!(a.conversation().unwrap().transcript().entries().filter(|e| matches!(e, Entry::Asked { .. })).count(), 1);
    // The viewer's refused edit is a separate check, with an injected script.
    a.assist_with(Box::new(Fake::new().then(vec![Step::tool("type_text", json!({"text": "zero "}))])));
    ask(&mut a, "Change something", Some("viewer"));
    assert_eq!(a.session.doc.plain_text(StoryRef::Body), "one two three");
    assert!(a.session.doc.revisions.is_empty());
}

#[test]
fn the_setting_and_the_conversation_s_role_decide_tracking() {
    let mut a = window("one two three");
    assert_eq!(a.execute("assist.defaultRole", json!({"value": "editor"})).unwrap(), json!({"value": "editor"}));
    a.assist_with(Box::new(Fake::new().then(rewrite_two())));
    let r = ask(&mut a, "Use digits", None);
    assert_eq!((r["role"].clone(), r["tracked"].clone()), (json!("editor"), json!(false)));
    assert_eq!(a.session.doc.plain_text(StoryRef::Body), "one 2 three", "an editor's edits aren't tracked");
    assert!(a.session.doc.revisions.is_empty());
    // The conversation's own role wins over the setting.
    a.assist_with(Box::new(
        Fake::new().then(vec![Step::tool("select_text", json!({"text": "three"})), Step::tool("type_text", json!({"text": "3"}))]),
    ));
    let r = ask(&mut a, "And three", Some("suggester"));
    assert_eq!((r["role"].clone(), r["tracked"].clone()), (json!("suggester"), json!(true)));
    assert_eq!(accepted(&a), "one 2 3");
    assert!(!a.session.doc.revisions.is_empty());
    assert!(a.execute("assist.defaultRole", json!({"value": "owner"})).is_err());
    assert_eq!(a.session.prefs.assistant.default_role.name(), "editor");
}

#[test]
fn accept_or_reject_all_of_the_assistant_s_changes_leaves_the_person_s() {
    for accept in [true, false] {
        let mut a = window("one two three");
        a.assist_with(Box::new(Fake::new().then(rewrite_two())));
        ask(&mut a, "Use digits", None);
        // The person's own tracked change.
        a.execute("review.trackChanges", json!({"value": true})).unwrap();
        a.execute("caret.docEnd", json!({})).unwrap();
        a.execute("text.insert", json!({"text": " four"})).unwrap();
        settle(&mut a);
        let name = a.conversation().unwrap().name().to_string();
        assert_eq!(wordcraft_engine::cmd::review::changes_by(&a.session, &name), 2);
        let id = if accept { "review.acceptAll" } else { "review.rejectAll" };
        a.execute(id, json!({"author": name})).unwrap();
        settle(&mut a);
        assert_eq!(wordcraft_engine::cmd::review::changes_by(&a.session, &name), 0);
        assert_eq!(wordcraft_engine::cmd::review::changes_by(&a.session, "Minoru"), 1, "the person's change is left");
        let expected = if accept { "one 2 three four" } else { "one two three four" };
        assert_eq!(accepted(&a), expected);
    }
}

#[test]
fn the_pane_opens_and_closes() {
    let mut a = window("one two three");
    assert!(!crate::panes::open(&a).contains(&crate::panes::Pane::Assistant));
    assert_eq!(a.execute("assist.pane", json!({})).unwrap(), json!({"value": true}));
    assert!(crate::panes::open(&a).contains(&crate::panes::Pane::Assistant));
    assert!(a.assist.focus_prompt > 0, "the prompt box takes the keyboard");
    assert!(!a.assist.offers.is_empty(), "what is offered is asked when it opens");
    assert_eq!(a.execute("assist.pane", json!({"value": false})).unwrap(), json!({"value": false}));
    assert!(!crate::panes::open(&a).contains(&crate::panes::Pane::Assistant));
}

#[test]
fn the_backend_is_chosen_from_those_offered() {
    let mut a = window("one two three");
    let r = a.execute("assist.backend", json!({})).unwrap();
    let ids: Vec<&str> = r["offers"].as_array().unwrap().iter().filter_map(|o| o["id"].as_str()).collect();
    assert!(ids.contains(&wordcraft_assist::FAKE_ID), "{r}");
    let r = a.execute("assist.backend", json!({"id": wordcraft_assist::FAKE_ID})).unwrap();
    assert_eq!(r["chosen"], wordcraft_assist::FAKE_ID);
    assert_eq!(a.ui.assistant, wordcraft_assist::FAKE_ID);
    let e = a.execute("assist.backend", json!({"id": "nobody"})).unwrap_err();
    assert!(e.contains("no such assistant"), "{e}");
    assert_eq!(a.ui.assistant, wordcraft_assist::FAKE_ID, "a refused choice changes nothing");
}

#[test]
fn junk_requests_never_panic() {
    let mut a = window("one");
    for req in [
        json!({"assist": 7}),
        json!({"assist": "nothing"}),
        json!({"assist": "ask"}),
        json!({"assist": "ask", "prompt": "", "role": "owner"}),
        json!({"assist": "ask", "prompt": "hi", "role": 3}),
        json!({"assist": "backend", "id": ["x"]}),
        json!({"assist": "cancel"}),
        json!({"assist": "pane", "open": "yes"}),
        json!({"assist": "\u{0}".repeat(500)}),
    ] {
        let _ = super::handle(&mut a, &req);
    }
    // The ones that reach the window through the engine say why they fail.
    a.session.push_ui_request(json!({"assist": "nothing"}));
    assert!(matches!(super::take_requests(&mut a), Some(Err(_))));
    assert!(super::take_requests(&mut a).is_none());
}

/// In the pane: a prompt typed in its box and sent with Enter makes the assistant's tracked
/// changes, and its Accept all takes them, leaving no change behind.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn the_pane_sends_a_prompt_and_accepts_the_assistant_s_changes() {
    use egui_kittest::kittest::Queryable;
    let mut app = window("one two three");
    app.assist_with(Box::new(Fake::new().then(rewrite_two())));
    // Wide enough for the whole Home tab.
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(2560.0, 900.0)).build_ui_state(
        |ui, app: &mut WordApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    );
    h.run_steps(4);
    // Home › Assistant opens the pane, and its prompt box has the keyboard, though the click on
    // the ribbon hands it back to the page.
    h.state_mut().run("assist.pane", json!({})).unwrap();
    h.state_mut().canvas.want_focus = true;
    h.run_steps(4);
    assert!(h.state().session.view.assistant_pane);
    assert!(h.query_by_label("Accept all").is_none(), "nothing to accept before a conversation");
    h.input_mut().events.push(egui::Event::Text("Use digits".into()));
    h.run_steps(2);
    assert_eq!(h.state().assist.prompt, "Use digits");
    h.key_press(egui::Key::Enter);
    h.run_steps(1);
    for _ in 0..4000 {
        if !h.state().conversation().is_some_and(|c| c.is_busy()) && h.state().share_settled() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
        h.run_steps(1);
    }
    h.run_steps(2);
    assert_eq!(accepted(h.state()), "one 2 three");
    assert_eq!(authors(h.state()), ["Test Assistant", "Test Assistant"]);
    assert!(h.state().assist.prompt.is_empty(), "the prompt box is emptied for the next one");
    h.get_by_label("Accept all").click();
    h.run_steps(3);
    assert_eq!(h.state().session.doc.plain_text(StoryRef::Body), "one 2 three");
    assert_eq!(wordcraft_engine::cmd::review::changes_by(&h.state().session, "Test Assistant"), 0);
}

/// The prompt the scripted assistant was asked last, and the context it came with.
fn last_turn(seen: &std::sync::Arc<std::sync::Mutex<wordcraft_assist::Seen>>) -> (String, String) {
    let seen = seen.lock().unwrap();
    let turn = seen.turns.last().expect("a turn");
    (turn.prompt.clone(), turn.context.clone().unwrap_or_default())
}

/// A window showing two sentences of the sample's opening, with `pick` selected.
fn selected(pick: &str) -> WordApp {
    let mut a = window("Every Thursday evening the old print shop fills with people who make things.\nRent is the obvious reason.");
    a.execute("select.text", json!({"text": pick})).unwrap();
    super::tick(&mut a);
    a
}

/// Run writing action `id` with the scripted assistant doing `steps`: what it was asked.
fn act(a: &mut WordApp, id: &str, params: Value, steps: Vec<Step>) -> (String, String) {
    let fake = Fake::new().then(steps);
    let seen = fake.seen();
    a.assist_with(Box::new(fake));
    let r = a.execute(id, params).unwrap();
    assert_eq!(r["asked"], true, "{id}: {r}");
    settle(a);
    last_turn(&seen)
}

#[test]
fn rewrite_shorten_expand_and_proofread_replace_the_selection_as_tracked_changes() {
    for (id, params, asks) in [
        ("assist.rewrite", json!({}), "Rewrite the selected text"),
        ("assist.rewrite", json!({"tone": "friendly"}), "in a friendly tone"),
        ("assist.shorten", json!({}), "shorter"),
        ("assist.expand", json!({}), "longer"),
        ("assist.proofread", json!({}), "Proofread"),
    ] {
        let mut a = selected("the old print shop");
        a.assist.prompt = "half a question".into();
        // The scripted assistant types over what its copy has selected: the person's selection.
        let (prompt, context) = act(&mut a, id, params, vec![Step::tool("type_text", json!({"text": "the print shop"}))]);
        assert!(prompt.contains(asks), "{id}: {prompt}");
        assert!(context.contains("the old print shop"), "{id}: the selection goes with it: {context}");
        assert!(accepted(&a).starts_with("Every Thursday evening the print shop fills"), "{id}: {}", accepted(&a));
        assert_eq!(authors(&a), ["Test Assistant", "Test Assistant"], "{id}: a deletion and an insertion in its name");
        assert!(a.session.view.assistant_pane, "{id}: the pane shows the turn");
        assert_eq!(a.assist.prompt, "half a question", "{id}: what the person was typing stays");
    }
}

#[test]
fn draft_writes_at_the_caret() {
    let mut a = window("Rent is the obvious reason.");
    a.execute("caret.docEnd", json!({})).unwrap();
    super::tick(&mut a);
    let (prompt, _) = act(
        &mut a,
        "assist.draft",
        json!({"about": "why people come back"}),
        vec![Step::tool("type_text", json!({"text": " People come back for each other."}))],
    );
    assert!(prompt.contains("why people come back"), "{prompt}");
    assert_eq!(accepted(&a), "Rent is the obvious reason. People come back for each other.");
    assert_eq!(authors(&a), ["Test Assistant"]);
    assert!(a.execute("assist.draft", json!({})).is_err(), "Draft needs to know what to write");
    // With text selected, the draft goes after it and the selection stays.
    let mut a = selected("Every Thursday evening");
    act(&mut a, "assist.draft", json!({"about": "a date"}), vec![Step::tool("type_text", json!({"text": " (May 2)"}))]);
    assert!(accepted(&a).starts_with("Every Thursday evening (May 2) the old print shop"), "{}", accepted(&a));
}

#[test]
fn writing_after_a_selected_paragraph_stays_before_the_next_paragraph() {
    for (id, params, steps, expected) in [
        (
            "assist.draft",
            json!({"about": "a date"}),
            vec![Step::tool("type_text", json!({"text": " (May 2)"}))],
            "First. (May 2)\nRent is the obvious reason.",
        ),
        (
            "assist.summarize",
            json!({"insert": true}),
            vec![Step::tool("execute", json!({"command": "text.newParagraph"})), Step::tool("type_text", json!({"text": "In short: rent."}))],
            "First.\nIn short: rent.\nRent is the obvious reason.",
        ),
    ] {
        let mut a = window("First.\nRent is the obvious reason.");
        a.execute("select.paragraph", json!({})).unwrap();
        assert_eq!(a.session.sel.ordered().1, wordcraft_doc::Pos::body(1, 0));
        act(&mut a, id, params, steps);
        assert_eq!(accepted(&a), expected, "{id}");
    }
}

#[test]
fn user_ask_and_draft_open_the_prompt_while_scripts_require_parameters() {
    let mut a = window("one two three");
    super::tick(&mut a);
    for (id, draft) in [("assist.draft", true), ("assist.ask", false)] {
        a.session.view.assistant_pane = false;
        a.assist.draft = !draft;
        a.assist.focus_prompt = 0;
        assert!(a.execute(id, json!({})).is_err(), "{id}: scripts still need text");
        assert_eq!(a.run(id, json!({})).unwrap(), json!({"pending": "assistant"}));
        assert!(a.session.view.assistant_pane, "{id}");
        assert_eq!(a.assist.draft, draft, "{id}");
        assert!(a.assist.focus_prompt > 0, "{id}");
        assert!(a.conversation().is_none(), "{id}: opening the box does not ask a question");
    }
}

#[test]
fn summarize_comments_on_the_selection_or_writes_after_it() {
    let mut a = selected("Rent is the obvious reason.");
    let comment = Step::tool("execute", json!({"command": "review.newComment", "params": {"text": "Why rent isn't the reason."}}));
    let (prompt, _) = act(&mut a, "assist.summarize", json!({}), vec![comment]);
    assert!(prompt.contains("comment"), "{prompt}");
    let doc = &a.session.doc;
    let notes: Vec<(String, String)> = doc.comments.values().map(|c| (c.author.clone(), doc.plain_text(StoryRef::Part(c.part)))).collect();
    assert_eq!(notes, [("Test Assistant".to_string(), "Why rent isn't the reason.".to_string())]);
    assert!(doc.revisions.is_empty(), "the text is left alone");
    // With `insert`, after the selection, which stays as it is.
    let mut a = selected("Rent is the obvious reason.");
    let (prompt, _) = act(&mut a, "assist.summarize", json!({"insert": true}), vec![Step::tool("type_text", json!({"text": " In short: rent."}))]);
    assert!(prompt.contains("at the caret"), "{prompt}");
    assert!(accepted(&a).ends_with("Rent is the obvious reason. In short: rent."), "{}", accepted(&a));
}

#[test]
fn actions_on_the_selection_need_one_and_an_assistant() {
    let mut a = window("one two three");
    super::tick(&mut a);
    for id in ["assist.rewrite", "assist.shorten", "assist.expand", "assist.proofread"] {
        assert_eq!(a.session.why_disabled(id), Some("select the text first"), "{id}");
    }
    assert_eq!(a.session.why_disabled("assist.summarize"), None, "the document, with nothing selected");
    assert_eq!(a.session.why_disabled("assist.draft"), None);
    // No assistant offered: every action says where to set one up.
    a.assist.offers.clear();
    a.execute("select.all", json!({})).unwrap();
    a.session.assistant = super::status(&a);
    for id in ["assist.draft", "assist.rewrite", "assist.shorten", "assist.expand", "assist.proofread", "assist.summarize"] {
        assert_eq!(a.session.why_disabled(id), Some(wordcraft_engine::cmd::assist::NOT_SET_UP), "{id}");
    }
}

#[test]
fn the_person_s_undo_leaves_the_assistant_s_change_alone() {
    let mut a = selected("the old print shop");
    // The first action shares the document; then the person types, and the assistant shortens.
    act(&mut a, "assist.proofread", json!({}), vec![Step::say("Nothing to correct.")]);
    a.execute("caret.docEnd", json!({})).unwrap();
    a.execute("text.insert", json!({"text": " Mine."})).unwrap();
    a.execute("select.text", json!({"text": "the old print shop"})).unwrap();
    act(&mut a, "assist.shorten", json!({}), vec![Step::tool("type_text", json!({"text": "the shop"}))]);
    assert!(accepted(&a).ends_with("reason. Mine."), "{}", accepted(&a));
    a.execute("edit.undo", json!({})).unwrap();
    settle(&mut a);
    assert!(!accepted(&a).contains("Mine."), "the person's own typing is undone: {}", accepted(&a));
    assert!(accepted(&a).starts_with("Every Thursday evening the shop fills"), "the assistant's change stays: {}", accepted(&a));
    // Its deletion and insertion, in the name of the second conversation's assistant (the first
    // may still be leaving the room as the second joins, so it can be "Test Assistant (2)").
    let names = authors(&a);
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names.iter().all(|n| n.starts_with("Test Assistant")), "{names:?}");
}

#[test]
fn a_whole_document_selection_is_capped() {
    let long: Vec<String> = (0..4000).map(|i| format!("Paragraph {i} says something about the studio and its people.")).collect();
    let mut a = window(&long.join("\n"));
    a.execute("select.all", json!({})).unwrap();
    super::tick(&mut a);
    let (_, context) = act(&mut a, "assist.summarize", json!({}), vec![Step::say("A summary.")]);
    let caps = wordcraft_assist::Caps::default();
    assert!(context.len() <= caps.total_bytes, "{} bytes", context.len());
    assert!(context.contains("Paragraph 0 "), "it starts with the selection's start");
    // Nothing selected: the document around the caret, capped the same way.
    a.execute("caret.docStart", json!({})).unwrap();
    let (prompt, context) = act(&mut a, "assist.summarize", json!({}), vec![Step::say("Again.")]);
    assert!(prompt.starts_with("Summarize the document"), "{prompt}");
    assert!(context.len() <= caps.total_bytes, "{} bytes", context.len());
}

/// The menus' Draft opens the pane with its box turned to drafting; what is typed there and
/// sent with Enter is written at the caret.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn draft_from_the_pane_writes_what_the_box_says() {
    let mut app = window("Rent is the obvious reason.");
    app.execute("caret.docEnd", json!({})).unwrap();
    app.assist_with(Box::new(Fake::new().then(vec![Step::tool("type_text", json!({"text": " People stay."}))])));
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(2560.0, 900.0)).build_ui_state(
        |ui, app: &mut WordApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    );
    h.run_steps(4);
    super::start_draft(h.state_mut());
    h.state_mut().canvas.want_focus = true;
    h.run_steps(4);
    assert!(h.state().assist.draft && h.state().session.view.assistant_pane);
    h.input_mut().events.push(egui::Event::Text("why people stay".into()));
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(1);
    for _ in 0..4000 {
        if !h.state().conversation().is_some_and(|c| c.is_busy()) && h.state().share_settled() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
        h.run_steps(1);
    }
    h.run_steps(2);
    assert_eq!(accepted(h.state()), "Rent is the obvious reason. People stay.");
    assert!(h.state().assist.prompt.is_empty() && !h.state().assist.draft, "the box is emptied and asks again");
    let c = h.state().conversation().unwrap();
    assert!(matches!(c.transcript().entries().next(), Some(Entry::Asked { text }) if text.contains("why people stay")));
}

#[test]
fn another_document_keeps_the_conversation_until_sharing_is_explicitly_switched() {
    use egui_kittest::kittest::Queryable;
    let mut a = window("one two three");
    a.assist_with(Box::new(Fake::new().then(vec![Step::say("First.")])));
    ask(&mut a, "Hello", None);
    let first = a.session.document_id();
    let room = a.shared_room().unwrap();
    a.execute("file.new", json!({})).unwrap();
    a.assist.next = Some(Box::new(Fake::new().then(vec![Step::say("Second.")])));
    assert!(a.execute("assist.ask", json!({"prompt": "Hello again"})).is_err());
    assert!(!a.assist.conversation.as_ref().unwrap().assistant.has_ended());
    assert!(a.assist.next.is_some(), "a refused prompt keeps the next backend");
    a.session.view.assistant_pane = true;
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1440.0, 900.0)).build_ui_state(
        |ui, app: &mut WordApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        a,
    );
    h.run_steps(3);
    h.get_by_label("Use this document").click();
    h.run_steps(3);
    assert!(room.is_closed());
    assert!(h.state().session.is_open(first));
    assert_eq!(ask(h.state_mut(), "Hello again", None)["asked"], true);
    assert_eq!(h.state().conversation().unwrap().transcript().last_answer().as_deref(), Some("Second."));
    assert!(h.state().shows_shared_document());
}

#[test]
fn a_role_pick_is_consumed_by_its_conversation() {
    use wordcraft_engine::room::Role;
    let mut a = window("one two three");
    a.assist.role = Some(Role::Editor);
    a.assist_with(Box::new(Fake::new().then(vec![Step::say("First.")])));
    assert_eq!(ask(&mut a, "Hello", None)["role"], "editor");
    a.assist_with(Box::new(Fake::new().then(rewrite_two())));
    let next = ask(&mut a, "Use digits", None);
    assert_eq!(next["role"], "suggester");
    assert_eq!(next["tracked"], true);
    assert!(!a.session.doc.revisions.is_empty());
}

#[test]
fn control_clicks_cannot_answer_an_assistant_permission() {
    use egui_kittest::kittest::Queryable;
    let mut a = window("one");
    let fake = Fake::new().then(vec![Step::Ask("Run a command?".into())]);
    let seen = fake.seen();
    a.assist_with(Box::new(fake));
    a.execute("assist.pane", json!({"value": true})).unwrap();
    // The person's prompt: a program's turn wouldn't ask anyone.
    a.run("assist.ask", json!({"prompt": "Ask permission"})).unwrap();
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1440.0, 900.0)).build_ui_state(
        |ui, app: &mut WordApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        a,
    );
    let frame = |h: &mut egui_kittest::Harness<'_, WordApp>| {
        let ctx = h.ctx.clone();
        let mut raw = std::mem::take(h.input_mut());
        h.state_mut().raw_input_hook(&ctx, &mut raw);
        *h.input_mut() = raw;
        h.step();
    };
    for _ in 0..4000 {
        frame(&mut h);
        if h.state().conversation().is_some_and(|c| c.pending_permission().is_some()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(3);
    let pos = h.get_by_label("Allow once").rect().center();
    let (req, _rx) = crate::control::ControlRequest::new("ui.click", json!({"x": pos.x, "y": pos.y}));
    let ctx = h.ctx.clone();
    assert!(matches!(crate::control::handle(h.state_mut(), &ctx, &req), crate::control::Outcome::Done(_)));
    for _ in 0..20 {
        frame(&mut h);
    }
    assert!(h.state().conversation().unwrap().pending_permission().is_some(), "control input leaves the question pending");
    assert!(seen.lock().unwrap().answers.is_empty());
    let pos = h.get_by_label("Allow once").rect().center();
    h.input_mut().events.push(egui::Event::PointerMoved(pos));
    frame(&mut h);
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
        frame(&mut h);
    }
    settle(h.state_mut());
    assert!(matches!(seen.lock().unwrap().answers.as_slice(), [wordcraft_assist::Answer::Selected { option }] if option == "allow"));
}

/// The window in a harness wide enough for the whole Home tab, after a few frames.
#[cfg(not(target_arch = "wasm32"))]
fn harness(app: WordApp) -> egui_kittest::Harness<'static, WordApp> {
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(2560.0, 900.0)).build_ui_state(
        |ui, app: &mut WordApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    );
    h.run_steps(4);
    h
}

/// Frames until `done` holds, while the assistant's thread works.
#[cfg(not(target_arch = "wasm32"))]
fn until(h: &mut egui_kittest::Harness<'static, WordApp>, done: impl Fn(&WordApp) -> bool) {
    for _ in 0..4000 {
        if done(h.state()) {
            h.run_steps(2);
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
        h.run_steps(1);
    }
    panic!("the window never gets there");
}

fn waiting(a: &WordApp) -> bool {
    a.conversation().is_some_and(|c| c.pending_permission().is_some())
}

fn idle(a: &WordApp) -> bool {
    !a.conversation().is_some_and(|c| c.is_busy()) && a.share_settled()
}

fn answers(seen: &std::sync::Arc<std::sync::Mutex<wordcraft_assist::Seen>>) -> Vec<Answer> {
    seen.lock().unwrap().answers.clone()
}

fn picked(option: &str) -> Answer {
    Answer::Selected { option: option.into() }
}

/// The person's prompt: the agent's permission request shows in the pane (which opens for it)
/// with its answers, and the one they click reaches the agent; Stop answers a waiting request
/// cancelled.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn the_pane_asks_the_person_and_their_choice_reaches_the_agent() {
    use egui_kittest::kittest::Queryable;
    let mut app = window("one two three");
    let fake =
        Fake::new().then(vec![Step::Ask("Delete the old notes".into()), Step::say("Deleted.")]).then(vec![Step::Ask("Rename the file".into())]);
    let seen = fake.seen();
    app.assist_with(Box::new(fake));
    let mut h = harness(app);
    h.state_mut().run("assist.ask", json!({"prompt": "Tidy up"})).unwrap();
    // Closed meanwhile, it opens again to ask.
    h.state_mut().session.view.assistant_pane = false;
    until(&mut h, waiting);
    assert!(h.state().session.view.assistant_pane, "the pane opens to ask");
    assert!(h.query_by_label("Delete the old notes").is_some());
    assert!(h.query_by_label("Test Assistant asks permission").is_some());
    h.get_by_label("Allow once").click();
    h.run_steps(1);
    until(&mut h, idle);
    assert_eq!(answers(&seen), [picked("allow")]);
    assert!(h.query_by_label("Answered: Allow once").is_some());
    assert!(h.query_by_label("Allow once").is_none(), "answered, the buttons go");
    // Stop while the next one waits: it is answered cancelled.
    h.state_mut().run("assist.ask", json!({"prompt": "And rename it"})).unwrap();
    until(&mut h, waiting);
    h.state_mut().run("assist.cancel", json!({})).unwrap();
    until(&mut h, idle);
    assert_eq!(answers(&seen), [picked("allow"), Answer::Cancelled]);
    let c = h.state().conversation().unwrap();
    assert!(
        c.transcript()
            .entries()
            .any(|e| matches!(e, Entry::Permission { request, answer: Some(Answer::Cancelled) } if request.title == "Rename the file"))
    );
    assert!(h.query_by_label("Stopped.").is_some());
}

/// A turn a program asked for (the control channel, MCP) never asks anyone: it answers per the
/// setting, refusing unless the person allowed it. The person's own prompt in the same
/// conversation still asks them.
#[test]
fn a_program_s_turn_answers_per_the_setting() {
    let mut a = window("one two three");
    let fake = Fake::new()
        .then(vec![Step::Ask("Edit the heading".into())])
        .then(vec![Step::Ask("Edit the heading".into())])
        .then(vec![Step::Ask("Edit the title".into())]);
    let seen = fake.seen();
    a.assist_with(Box::new(fake));
    ask(&mut a, "Go", None);
    assert_eq!(answers(&seen), [picked("reject")]);
    let refused = |a: &WordApp, option: &str| {
        a.conversation()
            .unwrap()
            .transcript()
            .entries()
            .filter(|e| matches!(e, Entry::Permission { answer: Some(Answer::Selected { option: o }), .. } if o == option))
            .count()
    };
    assert_eq!(refused(&a, "reject"), 1, "the conversation says how it was answered");
    assert!(a.execute("assist.allowUnattended", json!({"value": true})).is_err(), "only the person allows it");
    // The person ticks it in File › Options › Assistant.
    a.session.prefs.assistant.allow_unattended = true;
    ask(&mut a, "Again", None);
    assert_eq!(answers(&seen), [picked("reject"), picked("allow")]);
    // The person asks: the pane waits for them.
    a.run("assist.ask", json!({"prompt": "Mine"})).unwrap();
    let ctx = egui::Context::default();
    let mut request = None;
    for _ in 0..4000 {
        a.sync_share(&ctx);
        super::tick(&mut a);
        request = a.conversation().and_then(|c| c.pending_permission()).cloned();
        if request.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let request = request.expect("the person is asked");
    assert_eq!(request.title, "Edit the title");
    assert!(a.conversation_mut().unwrap().answer(request.id, picked("reject")));
    settle(&mut a);
    assert_eq!(answers(&seen), [picked("reject"), picked("allow"), picked("reject")]);
    assert!(!a.assist.by_person, "the person's command is over");
}

/// What an agent offers is held to the pane's limits: no option (Stop ends the turn), thousands
/// (the first 16), huge or hostile names and titles; the pane draws each without a panic, and
/// the person's pick reaches the agent.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn hostile_option_lists_are_capped_in_the_pane() {
    use egui_kittest::kittest::Queryable;
    use wordcraft_assist::{PermissionKind, PermissionOption};
    let option = |id: &str, name: String| PermissionOption { id: id.into(), name, kind: PermissionKind::AllowOnce };
    let thousands = (0..5000).map(|n| option(&format!("o{n}"), format!("Option {n}"))).collect();
    let huge = vec![
        option("big", "y".repeat(1 << 20)),
        option(&"z".repeat(1 << 16), "Too long an id".into()),
        option("rtl", "\u{202e}Go ahead\u{0}".into()),
    ];
    let mut app = window("one");
    let fake = Fake::new()
        .then(vec![Step::AskWith { title: "Nothing to pick".into(), options: vec![] }])
        .then(vec![Step::AskWith { title: "x".repeat(1 << 20), options: thousands }])
        .then(vec![Step::AskWith { title: "Huge".into(), options: huge }]);
    let seen = fake.seen();
    app.assist_with(Box::new(fake));
    let mut h = harness(app);
    h.state_mut().run("assist.ask", json!({"prompt": "none"})).unwrap();
    until(&mut h, waiting);
    assert!(h.query_by_label("It offers no answer to choose: Stop ends the turn.").is_some());
    h.state_mut().run("assist.cancel", json!({})).unwrap();
    until(&mut h, idle);
    h.state_mut().run("assist.ask", json!({"prompt": "thousands"})).unwrap();
    until(&mut h, waiting);
    assert!(h.query_by_label("Option 15").is_some() && h.query_by_label("Option 16").is_none(), "the first 16");
    h.get_by_label("Option 3").click();
    h.run_steps(1);
    until(&mut h, idle);
    h.state_mut().run("assist.ask", json!({"prompt": "huge"})).unwrap();
    until(&mut h, waiting);
    assert!(h.query_by_label("Too long an id").is_none(), "an option whose id is too long is dropped");
    h.get_by_label("Go ahead").click();
    h.run_steps(1);
    until(&mut h, idle);
    assert_eq!(answers(&seen), [Answer::Cancelled, picked("o3"), picked("rtl")]);
}

/// A tool call's row is folded to its title; open, it says how the call went, and for a call
/// that changed the document, Show the change selects that change with Review open. The plan
/// is a checklist that counts what is done.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_tool_row_opens_to_the_change_it_made() {
    use egui_kittest::kittest::Queryable;
    use wordcraft_assist::{PlanEntry, PlanStatus, Priority};
    let entry = |content: &str, status| PlanEntry { content: content.into(), priority: Priority::Medium, status };
    let plan = Step::Plan(vec![
        entry("Find “two”", PlanStatus::Completed),
        entry("Use a digit", PlanStatus::InProgress),
        entry("Say so", PlanStatus::Pending),
    ]);
    let mut app = window("one two three");
    app.assist_with(Box::new(Fake::new().then([vec![plan], rewrite_two()].concat())));
    let mut h = harness(app);
    h.state_mut().run("assist.ask", json!({"prompt": "Use digits"})).unwrap();
    until(&mut h, idle);
    assert!(h.query_by_label("1 of 3 done").is_some());
    assert!(h.query_by_label("Use a digit").is_some());
    assert!(h.query_by_label("type_text").is_some(), "the call's title");
    assert!(h.query_by_label("Show the change").is_none(), "folded");
    assert!(h.query_by_label("Edit · Done").is_none(), "folded");
    h.get_by_label("type_text").click();
    h.run_steps(4);
    assert!(h.query_by_label("Edit · Done").is_some());
    h.get_by_label("Show the change").click();
    h.run_steps(2);
    let a = h.state();
    assert_eq!(a.ui.tab, "Review");
    let (start, end) = a.session.sel.ordered();
    let para = a.session.doc.body.first().and_then(|b| b.as_para()).unwrap();
    assert_eq!(para.text.get(start.off..end.off), Some("2two"), "the tracked deletion and the insertion");
    // The call that only selected text changed nothing: it has no change to show.
    h.get_by_label("select_text").click();
    h.run_steps(4);
    assert_eq!(h.query_all_by_label("Show the change").count(), 1);
}

/// `assist.showChange` from a script: the version a tool call made, selected; a version the
/// room doesn't have says so, and without a shared document there is nothing to show.
#[test]
fn show_change_selects_what_a_tool_call_changed() {
    let mut a = window("one two three");
    assert!(a.execute("assist.showChange", json!({"version": 1})).unwrap_err().contains("isn't the one shown"));
    a.assist_with(Box::new(Fake::new().then(rewrite_two())));
    ask(&mut a, "Use digits", None);
    let versions: Vec<u64> = a
        .conversation()
        .unwrap()
        .transcript()
        .entries()
        .filter_map(|e| match e {
            Entry::Tool(call) => call.changes.map(|c| c.last),
            _ => None,
        })
        .collect();
    assert_eq!(versions.len(), 1, "only type_text changed the document");
    let r = a.execute("assist.showChange", json!({"version": versions[0]})).unwrap();
    assert_eq!(r, json!({"version": versions[0], "from": versions[0], "shown": true}));
    let (start, end) = a.session.sel.ordered();
    assert_eq!(a.session.doc.body.first().and_then(|b| b.as_para()).and_then(|p| p.text.get(start.off..end.off)), Some("2two"));
    assert!(a.assist.notice.is_none());
    let e = a.execute("assist.showChange", json!({"version": 9999})).unwrap_err();
    assert!(e.contains("isn't in the document"), "{e}");
    assert_eq!(a.assist.notice.as_deref(), Some(e.as_str()), "the pane says why");
    for bad in [json!({"version": 0}), json!({"version": "1"}), json!({})] {
        assert!(a.execute("assist.showChange", bad).is_err());
    }
}

/// What Show the change sends for each of the conversation's tool calls that changed the
/// document, in order.
fn shows(a: &WordApp) -> Vec<Value> {
    a.conversation()
        .unwrap()
        .transcript()
        .entries()
        .filter_map(|e| match e {
            Entry::Tool(call) => call.changes.map(|c| json!({"version": c.last, "from": c.first})),
            _ => None,
        })
        .collect()
}

/// The person's edits reach the room, and the window has all it took.
fn shared(a: &mut WordApp) {
    let ctx = egui::Context::default();
    for _ in 0..4000 {
        a.sync_share(&ctx);
        super::tick(a);
        if a.share_settled() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the window doesn't catch up");
}

/// A click or key the control channel made (MCP's `click`) isn't the person's: the turn it
/// starts is a program's, whose requests are answered per the setting (refused), never shown
/// to the person as their own.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_faked_click_starts_a_program_s_turn() {
    use egui_kittest::kittest::Queryable;
    let mut a = window("one");
    let fake = Fake::new().then(vec![Step::Ask("Run a command?".into())]);
    let seen = fake.seen();
    a.assist_with(Box::new(fake));
    a.session.view.assistant_pane = true;
    a.assist.prompt = "Go".into();
    let mut h = harness(a);
    let frame = |h: &mut egui_kittest::Harness<'_, WordApp>| {
        let ctx = h.ctx.clone();
        let mut raw = std::mem::take(h.input_mut());
        h.state_mut().raw_input_hook(&ctx, &mut raw);
        *h.input_mut() = raw;
        h.step();
    };
    let pos = h.get_by_label("Send").rect().center();
    let (req, _rx) = crate::control::ControlRequest::new("ui.click", json!({"x": pos.x, "y": pos.y}));
    let ctx = h.ctx.clone();
    assert!(matches!(crate::control::handle(h.state_mut(), &ctx, &req), crate::control::Outcome::Done(_)));
    for _ in 0..4000 {
        frame(&mut h);
        let a = h.state();
        if waiting(a) || (!answers(&seen).is_empty() && idle(a)) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!waiting(h.state()), "a faked click doesn't ask the person");
    assert_eq!(answers(&seen), [picked("reject")], "it is a program's turn, refused per the setting");
    assert!(!h.state().assist.by_person);
}

/// A backend whose turn asks, waits for the test to go on, then asks again: the answers it got.
#[cfg(not(target_arch = "wasm32"))]
struct Gated {
    go: std::sync::mpsc::Receiver<()>,
    answers: std::sync::Arc<std::sync::Mutex<Vec<Answer>>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl wordcraft_assist::Backend for Gated {
    fn info(&self) -> wordcraft_assist::BackendInfo {
        wordcraft_assist::BackendInfo { id: "gated".into(), label: "Gated".into(), author: "Gated".into(), in_process_tools: true }
    }

    fn turn(&mut self, _: &wordcraft_assist::Turn, ctx: &mut wordcraft_assist::TurnCtx<'_>) -> Result<StopReason, String> {
        use wordcraft_assist::{PermissionKind, PermissionOption, PermissionRequest};
        let options = || {
            [
                PermissionOption { id: "allow".into(), name: "Allow".into(), kind: PermissionKind::AllowOnce },
                PermissionOption { id: "reject".into(), name: "Reject".into(), kind: PermissionKind::RejectOnce },
            ]
        };
        let first = ctx.ask(PermissionRequest::bounded("First", options()));
        self.answers.lock().unwrap().push(first);
        let _ = self.go.recv_timeout(Duration::from_secs(30));
        let second = ctx.ask(PermissionRequest::bounded("Second", options()));
        self.answers.lock().unwrap().push(second);
        Ok(StopReason::EndTurn)
    }
}

/// Turning the setting off applies to a program's turn that is running: its next request is
/// refused, though its first was allowed.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn turning_the_setting_off_refuses_a_running_turn_s_next_request() {
    let mut a = window("one two three");
    a.session.prefs.assistant.allow_unattended = true;
    let (go, gate) = std::sync::mpsc::channel();
    let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    a.assist_with(Box::new(Gated { go: gate, answers: got.clone() }));
    a.execute("assist.ask", json!({"prompt": "Tidy every section"})).unwrap();
    for _ in 0..4000 {
        if !got.lock().unwrap().is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(*got.lock().unwrap(), [picked("allow")]);
    // The person unticks it in Options while the turn goes on.
    assert_eq!(a.execute("assist.allowUnattended", json!({"value": false})).unwrap(), json!({"value": false}));
    go.send(()).unwrap();
    settle(&mut a);
    assert_eq!(*got.lock().unwrap(), [picked("allow"), picked("reject")]);
}

/// A new conversation numbers its requests from 1 again: the pane, closed meanwhile, still
/// opens for its first, though it opened for the last conversation's first.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn the_pane_opens_for_a_new_conversation_s_first_request() {
    let mut app = window("one two three");
    app.assist_with(Box::new(Fake::new().then(vec![Step::Ask("First".into())])));
    let mut h = harness(app);
    h.state_mut().run("assist.ask", json!({"prompt": "One"})).unwrap();
    until(&mut h, waiting);
    let id = h.state().conversation().and_then(|c| c.pending_permission()).map(|r| r.id).unwrap();
    assert!(h.state_mut().conversation_mut().unwrap().answer(id, picked("allow")));
    until(&mut h, idle);
    h.state_mut().assist_with(Box::new(Fake::new().then(vec![Step::Ask("Second".into())])));
    h.state_mut().run("assist.ask", json!({"prompt": "Two"})).unwrap();
    h.state_mut().session.view.assistant_pane = false;
    until(&mut h, waiting);
    assert_eq!(h.state().conversation().and_then(|c| c.pending_permission()).map(|r| r.id), Some(id), "numbered from 1 again");
    assert!(h.state().session.view.assistant_pane, "the pane opens to ask");
}

/// Closing the pane while a request waits answers it cancelled: the turn doesn't wait, hidden,
/// for an answer no one can give.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn closing_the_pane_cancels_the_waiting_request() {
    let mut app = window("one two three");
    let fake = Fake::new().then(vec![Step::Ask("Delete the old notes".into()), Step::say("Deleted.")]);
    let seen = fake.seen();
    app.assist_with(Box::new(fake));
    let mut h = harness(app);
    h.state_mut().run("assist.ask", json!({"prompt": "Tidy up"})).unwrap();
    until(&mut h, waiting);
    h.state_mut().run("assist.pane", json!({"value": false})).unwrap();
    until(&mut h, idle);
    assert_eq!(answers(&seen), [Answer::Cancelled]);
    assert!(!h.state().session.view.assistant_pane, "it stays closed");
}

/// A call that made several changes (a batch, one command at a time) shows them all: Show the
/// change selects from the first to the last.
#[test]
fn show_change_selects_every_change_of_a_batch() {
    let mut a = window("one two three");
    let commands = json!({"commands": [
        {"command": "caret.docStart"},
        {"command": "text.insert", "params": {"text": "Intro. "}},
        {"command": "caret.docEnd"},
        {"command": "text.insert", "params": {"text": " The end."}},
    ]});
    a.assist_with(Box::new(Fake::new().then(vec![Step::tool("batch", commands)])));
    ask(&mut a, "Frame it", None);
    assert_eq!(accepted(&a), "Intro. one two three The end.");
    let shows = shows(&a);
    assert_eq!(shows.len(), 1, "one call");
    a.execute("assist.showChange", shows[0].clone()).unwrap();
    assert_eq!(a.session.selected_text(), "Intro. one two three The end.");
}

/// Show the change selects the text the change made, no more: not the paragraph a new one was
/// put after, and not what the person typed right before it later.
#[test]
fn show_change_selects_exactly_the_text_the_change_made() {
    let mut a = window("four");
    a.execute("caret.docEnd", json!({})).unwrap();
    a.assist_with(Box::new(Fake::new().then(vec![Step::tool("type_text", json!({"text": "\nSix\nSeven", "paragraphs": true}))])));
    ask(&mut a, "Go on", None);
    assert_eq!(accepted(&a), "four\nSix\nSeven");
    let show = shows(&a)[0].clone();
    a.execute("assist.showChange", show).unwrap();
    let (start, _) = a.session.sel.ordered();
    assert_eq!((start.path.0.clone(), start.off), (vec![0], 4), "from the end of the person's paragraph");
    assert_eq!(a.session.selected_text().trim_start(), "Six\nSeven");

    let mut a = window("Hello ");
    a.execute("caret.docEnd", json!({})).unwrap();
    a.assist_with(Box::new(Fake::new().then(vec![Step::tool("type_text", json!({"text": "World"}))])));
    ask(&mut a, "Greet", None);
    let show = shows(&a)[0].clone();
    // The person types right before it.
    let at = wordcraft_engine::cmd::pos_json(&wordcraft_doc::Pos::body(0, 6));
    a.execute("select.range", json!({"anchor": at, "focus": at})).unwrap();
    a.execute("text.insert", json!({"text": "big "})).unwrap();
    shared(&mut a);
    assert_eq!(accepted(&a), "Hello big World");
    a.execute("assist.showChange", show).unwrap();
    assert_eq!(a.session.selected_text(), "World");
}

/// A tool call's change is looked for in the conversation's shared document: once that closed,
/// the version means nothing in the next one shared.
#[test]
fn show_change_looks_in_the_conversation_s_own_room() {
    let mut a = window("one two three");
    a.assist_with(Box::new(Fake::new().then(rewrite_two())));
    ask(&mut a, "Use digits", None);
    let show = shows(&a)[0].clone();
    let version = show["version"].as_u64().unwrap();
    a.stop_sharing();
    super::tick(&mut a);
    a.share_document(None).unwrap();
    for _ in 0..version {
        a.execute("caret.docEnd", json!({})).unwrap();
        a.execute("text.insert", json!({"text": "!"})).unwrap();
        shared(&mut a);
    }
    assert!(a.shared_room().unwrap().version() >= version);
    assert!(a.execute("assist.showChange", show).is_err(), "nothing of the person's is shown as the assistant's");
    assert_eq!(a.session.sel.ordered().0, a.session.sel.ordered().1);
}

/// A change whose text is gone (the person rejected it) isn't shown as a caret where it was:
/// the pane says it isn't there any more.
#[test]
fn show_change_says_when_the_text_is_gone() {
    let mut a = window("one two");
    a.execute("caret.docEnd", json!({})).unwrap();
    a.assist_with(Box::new(Fake::new().then(vec![Step::tool("type_text", json!({"text": " three"}))])));
    ask(&mut a, "Count on", None);
    let show = shows(&a)[0].clone();
    a.execute("review.rejectAll", json!({})).unwrap();
    shared(&mut a);
    assert_eq!(accepted(&a), "one two");
    let e = a.execute("assist.showChange", show).unwrap_err();
    assert!(e.contains("isn't in the document any more"), "{e}");
}

/// A table a call put in is still selected whole after the person adds a row to it, not a
/// caret in its first cell.
#[test]
fn show_change_selects_a_table_whose_rows_changed() {
    let mut a = window("one");
    a.execute("caret.docEnd", json!({})).unwrap();
    a.execute("text.insert", json!({"text": "\n"})).unwrap();
    let table = json!({"command": "insert.table", "params": {"rows": 2, "cols": 2}});
    a.assist_with(Box::new(Fake::new().then(vec![Step::tool("execute", table)])));
    ask(&mut a, "A table", Some("editor"));
    let show = shows(&a)[0].clone();
    let cells: Vec<_> = a.session.doc.para_paths(StoryRef::Body).into_iter().filter(|p| p.0.len() > 1).collect();
    assert_eq!(cells.len(), 4, "a 2 by 2 table");
    let first = wordcraft_engine::cmd::pos_json(&wordcraft_doc::Pos { story: StoryRef::Body, path: cells[0].clone(), off: 0 });
    a.execute("select.range", json!({"anchor": first, "focus": first})).unwrap();
    a.execute("table.insertRowBelow", json!({})).unwrap();
    shared(&mut a);
    a.execute("assist.showChange", show).unwrap();
    let (start, end) = a.session.sel.ordered();
    assert_ne!(start, end, "not a caret");
    assert_eq!(start.path, cells[0], "from the first cell");
    assert!(end.path.0.len() > 1 && end.path.0[0] == cells[0].0[0] && end.path != cells[0], "to the table's last cell: {end:?}");
}

/// The web app (#26): the pane and the commands are there, but no assistant is offered, and the
/// pane, the writing actions and File › Options › Assistant say it takes the desktop app.
#[test]
fn on_the_web_the_pane_says_the_assistant_needs_the_desktop_app() {
    use egui_kittest::kittest::Queryable;
    use wordcraft_engine::cmd::assist::NEEDS_DESKTOP;
    let mut a = window("one two three");
    a.run("assist.pane", json!({"value": true})).unwrap();
    let mut h = harness(a);
    assert!(h.query_by_label("Role:").is_some(), "the desktop pane has a role to choose");
    h.state_mut().assist_as_web();
    h.run_steps(3);
    assert!(h.state().assist.offers.is_empty(), "no backend on the web");
    // Asking again what is offered (the pane does now and then) still finds none.
    super::refresh_offers(h.state_mut());
    h.run_steps(3);
    assert!(h.state().assist.offers.is_empty());
    assert!(h.query_by_label("The assistant needs the desktop app").is_some(), "the pane says so");
    assert!(h.query_by_label_contains("a web page can't keep a sign-in safe or start a program").is_some());
    assert!(h.query_by_label("Role:").is_none(), "no role to choose for no assistant");
    // The prompt and the writing actions are off, and say why.
    h.state_mut().run("select.all", json!({})).unwrap();
    h.run_steps(2);
    for id in ["assist.ask", "assist.draft", "assist.rewrite", "assist.shorten", "assist.expand", "assist.proofread", "assist.summarize"] {
        assert_eq!(h.state().session.why_disabled(id), Some(NEEDS_DESKTOP), "{id}");
    }
    let e = h.state_mut().run("assist.ask", json!({"prompt": "Hi"})).unwrap_err();
    assert!(e.contains("desktop app"), "{e}");
    assert!(h.state().conversation().is_none(), "nothing started");
    // And File › Options › Assistant sets no agent up.
    h.state_mut().run("file.options", json!({"pane": "assistant"})).unwrap();
    h.run_steps(3);
    assert!(h.query_by_label("Agents are programs WordCraft starts on your computer, so they are set up in the desktop app.").is_some());
    assert!(h.query_by_label_contains("Claude Code").is_none());
}

/// File › Options › Assistant on the web (#26): no role or unattended permission to set for an
/// assistant the web can't run, and nothing that points at a role the pane doesn't offer.
#[test]
fn on_the_web_options_sets_no_role_or_permission() {
    use egui_kittest::kittest::Queryable;
    let mut a = window("one two three");
    a.run("file.options", json!({"pane": "assistant"})).unwrap();
    let mut h = harness(a);
    h.run_steps(3);
    assert!(h.query_by_label("Assistants join as:").is_some(), "the desktop page sets the role");
    assert!(h.query_by_label("Allow what agents ask when no one can answer").is_some());
    h.state_mut().assist_as_web();
    h.run_steps(3);
    assert!(h.query_by_label("Assistants join as:").is_none(), "no role on the web");
    assert!(h.query_by_label_contains("Each conversation in the Assistant pane may choose another").is_none());
    assert!(h.query_by_label("Allow what agents ask when no one can answer").is_none());
    assert!(h.query_by_label("The assistant needs the desktop app").is_some(), "it says why");
    assert!(h.query_by_label("Agents are programs WordCraft starts on your computer, so they are set up in the desktop app.").is_some());
}

/// A scripted assistant given before the window offers what the web does (#26) doesn't survive
/// it, and one given after isn't run: the web window runs no assistant, and asking starts nothing.
#[test]
fn the_web_runs_no_scripted_assistant() {
    use wordcraft_engine::cmd::assist::NEEDS_DESKTOP;
    for given_first in [true, false] {
        let mut a = window("one two three");
        super::tick(&mut a);
        if given_first {
            a.assist_with(Box::new(Fake::new().then(vec![Step::say("Hi.")])));
            assert_eq!(a.session.why_disabled("assist.ask"), None, "the scripted assistant is ready on the desktop");
            a.assist_as_web();
        } else {
            a.assist_as_web();
            a.assist_with(Box::new(Fake::new().then(vec![Step::say("Hi.")])));
        }
        super::tick(&mut a);
        assert!(a.assist.offers.is_empty());
        assert_eq!(a.session.why_disabled("assist.ask"), Some(NEEDS_DESKTOP), "given first: {given_first}");
        let e = a.run("assist.ask", json!({"prompt": "Hi"})).unwrap_err();
        assert!(e.contains("desktop app"), "{e}");
        assert!(a.conversation().is_none(), "nothing started");
        assert!(a.shared_room().is_none(), "nothing shared");
    }
}

/// The web window never asks the backends for the scripted one (#26), though a test build
/// offers it on the desktop.
#[test]
fn on_the_web_the_window_never_asks_for_the_fake() {
    let mut a = window("one");
    assert!(super::host(&a).fake, "test builds offer the fake on the desktop");
    a.assist_as_web();
    assert!(!super::host(&a).fake);
    assert!(wordcraft_assist::offers::offers_in(&super::host(&a), super::desktop(&a)).is_empty());
}

/// #27: the window never waits for an assistant. The pane's assistant holds its turn after its
/// edit, another assistant in the shared document holds its own, and the person types into the
/// page with the keyboard meanwhile, every frame timed; the pane's Stop and the other's don't
/// wait either. Each edit is in, in its author's name; the pane accepts its assistant's, Review
/// rejects the other's, and the person's typing stays.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn the_window_never_waits_for_its_assistants() {
    use std::time::Instant;

    use egui_kittest::kittest::Queryable;
    use wordcraft_assist::{Assistant, Backend, BackendInfo, Options, Seat, Turn, TurnCtx};
    use wordcraft_engine::cmd::review::changes_by;

    /// A frame that waited for an assistant would take as long as it holds (until it's stopped),
    /// or as long as it takes to stop.
    const BUDGET: Duration = Duration::from_secs(1);
    /// How long each assistant takes to stop: well over a frame's budget.
    const STOPPING: Duration = Duration::from_secs(3);

    /// An assistant slow to stop, as an agent in the middle of a tool call is: its fake's turn,
    /// then, once stopped, [`STOPPING`] more before the turn ends.
    struct SlowToStop(Fake);

    impl Backend for SlowToStop {
        fn info(&self) -> BackendInfo {
            self.0.info()
        }

        fn begin(&mut self, seat: &Seat) -> Result<(), String> {
            self.0.begin(seat)
        }

        fn turn(&mut self, turn: &Turn, ctx: &mut TurnCtx<'_>) -> Result<StopReason, String> {
            let stop = self.0.turn(turn, ctx);
            if ctx.is_cancelled() {
                std::thread::sleep(STOPPING);
            }
            stop
        }
    }

    let mut app = window("one two three");
    let pane = vec![Step::tool("select_text", json!({"text": "two"})), Step::tool("type_text", json!({"text": "2"})), Step::Hold];
    app.assist_with(Box::new(SlowToStop(Fake::named("Pane Assistant").then(pane))));
    let mut h = harness(app);
    h.state_mut().execute("assist.ask", json!({"prompt": "Use digits"})).unwrap();
    let room = h.state().shared_room().unwrap();
    let steps = vec![Step::tool("execute", json!({"command": "caret.docEnd"})), Step::tool("type_text", json!({"text": " four"})), Step::Hold];
    let mut other = Assistant::start(&room, Box::new(SlowToStop(Fake::named("Other Assistant").then(steps))), Options::default()).unwrap();
    other.ask("Count on", None).unwrap();
    // The page has the keyboard, the caret at the start.
    h.state_mut().execute("caret.docStart", json!({})).unwrap();
    h.state_mut().canvas.want_focus = true;
    h.run_steps(2);

    let mut keys = "Zero, ".chars().peekable();
    let mut frames = 0usize;
    let mut frame = |h: &mut egui_kittest::Harness<'static, WordApp>, other: &mut Assistant| {
        if frames.is_multiple_of(4)
            && let Some(key) = keys.next()
        {
            h.event(egui::Event::Text(key.to_string()));
        }
        let started = Instant::now();
        h.step();
        other.poll();
        assert!(started.elapsed() < BUDGET, "frame {frames} took {:?}: the window waited", started.elapsed());
        frames += 1;
        std::thread::sleep(Duration::from_millis(5));
    };
    // Frames, at least two seconds of them, until both assistants made their edits and hold, and
    // the person typed everything.
    let held_from = Instant::now();
    let deadline = held_from + Duration::from_secs(60);
    loop {
        frame(&mut h, &mut other);
        let s = &h.state().session;
        let edited = changes_by(s, "Pane Assistant") >= 2 && changes_by(s, "Other Assistant") >= 1;
        if edited && held_from.elapsed() >= Duration::from_secs(2) && s.doc.plain_text(StoryRef::Body).starts_with("Zero, ") {
            break;
        }
        assert!(Instant::now() < deadline, "the edits never came in");
    }
    assert!(h.state().conversation().is_some_and(|c| c.is_busy()) && other.is_busy(), "both hold their turns");
    // Stop in the pane, then the other's Stop: neither waits for its assistant, which takes
    // seconds to stop; frames go on meanwhile, and both turns end.
    let stopped_at = Instant::now();
    h.get_by_label("Stop").click();
    let started = Instant::now();
    other.cancel();
    assert!(started.elapsed() < BUDGET);
    while h.state().conversation().is_some_and(|c| c.is_busy()) || other.is_busy() || !h.state().share_settled() {
        frame(&mut h, &mut other);
        assert!(Instant::now() < deadline, "the turns never ended");
    }
    assert!(stopped_at.elapsed() >= STOPPING, "the turns ended {:?} after Stop", stopped_at.elapsed());
    let stopped = |a: &Assistant| a.transcript().entries().any(|e| matches!(e, Entry::Stopped { stop: StopReason::Cancelled }));
    assert!(stopped(h.state().conversation().unwrap()) && stopped(&other));
    assert_eq!(accepted(h.state()), "Zero, one 2 three four");
    assert_eq!(changes_by(&h.state().session, "Minoru"), 0, "the person's typing isn't tracked");
    assert_eq!(h.state().session.doc, room.document().1);

    // The pane's Accept all takes its assistant's changes; Review's Reject All by author the
    // other's.
    h.get_by_label("Accept all").click();
    h.run_steps(3);
    h.state_mut().execute("review.rejectAll", json!({"author": "Other Assistant"})).unwrap();
    while !h.state().share_settled() {
        frame(&mut h, &mut other);
        assert!(Instant::now() < deadline, "the window never caught up");
    }
    let s = &h.state().session;
    assert_eq!(s.doc.final_text(StoryRef::Body), "Zero, one 2 three");
    assert_eq!((changes_by(s, "Pane Assistant"), changes_by(s, "Other Assistant")), (0, 0));
    assert_eq!(s.doc, room.document().1);
    let started = Instant::now();
    other.end();
    assert!(started.elapsed() < BUDGET);
}

/// File › Share doesn't change the assistant's role (#70, Codex): its conversation was started
/// and told with that role, and asking for the role again reuses it, so a suggester changed to an
/// editor there would edit untracked when asked as a suggester. The Assistant pane chooses it.
#[test]
fn the_share_page_leaves_the_assistant_s_role_to_the_assistant_pane() {
    let mut a = window("one two three");
    a.execute("assist.backend", json!({"id": wordcraft_assist::FAKE_ID})).unwrap();
    let first = ask(&mut a, "Hello", Some("suggester"));
    let name = first["assistant"].as_str().unwrap().to_string();
    let e = a.execute("room.role", json!({"name": name.to_uppercase(), "role": "editor"})).unwrap_err();
    assert!(e.contains("Assistant pane"), "{e}");
    let state = a.share_state();
    let row = crate::share_page::rows(&state).into_iter().find(|r| r.name == name).unwrap();
    assert_eq!(row.role, wordcraft_engine::room::Role::Suggester, "its role in the room is unchanged");
    let again = ask(&mut a, "Again", Some("suggester"));
    assert_eq!((again["role"].as_str(), again["tracked"].as_bool()), (Some("suggester"), Some(true)));
    // Another participant's role still changes.
    a.execute("room.invite", json!({"name": "Ana", "role": "viewer"})).unwrap();
    assert_eq!(a.execute("room.role", json!({"name": "Ana", "role": "editor"})).unwrap()["changed"], true);
}

/// A window with the pane open and a turn of the scripted assistant that says `answer`, finished.
#[cfg(not(target_arch = "wasm32"))]
fn answered(answer: &str) -> egui_kittest::Harness<'static, WordApp> {
    let mut app = window("one two three");
    app.assist_with(Box::new(Fake::new().then(vec![Step::say(answer)])));
    let mut h = harness(app);
    h.state_mut().run("assist.ask", json!({"prompt": "Answer"})).unwrap();
    for _ in 0..4000 {
        if !h.state().conversation().is_some_and(|c| c.is_busy()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
        h.run_steps(1);
    }
    h.run_steps(2);
    h
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn an_answer_shows_formatted_and_not_its_markers() {
    use egui_kittest::kittest::Queryable;
    let h = answered("## Findings\n\n- **first** point\n\n> a quote\n\n```\nsome code\n```");
    for shown in ["Findings", "first", " point", "a quote", "some code"] {
        assert!(h.query_by_label(shown).is_some(), "{shown}");
    }
    for marker in ["## Findings", "**first**", "> a quote", "```"] {
        assert!(h.query_by_label_contains(marker).is_none(), "{marker}");
    }
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_link_in_an_answer_says_where_it_goes_and_opens_only_on_the_person_s_click() {
    use egui_kittest::kittest::Queryable;
    let mut h = answered("See [the bank](https://example.com/not-the-bank) or [run this](file:///bin/sh).");
    assert!(h.query_by_label(" (https://example.com/not-the-bank)").is_some(), "the address shows beside the words");
    assert!(h.query_by_label(" (file:///bin/sh)").is_some());
    // What isn't a web address isn't a link.
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Link, "run this").is_none());

    // What the window asked to open, as its frames go.
    let frame = |h: &mut egui_kittest::Harness<'_, WordApp>| {
        let ctx = h.ctx.clone();
        let mut raw = std::mem::take(h.input_mut());
        h.state_mut().raw_input_hook(&ctx, &mut raw);
        *h.input_mut() = raw;
        h.step();
        h.output()
            .platform_output
            .commands
            .iter()
            .any(|c| matches!(c, egui::OutputCommand::OpenUrl(u) if u.url == "https://example.com/not-the-bank"))
    };
    let pos = h.get_by_role_and_label(egui::accesskit::Role::Link, "the bank").rect().center();
    let (req, _rx) = crate::control::ControlRequest::new("ui.click", json!({"x": pos.x, "y": pos.y}));
    let ctx = h.ctx.clone();
    assert!(matches!(crate::control::handle(h.state_mut(), &ctx, &req), crate::control::Outcome::Done(_)));
    assert!(!(0..20).any(|_| frame(&mut h)), "a click the control channel made opens nothing");
    assert!(h.state().assist.notice.as_deref().is_some_and(|n| n.contains("Only you")), "{:?}", h.state().assist.notice);

    h.input_mut().events.push(egui::Event::PointerMoved(pos));
    let mut opened = frame(&mut h);
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
        opened |= frame(&mut h);
    }
    opened |= (0..3).any(|_| frame(&mut h));
    assert!(opened, "the person's click opens it");
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_huge_answer_draws_a_bounded_number_of_widgets_within_a_frame_budget() {
    use egui_kittest::kittest::Queryable;
    // A debug build's budget on a busy machine, as `the_window_never_waits_for_its_assistants`.
    // What keeps a frame short is the number of widgets: a debug frame takes about 18 ms with the
    // limit and 80 ms without, too close to time on a shared machine.
    const BUDGET: Duration = Duration::from_secs(1);
    let mut h = answered(&"- a\n".repeat(64 << 10));
    let at = std::time::Instant::now();
    h.run_steps(1);
    assert!(at.elapsed() < BUDGET, "a frame took {:?}", at.elapsed());
    let items = h.query_all_by_label("a").count();
    assert!((1..=wordcraft_assist::answer::MAX_BLOCKS).contains(&items), "{items} widgets");
}

/// A window with the pane open and a scripted turn holding on after `steps`.
#[cfg(not(target_arch = "wasm32"))]
fn running(mut steps: Vec<Step>) -> egui_kittest::Harness<'static, WordApp> {
    use wordcraft_assist::Entry;
    steps.push(Step::say("End of the script."));
    steps.push(Step::Hold);
    let mut app = window("one two three\nsecond paragraph");
    app.assist_with(Box::new(Fake::new().then(steps)));
    let mut h = harness(app);
    h.state_mut().run("assist.ask", json!({"prompt": "Do the thing"})).unwrap();
    for _ in 0..4000 {
        h.run_steps(1);
        let said = h
            .state()
            .conversation()
            .is_some_and(|c| c.transcript().entries().any(|e| matches!(e, Entry::Answer { text } if text == "End of the script.")));
        if said {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(2);
    h
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn details_shows_steps_and_properties_during_a_turn_and_answers_the_same_to_a_script() {
    use egui_kittest::kittest::Queryable;
    let usage = wordcraft_assist::Usage { input_tokens: Some(1200), output_tokens: Some(80), ..Default::default() };
    let mut h = running(vec![Step::tool("get_text", json!({})), Step::Emit(wordcraft_assist::Event::Usage(usage))]);
    assert!(h.query_by_label("End of the script.").is_some(), "the conversation shows first");
    assert!(h.query_by_label("Do the thing").is_some());

    h.get_by_label("Details").click();
    h.run_steps(2);
    assert!(h.query_by_label("End of the script.").is_none(), "the view changed");
    assert!(h.query_all_by_label("Do the thing").count() >= 1, "the turn names its steps");
    assert!(h.query_all_by_label("get_text").count() >= 1, "{:?}", h.state().conversation().map(|c| c.details()));
    for row in ["Scripted assistant (for testing)", "Suggester", "Tracked, in its name", "scripted", "1200 in, 80 out"] {
        assert!(h.query_by_label(row).is_some(), "{row}");
    }

    let answer = h.state_mut().execute("assist.details", json!({})).unwrap();
    assert_eq!(answer["properties"]["role"], "suggester");
    assert_eq!(answer["properties"]["tracked"], true);
    assert_eq!(answer["turns"][0]["asked"], "Do the thing");
    assert_eq!(answer["turns"][0]["steps"][0]["title"], "get_text");
    assert_eq!(answer["usage"]["inputTokens"], 1200);
    assert_eq!(answer["model"], "scripted");

    h.get_by_label("Conversation").click();
    h.run_steps(2);
    assert!(h.query_by_label("End of the script.").is_some());
    h.state_mut().run("assist.cancel", json!({})).unwrap();
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_section_of_details_folds() {
    use egui_kittest::kittest::Queryable;
    let mut h = running(vec![Step::tool("get_text", json!({}))]);
    h.get_by_label("Details").click();
    h.run_steps(2);
    let rows = h.query_all_by_label("get_text").count();
    assert!(rows > 0);
    h.get_by_label("Steps").click();
    h.run_steps(3);
    assert_eq!(h.query_all_by_label("get_text").count(), rows - 1, "folded");
    h.state_mut().run("assist.cancel", json!({})).unwrap();
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn details_gives_way_to_a_question_the_turn_waits_on() {
    use egui_kittest::kittest::Queryable;
    let mut app = window("one");
    app.assist_with(Box::new(Fake::new().then(vec![Step::say("Before."), Step::Hold])));
    let mut h = harness(app);
    h.state_mut().run("assist.ask", json!({"prompt": "Go"})).unwrap();
    h.run_steps(3);
    h.get_by_label("Details").click();
    h.run_steps(2);
    assert_eq!(h.state().assist.view, crate::assist::PaneView::Details);
    h.state_mut().run("assist.cancel", json!({})).unwrap();
    settle(h.state_mut());

    h.state_mut().assist_with(Box::new(Fake::new().then(vec![Step::Ask("Run a command?".into())])));
    h.state_mut().run("assist.ask", json!({"prompt": "Ask"})).unwrap();
    for _ in 0..4000 {
        h.run_steps(1);
        if h.state().conversation().is_some_and(|c| c.pending_permission().is_some()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(3);
    assert_eq!(h.state().assist.view, crate::assist::PaneView::Conversation);
    assert!(h.query_by_label("Run a command?").is_some(), "the person sees what the turn waits for");
    h.state_mut().run("assist.cancel", json!({})).unwrap();
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn usage_arriving_after_a_piece_of_an_answer_leaves_it_arriving() {
    use egui_kittest::kittest::Queryable;
    use wordcraft_assist::Entry;
    let usage = wordcraft_assist::Event::Usage(wordcraft_assist::Usage { context_used: Some(1), ..Default::default() });
    let mut app = window("one");
    app.assist_with(Box::new(Fake::new().then(vec![Step::say("**half written"), Step::Emit(usage), Step::Hold])));
    let mut h = harness(app);
    h.state_mut().run("assist.ask", json!({"prompt": "Go"})).unwrap();
    for _ in 0..4000 {
        h.run_steps(1);
        if h.state().conversation().is_some_and(|c| c.transcript().entries().any(|e| matches!(e, Entry::Usage(_)))) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(2);
    assert!(h.query_by_label("half written").is_some(), "its open marker is still closed for it");
    h.state_mut().run("assist.cancel", json!({})).unwrap();
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn sources_say_what_a_turn_was_given_and_what_it_read() {
    use egui_kittest::kittest::Queryable;
    use wordcraft_assist::{Event, Source, SourceKind, ToolCall, ToolStatus};
    let source = |kind, title: &str, target: &str| Source { kind, title: title.into(), target: Some(target.into()) };
    let agent_read = ToolCall {
        id: "a".into(),
        title: "Agent read".into(),
        kind: "read".into(),
        status: ToolStatus::Completed,
        detail: String::new(),
        changes: None,
        sources: vec![
            source(SourceKind::File, "/home/a/notes.md:3", "/home/a/notes.md"),
            source(SourceKind::Web, "https://example.com/page", "https://example.com/page"),
            source(SourceKind::Web, "file:///etc/passwd", "file:///etc/passwd"),
        ],
    };
    let mut h = running(vec![
        Step::tool("get_text", json!({})),
        Step::tool("execute", json!({"command": "document.paragraph", "params": {"path": [1]}})),
        Step::Emit(Event::Tool(agent_read)),
    ]);
    h.get_by_label("Details").click();
    h.run_steps(2);
    h.get_by_label("Steps").click();
    h.run_steps(2);
    assert!(h.query_by_label("/home/a/notes.md:3").is_some(), "a file shows its path");
    assert!(h.query_by_label_contains("Given: where the caret is").is_some());
    assert!(h.query_by_label("get_text").is_some(), "WordCraft's own read");
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Link, "https://example.com/page").is_some(), "a web address is a link");
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Link, "file:///etc/passwd").is_none(), "only a web address is a link");
    let paragraph = h.get_by_role_and_label(egui::accesskit::Role::Link, "execute document.paragraph");
    paragraph.click();
    h.run_steps(2);
    let selected = h.state().session.doc.copy_range(&h.state().session.sel.anchor, &h.state().session.sel.focus).plain_text();
    assert_eq!(selected.trim_end(), "second paragraph", "a paragraph's row selects it, by its id");

    // A row for a paragraph that went says so: an old path could go somewhere else.
    h.state_mut().execute("select.all", json!({})).unwrap();
    h.state_mut().execute("text.insert", json!({"text": "Other"})).unwrap();
    h.run_steps(2);
    h.get_by_role_and_label(egui::accesskit::Role::Link, "execute document.paragraph").click();
    h.run_steps(2);
    assert!(h.state().assist.notice.as_deref().is_some_and(|n| n.contains("isn't in the document")), "{:?}", h.state().assist.notice);
    h.state_mut().run("assist.cancel", json!({})).unwrap();
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_turn_records_what_it_was_given_as_the_model_got_it() {
    use wordcraft_assist::Entry;
    let mut a = window("one two three");
    let fake = Fake::new().then(vec![Step::say("Hi")]);
    let seen = fake.seen();
    a.assist_with(Box::new(fake));
    a.run("select.text", json!({"text": "two"})).unwrap();
    ask(&mut a, "Go", None);
    settle(&mut a);
    let sent = a
        .conversation()
        .unwrap()
        .transcript()
        .entries()
        .find_map(|e| if let Entry::Context(s) = e { Some(s.clone()) } else { None })
        .expect("the turn recorded it");
    assert_eq!(sent.selection_bytes, 3);
    let turns = seen.lock().unwrap();
    let context = turns.turns[0].context.as_deref().unwrap_or_default();
    assert_eq!(sent.bytes as usize, context.len(), "{context}");
}

/// Replace "two" and "three", one tool call each, then hold the turn.
#[cfg(not(target_arch = "wasm32"))]
fn rewrite_both() -> Vec<Step> {
    vec![
        Step::tool("select_text", json!({"text": "two"})),
        Step::tool("type_text", json!({"text": "2"})),
        Step::tool("select_text", json!({"text": "three"})),
        Step::tool("type_text", json!({"text": "3"})),
    ]
}

/// Frames until the window has what the room took.
#[cfg(not(target_arch = "wasm32"))]
fn caught_up(h: &mut egui_kittest::Harness<'static, WordApp>) {
    for _ in 0..4000 {
        h.run_steps(1);
        if h.state().share_settled() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(3);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn accepting_a_row_of_output_changes_only_that_change() {
    use egui_kittest::kittest::Queryable;
    let mut h = running(rewrite_both());
    caught_up(&mut h);
    h.get_by_label("Details").click();
    h.run_steps(2);
    // The pane is short: the rows to click are in view with the sections above them folded.
    h.get_by_label("Steps").click();
    h.run_steps(2);
    h.get_by_label("Sources").click();
    h.run_steps(2);
    let rows = |h: &egui_kittest::Harness<'static, WordApp>| wordcraft_engine::cmd::review::change_rows(&h.state().session, "Test Assistant", 16).0;
    assert_eq!(rows(&h).len(), 4, "{:?}", rows(&h));
    assert!(h.query_by_label("3").is_some() && h.query_by_label("three").is_some());
    // Newest first: "three" and its Accept are the first row.
    h.get_all_by_label("Accept").next().unwrap().click();
    h.run_steps(3);
    let left: Vec<String> = rows(&h).into_iter().map(|c| c.text).collect();
    assert_eq!(left, ["2", "two", "3"], "only that change");
    assert!(h.query_by_label("Accepted").is_some(), "the row says how it ended");

    // Reject the next one, its deletion: only that one too.
    h.get_all_by_label("Reject").next().unwrap().click();
    h.run_steps(3);
    let left: Vec<String> = rows(&h).into_iter().map(|c| c.text).collect();
    assert_eq!(left, ["2", "two"]);
    assert!(h.query_by_label("Rejected").is_some());
    h.state_mut().run("assist.cancel", json!({})).unwrap();
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_change_another_participant_resolved_updates_its_row() {
    use egui_kittest::kittest::Queryable;
    use wordcraft_engine::room::Role;
    let mut h = running(rewrite_both());
    caught_up(&mut h);
    h.get_by_label("Details").click();
    h.run_steps(2);
    assert_eq!(h.query_all_by_label("Accept").count(), 4);
    let room = h.state().shared_room().expect("the document is shared");
    let (_, key) = room.invite_numbered("Colleague", Role::Editor).unwrap();
    let mut colleague = room.join_with(&key).unwrap();
    colleague.run("select.text", &json!({"text": "2"})).unwrap();
    colleague.run("review.accept", &json!({})).unwrap();
    colleague.send();
    caught_up(&mut h);
    assert_eq!(h.query_all_by_label("Accept").count(), 3);
    assert!(h.query_by_label("Accepted").is_some());
    h.state_mut().run("assist.cancel", json!({})).unwrap();
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn accept_all_stays_on_the_conversation() {
    use egui_kittest::kittest::Queryable;
    let mut h = running(rewrite_both());
    caught_up(&mut h);
    assert!(h.query_by_label("Accept all").is_some());
    h.get_by_label("Details").click();
    h.run_steps(2);
    assert!(h.query_by_label("Accept all").is_some(), "it stays under both views");
    h.state_mut().run("assist.cancel", json!({})).unwrap();
}

/// A temporary file with `text`, and the folder that holds it.
#[cfg(not(target_arch = "wasm32"))]
fn source_file(name: &str, text: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("wordcraft-source-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    (dir, path)
}

/// Frames until no source is being read.
#[cfg(not(target_arch = "wasm32"))]
fn sources_read(a: &mut WordApp) -> Value {
    for _ in 0..4000 {
        super::tick(a);
        let now = a.execute("assist.knowledge", json!({})).unwrap();
        if now["pending"] == 0 {
            return now;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the source isn't read");
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_source_over_the_limit_is_cut_says_so_and_reaches_the_turn_until_removed() {
    use wordcraft_assist::Entry;
    let (dir, path) = source_file("big.txt", &"word ".repeat(20 << 10));
    let mut a = window("one");
    let fake = Fake::new().then(vec![Step::say("A")]).then(vec![Step::say("B")]);
    let seen = fake.seen();
    a.assist_with(Box::new(fake));
    a.execute("assist.knowledge", json!({"add": path.to_string_lossy()})).unwrap();
    let now = sources_read(&mut a);
    assert_eq!(now["sources"][0]["name"], "big.txt");
    assert_eq!(now["sources"][0]["cut"], true, "{now}");
    assert_eq!(now["sources"][0]["bytes"], 64 << 10, "{now}");

    ask(&mut a, "Use it", None);
    settle(&mut a);
    let context = |a: &WordApp| {
        a.conversation().unwrap().transcript().entries().rev().find_map(|e| if let Entry::Context(s) = e { Some(s.clone()) } else { None }).unwrap()
    };
    assert_eq!(context(&a).knowledge[0].name, "big.txt");
    assert!(context(&a).knowledge[0].cut);
    assert_eq!(seen.lock().unwrap().turns[0].knowledge[0].text.len(), 64 << 10);

    a.execute("assist.knowledge", json!({"remove": 0})).unwrap();
    ask(&mut a, "Without it", None);
    settle(&mut a);
    assert!(context(&a).knowledge.is_empty());
    assert!(seen.lock().unwrap().turns[1].knowledge.is_empty(), "removing a source stops it reaching the next turn");
    assert!(a.execute("assist.knowledge", json!({"remove": 5})).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn sources_hold_to_their_number_and_a_file_that_can_t_be_read_says_why() {
    let (dir, path) = source_file("small.txt", "words");
    let mut a = window("one");
    a.assist_with(Box::new(Fake::new()));
    a.execute("assist.knowledge", json!({"add": dir.join("missing.txt").to_string_lossy()})).unwrap();
    let now = sources_read(&mut a);
    assert!(now["error"].as_str().unwrap_or_default().contains("missing.txt"), "{now}");
    for _ in 0..12 {
        a.execute("assist.knowledge", json!({"add": path.to_string_lossy()})).unwrap();
    }
    let now = sources_read(&mut a);
    assert_eq!(now["sources"].as_array().unwrap().len(), wordcraft_assist::knowledge::MAX_SOURCES);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_script_s_path_is_held_to_the_file_rule() {
    use wordcraft_engine::file_access::{AutomationRoots, confine};
    let (dir, path) = source_file("secret.txt", "words");
    let mut a = window("one");
    a.assist_with(Box::new(Fake::new()));
    let none = AutomationRoots::none();
    let r = confine(Some(&none), || a.execute("assist.knowledge", json!({"add": path.to_string_lossy()})));
    assert!(r.is_err(), "{r:?}");
    assert_eq!(sources_read(&mut a)["sources"], json!([]));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn adding_a_source_takes_the_person_s_click() {
    use egui_kittest::kittest::Queryable;
    let (dir, path) = source_file("picked.txt", "words");
    let mut app = window("one two three");
    let picked = path.to_string_lossy().into_owned();
    app.services.pick_open = Some(Box::new(move |_| Some(picked.clone())));
    let mut h = harness(app);
    h.state_mut().assist.view = crate::assist::PaneView::Details;
    h.state_mut().run("assist.pane", json!({"value": true})).unwrap();
    h.run_steps(3);
    let frame = |h: &mut egui_kittest::Harness<'_, WordApp>| {
        let ctx = h.ctx.clone();
        let mut raw = std::mem::take(h.input_mut());
        h.state_mut().raw_input_hook(&ctx, &mut raw);
        *h.input_mut() = raw;
        h.step();
    };
    let pos = h.get_by_label("Add a source…").rect().center();
    let (req, _rx) = crate::control::ControlRequest::new("ui.click", json!({"x": pos.x, "y": pos.y}));
    let ctx = h.ctx.clone();
    assert!(matches!(crate::control::handle(h.state_mut(), &ctx, &req), crate::control::Outcome::Done(_)));
    for _ in 0..20 {
        frame(&mut h);
    }
    assert!(!h.state().assist.add_open, "a click the control channel made opens nothing");
    assert!(h.state().assist.notice.as_deref().is_some_and(|n| n.contains("Only you")), "{:?}", h.state().assist.notice);

    h.get_by_label("Add a source…").click();
    h.run_steps(2);
    h.get_by_label("A file…").click();
    for _ in 0..4000 {
        h.run_steps(1);
        if !h.state().assist.knowledge.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(h.state().assist.knowledge[0].name, "picked.txt");
    h.run_steps(2);
    assert!(h.query_by_label("picked.txt").is_some());
    h.get_by_label("Remove").click();
    h.run_steps(2);
    assert!(h.state().assist.knowledge.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn sources_belong_to_their_conversation() {
    let mut a = window("one");
    a.assist_with(Box::new(Fake::new()));
    let (dir, path) = source_file("before.txt", "words");
    a.execute("assist.knowledge", json!({"add": path.to_string_lossy()})).unwrap();
    sources_read(&mut a);
    ask(&mut a, "First", None);
    settle(&mut a);
    assert_eq!(a.assist.knowledge.len(), 1, "what was attached before the first goes with it");
    a.assist.knowledge.push(wordcraft_assist::Knowledge::new("more.txt", "words"));
    ask(&mut a, "Another role", Some("viewer"));
    settle(&mut a);
    assert!(a.assist.knowledge.is_empty(), "a new conversation starts without them");

    // One still being read for this document doesn't land in another's new conversation.
    a.execute("assist.knowledge", json!({"add": path.to_string_lossy()})).unwrap();
    assert_eq!(a.assist.adding.len(), 1);
    a.execute("file.new", json!({})).unwrap();
    a.execute("assist.useDocument", json!({})).unwrap();
    a.assist_with(Box::new(Fake::new()));
    ask(&mut a, "In another document", None);
    settle(&mut a);
    assert_eq!(sources_read(&mut a)["sources"], json!([]));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_source_attached_in_one_document_doesn_t_reach_another() {
    let mut a = window("first");
    let fake = Fake::new();
    let seen = fake.seen();
    a.assist_with(Box::new(fake));
    let (dir, path) = source_file("private.txt", "words");
    a.execute("assist.knowledge", json!({"add": path.to_string_lossy()})).unwrap();
    assert_eq!(sources_read(&mut a)["sources"].as_array().unwrap().len(), 1);
    a.execute("file.new", json!({})).unwrap();
    assert_eq!(a.execute("assist.knowledge", json!({})).unwrap()["sources"], json!([]));
    ask(&mut a, "First prompt, here", None);
    settle(&mut a);
    assert!(seen.lock().unwrap().turns[0].knowledge.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_plain_text_file_of_line_breaks_is_read_as_text_and_a_file_past_the_limit_refused() {
    let (dir, breaks) = source_file("breaks.txt", &"\n".repeat(1 << 20));
    let mut a = window("one");
    a.assist_with(Box::new(Fake::new()));
    a.execute("assist.knowledge", json!({"add": breaks.to_string_lossy()})).unwrap();
    let now = sources_read(&mut a);
    assert_eq!(now["sources"][0]["cut"], true, "{now}");
    let big = dir.join("big.txt");
    std::fs::write(&big, vec![b'x'; (super::MAX_SOURCE_FILE + 1) as usize]).unwrap();
    a.execute("assist.knowledge", json!({"add": big.to_string_lossy()})).unwrap();
    let now = sources_read(&mut a);
    assert_eq!(now["sources"].as_array().unwrap().len(), 1, "{now}");
    assert!(now["error"].as_str().unwrap_or_default().contains("larger than"), "{now}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn another_open_document_can_be_a_source() {
    let mut a = window("first document");
    let first = a.session.document_id();
    a.execute("file.new", json!({})).unwrap();
    assert_ne!(a.session.document_id(), first);
    super::add_source_document(&mut a, first, "First");
    assert_eq!(a.assist.knowledge[0].name, "First");
    assert_eq!(a.assist.knowledge[0].text.trim_end(), "first document");

    // A long one is read only as far as it takes to say it was cut.
    let long = window(&"long words\n".repeat(64 << 10));
    let text = super::text_for_source(&long.session.doc);
    assert!(text.len() < (64 << 10) + 64, "{}", text.len());
    assert!(wordcraft_assist::Knowledge::new("Long", &text).cut);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_citation_with_real_words_selects_them_and_one_with_invented_words_shows_no_chip() {
    use egui_kittest::kittest::Queryable;
    let answer = r#"It says two [[cite: "two three"]]. It also says [[cite: "made up words"]] nothing."#;
    let mut h = answered(answer);
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "1").is_some(), "one chip");
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "2").is_none(), "an invented citation shows no chip");
    assert!(h.query_by_label_contains("cite").is_none(), "the markers don't show");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "1").click();
    h.run_steps(2);
    let s = &h.state().session;
    assert_eq!(s.doc.copy_range(&s.sel.anchor, &s.sel.focus).plain_text(), "two three", "the chip selects the words");

    h.get_by_label("Details").click();
    h.run_steps(2);
    h.get_by_label("Steps").click();
    h.run_steps(2);
    assert!(h.query_by_label("“two three”").is_some(), "Sources lists what was found");
    assert!(h.query_by_label("One citation couldn't be found.").is_some(), "and says what wasn't");
    assert!(h.query_by_label_contains("made up words").is_none());

    let details = h.state_mut().execute("assist.details", json!({})).unwrap();
    assert_eq!(details["turns"][0]["unfound"], 1);
    assert_eq!(details["turns"][0]["citations"][0]["words"], "two three");
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_citation_of_an_attached_source_has_a_chip_that_says_which() {
    let (dir, path) = source_file("notes.txt", "The source says this.");
    let mut app = window("one two three");
    app.assist_with(Box::new(Fake::new().then(vec![Step::say(r#"Yes [[cite: "says this"]]."#)])));
    app.execute("assist.knowledge", json!({"add": path.to_string_lossy()})).unwrap();
    sources_read(&mut app);
    let mut h = harness(app);
    h.state_mut().run("assist.ask", json!({"prompt": "Go"})).unwrap();
    for _ in 0..4000 {
        h.run_steps(1);
        if !h.state().conversation().is_some_and(|c| c.is_busy()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(2);
    use egui_kittest::kittest::Queryable;
    h.get_by_role_and_label(egui::accesskit::Role::Button, "1").click();
    h.run_steps(2);
    assert!(h.state().assist.notice.as_deref().is_some_and(|n| n.contains("notes.txt")), "{:?}", h.state().assist.notice);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_citation_in_a_thought_isn_t_read() {
    use egui_kittest::kittest::Queryable;
    let mut app = window("one two three");
    app.assist_with(Box::new(Fake::new().then(vec![Step::Think(r#"Maybe [[cite: "two three"]]"#.into()), Step::say("Done.")])));
    let mut h = harness(app);
    h.state_mut().run("assist.ask", json!({"prompt": "Go"})).unwrap();
    for _ in 0..4000 {
        h.run_steps(1);
        if !h.state().conversation().is_some_and(|c| c.is_busy()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(2);
    h.get_by_label("Thinking").click();
    h.run_steps(3);
    assert!(h.query_by_label_contains("[[cite:").is_some(), "a thought shows as it came");
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "1").is_none(), "no chip");
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_source_replaced_by_another_leaves_no_chip_for_words_it_alone_had() {
    use egui_kittest::kittest::Queryable;
    let mut app = window("one two three");
    app.assist_with(Box::new(Fake::new().then(vec![Step::say(r#"Yes [[cite: "only the old one"]]."#)])));
    let (dir, old) = source_file("old.txt", "only the old one");
    let new = dir.join("new.txt");
    std::fs::write(&new, "something else").unwrap();
    app.execute("assist.knowledge", json!({"add": old.to_string_lossy()})).unwrap();
    sources_read(&mut app);
    let mut h = harness(app);
    h.state_mut().run("assist.ask", json!({"prompt": "Go"})).unwrap();
    for _ in 0..4000 {
        h.run_steps(1);
        if !h.state().conversation().is_some_and(|c| c.is_busy()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(2);
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "1").is_some());
    h.state_mut().execute("assist.knowledge", json!({"remove": 0})).unwrap();
    h.run_steps(2);
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "1").is_none(), "its source is gone");
    h.state_mut().execute("assist.knowledge", json!({"add": new.to_string_lossy()})).unwrap();
    sources_read(h.state_mut());
    h.run_steps(2);
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "1").is_none(), "the same number of sources, others' words");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_chip_selects_in_the_body_from_wherever_the_caret_is_and_past_a_deletion() {
    use egui_kittest::kittest::Queryable;
    let mut app = window("one obsolete two");
    app.execute("select.text", json!({"text": "obsolete "})).unwrap();
    app.execute("review.trackChanges", json!({"value": true})).unwrap();
    app.execute("text.delete", json!({})).unwrap();
    app.assist_with(Box::new(Fake::new().then(vec![Step::say(r#"Yes [[cite: "one two"]] and [[cite: "obsolete"]]."#)])));
    let mut h = harness(app);
    h.state_mut().run("assist.ask", json!({"prompt": "Go"})).unwrap();
    for _ in 0..4000 {
        h.run_steps(1);
        if !h.state().conversation().is_some_and(|c| c.is_busy()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(2);
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "2").is_none(), "words deleted aren't the document's");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "1").click();
    h.run_steps(2);
    let s = &h.state().session;
    assert_eq!((s.sel.anchor.off, s.sel.focus.off), (0, "one obsolete two".len()));
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_source_attached_for_another_document_s_first_turn_reaches_it() {
    let (dir, path) = source_file("for-b.txt", "words");
    let mut a = window("document A");
    a.assist_with(Box::new(Fake::new()));
    ask(&mut a, "In A", None);
    settle(&mut a);
    a.execute("file.new", json!({})).unwrap();
    a.execute("assist.useDocument", json!({})).unwrap();
    a.execute("assist.knowledge", json!({"add": path.to_string_lossy()})).unwrap();
    sources_read(&mut a);
    let fake = Fake::new();
    let seen = fake.seen();
    a.assist_with(Box::new(fake));
    ask(&mut a, "In B", None);
    settle(&mut a);
    assert_eq!(seen.lock().unwrap().turns[0].knowledge.first().map(|s| s.name.as_str()), Some("for-b.txt"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_prompt_waits_for_a_source_still_being_read() {
    let (dir, path) = source_file("slow.txt", "words");
    let mut a = window("one");
    let fake = Fake::new();
    let seen = fake.seen();
    a.assist_with(Box::new(fake));
    a.execute("assist.knowledge", json!({"add": path.to_string_lossy()})).unwrap();
    // A read that hasn't ended, whatever the disk does.
    let (held, rx) = std::sync::mpsc::channel();
    a.assist.adding.push(rx);
    let waited = a.execute("assist.ask", json!({"prompt": "Now"}));
    assert!(waited.is_err() || a.assist.notice.as_deref().is_some_and(|n| n.contains("still being read")), "{waited:?}");
    assert!(seen.lock().unwrap().turns.is_empty(), "no turn went without it");
    drop(held);
    sources_read(&mut a);
    ask(&mut a, "Now", None);
    settle(&mut a);
    assert_eq!(seen.lock().unwrap().turns[0].knowledge.len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

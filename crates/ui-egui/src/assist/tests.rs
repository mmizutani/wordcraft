//! The window's assistant with the scripted one ([`Fake`]): what a prompt does in the document,
//! Stop, roles, the setting, and the pane.

use std::time::Duration;

use serde_json::{Value, json};
use wordcraft_assist::{Entry, Fake, Step, StopReason};
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
    a.execute("assist.ask", json!({"prompt": "Ask permission"})).unwrap();
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

use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use serde_json::json;
use std::{cell::RefCell, rc::Rc, sync::mpsc};
use wordcraft_doc::{Document, StoryRef};

use super::{Reply, Request, Rewrite, Session};
use crate::{Services, WordApp};

fn selected() -> Session {
    let mut session = Session::new(Document::from_text("one two three"));
    session.author = "Writer".into();
    session.run("select.text", &json!({"text": "two"})).unwrap();
    session
}

#[test]
fn rewrite_stages_a_chatgpt_replacement_the_reviewer_can_accept_or_reject() {
    let mut session = selected();
    Rewrite::capture(&session).unwrap().stage(&mut session, "second").unwrap();

    assert!(!session.doc.settings.track_changes);
    assert_eq!(session.author, "Writer");
    assert_eq!(session.role, None);
    let changes = wordcraft_engine::cmd::review::changes_of(&session, "ChatGPT").unwrap();
    assert_eq!(changes.len(), 2);
    assert!(changes.iter().any(|change| change.kind == "delete" && change.text == "two"));
    assert!(changes.iter().any(|change| change.kind == "insert" && change.text == "second"));
    for (command, expected) in [("review.acceptAll", "one second three"), ("review.rejectAll", "one two three")] {
        let mut review = Session::new(session.doc.clone());
        review.run(command, &json!({"author": "ChatGPT"})).unwrap();
        assert_eq!(review.doc.plain_text(StoryRef::Body), expected);
    }
}

#[test]
fn a_delayed_rewrite_leaves_a_changed_selection_revision_or_document_alone() {
    for (command, params) in [("select.text", json!({"text": "three"})), ("format.bold", json!({})), ("file.new", json!({}))] {
        let mut session = selected();
        let pending = Rewrite::capture(&session).unwrap();
        session.run(command, &params).unwrap();
        let expected = session.doc.clone();
        assert!(pending.stage(&mut session, "second").is_err(), "{command}");
        assert!(session.doc == expected, "{command}");
    }
}

fn local_app(session: Session) -> (WordApp, Rc<RefCell<Vec<String>>>) {
    let sent = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&sent);
    let service = Box::new(move |request| {
        let reply = match request {
            Request::Status => Reply::Status { connected: true, connecting: false },
            Request::Rewrite(text) => {
                seen.borrow_mut().push(text);
                Reply::Replacement("second".into())
            }
            _ => panic!("unexpected request"),
        };
        let (send, recv) = mpsc::channel();
        send.send(Ok(reply)).unwrap();
        recv
    });
    let mut app = WordApp::new(session, Services { local_assistant: Some(service), ..Services::default() });
    app.assist_as_web();
    (app, sent)
}

fn app_harness(app: WordApp) -> Harness<'static, WordApp> {
    Harness::builder().with_size(egui::vec2(1500.0, 900.0)).build_ui_state(
        |ui, app: &mut WordApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        app,
    )
}

fn open_pane(mut app: WordApp) -> Harness<'static, WordApp> {
    app.run("assist.pane", json!({"value": true})).unwrap();
    let mut harness = app_harness(app);
    harness.run_steps(4);
    assert!(harness.query_by_label("Connected to ChatGPT.").is_some());
    harness
}

#[test]
fn local_account_buttons_require_a_person() {
    use std::cell::Cell;

    let sent = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&sent);
    let account = Rc::new(Cell::new(super::Account::Disconnected));
    let status = Rc::clone(&account);
    let response = Rc::new(RefCell::new(None));
    let pending_response = Rc::clone(&response);
    let service = Box::new(move |request| {
        let is_status = matches!(request, Request::Status);
        let reply = match request {
            Request::Status => Reply::Status {
                connected: matches!(status.get(), super::Account::Connected),
                connecting: matches!(status.get(), super::Account::Connecting),
            },
            Request::Connect => {
                seen.borrow_mut().push("connect");
                status.set(super::Account::Connecting);
                Reply::AuthorizationUrl("https://example.com/sign-in".into())
            }
            Request::Disconnect => {
                seen.borrow_mut().push("disconnect");
                status.set(super::Account::Disconnected);
                Reply::Disconnected
            }
            Request::Rewrite(_) => panic!("unexpected request"),
        };
        let (send, recv) = mpsc::channel();
        if is_status {
            send.send(Ok(reply)).unwrap();
        } else {
            *pending_response.borrow_mut() = Some((send, reply));
        }
        recv
    });
    let mut app = WordApp::new(selected(), Services { local_assistant: Some(service), ..Services::default() });
    app.assist_as_web();
    app.run("assist.pane", json!({"value": true})).unwrap();
    let synthetic = Rc::new(Cell::new(false));
    let frame_origin = Rc::clone(&synthetic);
    let mut harness = Harness::builder().with_size(egui::vec2(1500.0, 900.0)).build_ui_state(
        move |ui, app: &mut WordApp| {
            app.synthetic_pending = frame_origin.get();
            app.logic(ui.ctx());
            app.ui(ui);
        },
        app,
    );
    harness.run_steps(4);

    for (label, command, request, expected) in [
        ("Continue with ChatGPT", "ui.localAssistantConnect", "connect", "Cancel sign-in"),
        ("Cancel sign-in", "ui.localAssistantDisconnect", "disconnect", "Continue with ChatGPT"),
        ("Disconnect", "ui.localAssistantDisconnect", "disconnect", "Continue with ChatGPT"),
    ] {
        if label == "Disconnect" {
            account.set(super::Account::Connected);
            harness.state_mut().assist.local.poll_at = 0.0;
            harness.run_steps(3);
        }
        assert!(harness.state_mut().execute(command, json!({})).is_err());
        synthetic.set(true);
        harness.get_by_label(label).click();
        harness.run_steps(3);
        assert!(sent.borrow().is_empty(), "{label} sent a request for synthetic input");

        synthetic.set(false);
        harness.get_by_label(label).click();
        harness.run_steps(3);
        assert!(harness.get_by_label(label).accesskit_node().is_disabled(), "{label}");
        assert!(harness.state_mut().run(command, json!({})).is_err());
        assert_eq!(*sent.borrow(), [request], "{label}");
        sent.borrow_mut().clear();
        let (send, reply) = response.borrow_mut().take().unwrap();
        send.send(Ok(reply)).unwrap();
        harness.run_steps(3);
        assert!(harness.query_by_label(expected).is_some());
    }

    let mut app_without_service = WordApp::new(selected(), Services::default());
    for command in ["ui.localAssistantConnect", "ui.localAssistantDisconnect"] {
        assert!(!crate::widgets::enabled(&app_without_service, command));
        assert!(app_without_service.run(command, json!({})).is_err());
    }
}

#[test]
fn local_rewrite_command_requires_a_person_and_an_eligible_selection() {
    let id = "ui.localAssistantRewrite";
    let (app, sent) = local_app(selected());
    let mut harness = open_pane(app);
    assert!(harness.state_mut().execute(id, json!({})).is_err());
    harness.state_mut().synthetic_pending = true;
    assert!(harness.state_mut().run(id, json!({})).is_err());
    harness.state_mut().synthetic_pending = false;
    harness.state_mut().run("ui.backstage", json!({"value": true})).unwrap();
    assert!(!crate::widgets::enabled(harness.state(), id));
    assert!(harness.state_mut().run(id, json!({})).is_err());
    harness.state_mut().run("ui.backstage", json!({"value": false})).unwrap();
    harness.state_mut().run("ui.dialog", json!({"name": "paragraph"})).unwrap();
    assert!(harness.state().modal_open());
    assert!(!crate::widgets::enabled(harness.state(), id));
    assert!(harness.state_mut().run(id, json!({})).is_err());
    harness.state_mut().run("ui.closeDialog", json!({})).unwrap();
    assert!(sent.borrow().is_empty());

    harness.state_mut().run("select.text", json!({"text": "three"})).unwrap();
    assert!(crate::widgets::enabled(harness.state(), id));
    harness.state_mut().run(id, json!({})).unwrap();
    assert_eq!(*sent.borrow(), ["three"]);
    assert!(!crate::widgets::enabled(harness.state(), id));
    assert!(harness.state_mut().run(id, json!({})).is_err());
    assert_eq!(*sent.borrow(), ["three"]);

    let (app, sent) = local_app(Session::new(Document::from_text("No selection.")));
    let mut harness = open_pane(app);
    assert!(!crate::widgets::enabled(harness.state(), id));
    assert!(harness.state_mut().run(id, json!({})).is_err());
    assert!(sent.borrow().is_empty());

    let mut desktop = WordApp::new(selected(), Services::default());
    assert!(!crate::widgets::enabled(&desktop, id));
    assert!(desktop.run(id, json!({})).is_err());
}

#[test]
fn local_pane_sends_only_the_selection_and_opens_review_on_the_proposal() {
    let (app, sent) = local_app(selected());
    let mut harness = open_pane(app);
    harness.get_by_label("Rewrite selected text").click();
    harness.run_steps(3);
    assert_eq!(*sent.borrow(), ["two"]);
    assert!(harness.query_by_label("ChatGPT proposed a tracked rewrite.").is_some());

    harness.get_by_label("Review rewrite").click();
    harness.run_steps(3);
    assert_eq!(harness.state().ui.tab, "Review");
    assert!(harness.get_by_label("Rewrite selected text").accesskit_node().is_disabled());
    assert!(harness.query_by_label("This selection contains tracked changes. Accept or reject them in Review, or select other text.").is_some());
    harness.state_mut().run("review.accept", json!({})).unwrap();
    assert_eq!(harness.state().session.doc.plain_text(StoryRef::Body), "one second three");
    harness.state_mut().run("select.text", json!({"text": "second"})).unwrap();
    harness.run_steps(3);
    assert!(!harness.get_by_label("Rewrite selected text").accesskit_node().is_disabled());
    assert_eq!(*sent.borrow(), ["two"]);
}

#[test]
fn local_pane_selects_only_the_current_paragraph_and_waits_for_rewrite() {
    let (app, sent) = local_app(Session::new(Document::from_text("First paragraph.\nSecond paragraph.")));
    let mut harness = open_pane(app);
    assert!(harness.get_by_label("Rewrite selected text").accesskit_node().is_disabled());
    harness.get_by_label("Select current paragraph").click();
    harness.run_steps(3);
    assert!(!harness.get_by_label("Rewrite selected text").accesskit_node().is_disabled());
    assert!(sent.borrow().is_empty());
    harness.get_by_label("Rewrite selected text").click();
    harness.run_steps(3);
    assert_eq!(*sent.borrow(), ["First paragraph."]);
}

#[test]
fn local_assistant_menu_opens_the_pane_without_sending_text() {
    let (app, sent) = local_app(selected());
    let selected = app.session.sel.clone();
    let mut harness = Harness::builder().with_size(egui::vec2(400.0, 400.0)).build_ui_state(crate::assist::action_items, app);
    harness.run_steps(2);
    assert!(harness.query_by_label_contains("Draft…").is_none());
    harness.get_by_label_contains("Open Assistant for Rewrite…").click();
    harness.run_steps(2);
    assert!(harness.state().session.view.assistant_pane);
    assert_eq!(harness.state().session.sel, selected);
    assert!(sent.borrow().is_empty());
}

#[test]
fn local_rewrite_command_search_opens_the_pane_without_sending_text() {
    let (mut app, sent) = local_app(selected());
    let selection = app.session.sel.clone();
    app.dialog = Some(crate::dialogs::Dialog::Commands { query: "rewrite".into(), swallow: false });
    let mut harness = app_harness(app);
    harness.run_steps(4);
    harness.get_by_label_contains("Open Assistant for Rewrite…").click();
    harness.run_steps(4);

    assert!(harness.state().session.view.assistant_pane);
    assert_eq!(harness.state().session.sel, selection);
    assert!(sent.borrow().is_empty());
    harness.get_by_label("Rewrite selected text").click();
    harness.run_steps(3);
    assert_eq!(*sent.borrow(), ["two"]);
}

#[test]
fn local_pane_excludes_hidden_text_from_the_selection_and_paragraph_shortcut() {
    let mut session = selected();
    session.run("format.hidden", &json!({"value": true})).unwrap();
    session.run("select.range", &json!({"anchor": {"block": 0, "off": 0}, "focus": {"block": 0, "off": 13}})).unwrap();
    let (app, sent) = local_app(session);
    let mut harness = open_pane(app);
    assert!(harness.get_by_label("Rewrite selected text").accesskit_node().is_disabled());
    assert!(harness.query_by_label("Hidden text cannot be rewritten. Select visible text to rewrite.").is_some());
    assert!(sent.borrow().is_empty());

    harness.state_mut().run("select.range", json!({"anchor": {"block": 0, "off": 1}, "focus": {"block": 0, "off": 1}})).unwrap();
    harness.run_steps(3);
    assert!(harness.query_by_label("Select current paragraph").is_none());
    assert!(sent.borrow().is_empty());

    harness.state_mut().run("select.text", json!({"text": "three"})).unwrap();
    harness.run_steps(3);
    harness.get_by_label("Rewrite selected text").click();
    harness.run_steps(3);
    assert_eq!(*sent.borrow(), ["three"]);
}

#[test]
fn local_rewrite_rejects_hidden_text_inherited_from_document_styles() {
    use std::sync::Arc;
    use wordcraft_doc::props::CharProps;
    use wordcraft_doc::styles::{Style, StyleKind, TableStyleParts};
    use wordcraft_doc::{Block, Paragraph, Path, Pos, para_block, table::Table};

    let hidden = CharProps { hidden: Some(true), ..Default::default() };
    let mut doc = Document::new();
    for kind in [StyleKind::Paragraph, StyleKind::Character, StyleKind::Table] {
        let base = format!("Hidden{kind:?}");
        Arc::make_mut(&mut doc.styles).upsert(Style {
            id: base.clone(),
            kind,
            chr: if kind == StyleKind::Table { CharProps::default() } else { hidden.clone() },
            table: (kind == StyleKind::Table).then(|| TableStyleParts { header_chr: hidden.clone(), ..Default::default() }),
            ..Default::default()
        });
        Arc::make_mut(&mut doc.styles).upsert(Style { id: format!("Derived{kind:?}"), kind, based_on: Some(base), ..Default::default() });
    }
    let mut paragraph = Paragraph::with_text("secret", CharProps::default());
    paragraph.props.style = Some("DerivedParagraph".into());
    let character = Paragraph::with_text("secret", CharProps { style: Some("DerivedCharacter".into()), ..Default::default() });
    let mut table = Table::new(2, 1, 180.0);
    table.props.style = Some("DerivedTable".into());
    table.props.look.header_row = true;
    table.rows[0].cells[0].blocks = vec![para_block(Paragraph::with_text("secret", CharProps::default()))];
    table.rows[1].cells[0].blocks = vec![para_block(Paragraph::with_text("public", CharProps::default()))];
    doc.body = vec![para_block(paragraph), para_block(character), Arc::new(Block::Table(table))];
    let (app, sent) = local_app(Session::new(doc));
    let mut harness = open_pane(app);

    let hidden_ranges = [Path::top(0), Path::top(1), Path(vec![2, 0, 0, 0])];
    let mut allowed = Vec::new();
    for path in hidden_ranges {
        let anchor = Pos::new(StoryRef::Body, path.clone(), 0);
        let focus = Pos { off: 6, ..anchor.clone() };
        harness.state_mut().run("select.range", json!({"anchor": anchor, "focus": focus})).unwrap();
        harness.run_steps(3);
        if !harness.get_by_label("Rewrite selected text").accesskit_node().is_disabled() {
            allowed.push(path);
        }
    }
    assert!(allowed.is_empty(), "hidden ranges enabled Rewrite: {allowed:?}");
    assert!(sent.borrow().is_empty());

    let anchor = Pos::new(StoryRef::Body, Path(vec![2, 1, 0, 0]), 0);
    let focus = Pos { off: 6, ..anchor.clone() };
    harness.state_mut().run("select.range", json!({"anchor": anchor, "focus": focus})).unwrap();
    harness.run_steps(3);
    harness.get_by_label("Rewrite selected text").click();
    harness.run_steps(3);
    assert_eq!(*sent.borrow(), ["public"]);
}

#[test]
fn local_rewrite_replaces_the_selection_when_typing_leaves_it_intact() {
    let mut session = selected();
    session.prefs.typing_replaces_selection = false;
    let (app, sent) = local_app(session);
    let mut harness = open_pane(app);

    harness.get_by_label("Rewrite selected text").click();
    harness.run_steps(3);
    assert_eq!(*sent.borrow(), ["two"]);
    let app = harness.state_mut();
    app.run("review.acceptAll", json!({"author": "ChatGPT"})).unwrap();

    assert_eq!(app.session.doc.plain_text(StoryRef::Body), "one second three");
    assert!(!app.session.prefs.typing_replaces_selection);
}

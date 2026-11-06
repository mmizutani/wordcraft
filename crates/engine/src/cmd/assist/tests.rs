use serde_json::{Value, json};
use wordcraft_doc::Document;

use super::{AssistantStatus, MAX_PROMPT};
use crate::room::{Role, Room};
use crate::scope::Scope;
use crate::{CmdError, Session};

/// A session in a window with an assistant set up.
fn in_window() -> Session {
    let mut s = Session::new(Document::from_text("one two three"));
    s.assistant = AssistantStatus { window: true, ready: true, busy: false };
    s
}

/// The requests the commands made of the window, taken out.
fn requests(s: &mut Session) -> Vec<Value> {
    std::mem::take(&mut s.ui_requests).into_iter().filter(|r| r.get("assist").is_some()).collect()
}

fn disabled(r: Result<Value, CmdError>) -> String {
    match r {
        Err(CmdError::Disabled(why)) => why,
        other => panic!("not disabled: {other:?}"),
    }
}

#[test]
fn a_prompt_goes_to_the_window_with_its_role() {
    let mut s = in_window();
    assert_eq!(s.run("assist.ask", &json!({"prompt": "  Tighten this  "})).unwrap(), json!({"asked": true}));
    assert_eq!(requests(&mut s), [json!({"assist": "ask", "prompt": "Tighten this", "role": null})]);
    s.run("assist.ask", &json!({"prompt": "Check it", "role": " editor "})).unwrap();
    assert_eq!(requests(&mut s), [json!({"assist": "ask", "prompt": "Check it", "role": "editor"})]);
    // Asking changes nothing in the document and leaves no undo step: the assistant's edits
    // come through the shared document, in its name.
    assert!(!s.can_undo());
    assert_eq!(s.doc.plain_text(wordcraft_doc::StoryRef::Body), "one two three");
}

#[test]
fn a_bad_prompt_or_role_is_refused_and_nothing_is_asked() {
    let mut s = in_window();
    for bad in [
        json!({}),
        json!({"prompt": ""}),
        json!({"prompt": " \n\t "}),
        json!({"prompt": 7}),
        json!({"prompt": ["a"]}),
        json!({"prompt": "x".repeat(MAX_PROMPT + 1)}),
        json!({"prompt": "Hi", "role": "owner"}),
        json!({"prompt": "Hi", "role": "Owner "}),
        json!({"prompt": "Hi", "role": "admin"}),
        json!({"prompt": "Hi", "role": 3}),
    ] {
        assert!(matches!(s.run("assist.ask", &bad), Err(CmdError::Params(_))), "{bad}");
    }
    assert!(requests(&mut s).is_empty());
    let e = s.run("assist.ask", &json!({"prompt": "Hi", "role": "owner"})).unwrap_err().to_string();
    assert!(e.contains("can't be an owner"), "{e}");
}

/// The setting is refused owner and anything that isn't a role; it is the person's preference,
/// read back without a value.
#[test]
fn the_default_role_is_never_owner() {
    let mut s = Session::new(Document::new());
    assert_eq!(s.run("assist.defaultRole", &json!({})).unwrap(), json!({"value": "suggester"}));
    for bad in [json!({"value": "owner"}), json!({"value": " owner"}), json!({"value": "boss"}), json!({"value": true}), json!({"value": {}})] {
        assert!(matches!(s.run("assist.defaultRole", &bad), Err(CmdError::Params(_))), "{bad}");
    }
    assert_eq!(s.prefs.assistant.default_role, Role::Suggester);
    for name in ["viewer", "commenter", "editor", "suggester"] {
        assert_eq!(s.run("assist.defaultRole", &json!({"value": name})).unwrap(), json!({"value": name}));
        assert_eq!(s.prefs.assistant.default_role.name(), name);
    }
    assert!(!s.can_undo(), "a preference, not an edit");
}

#[test]
fn the_pane_toggles_and_asks_for_the_prompt_box() {
    let mut s = Session::new(Document::new());
    assert!(!s.view.assistant_pane);
    assert_eq!(s.run("assist.pane", &json!({})).unwrap(), json!({"value": true}));
    assert!(s.view.assistant_pane);
    assert_eq!(requests(&mut s), [json!({"assist": "pane", "open": true})]);
    s.run("assist.pane", &json!({"value": true})).unwrap();
    assert!(s.view.assistant_pane);
    s.run("assist.pane", &json!({"value": false})).unwrap();
    assert!(!s.view.assistant_pane);
    assert!(s.run("assist.pane", &json!({"value": "yes"})).is_err());
    assert!(!s.can_undo());
}

/// Stop is there while the assistant answers; choosing an assistant takes its id, checked here
/// and against what the window offers there.
#[test]
fn stop_and_the_backend_choice() {
    let mut s = in_window();
    assert!(disabled(s.run("assist.cancel", &json!({}))).contains("isn't answering"));
    s.assistant.busy = true;
    s.run("assist.cancel", &json!({})).unwrap();
    assert_eq!(requests(&mut s), [json!({"assist": "cancel"})]);
    assert!(disabled(s.run("assist.ask", &json!({"prompt": "More"}))).contains("still answering"));

    s.run("assist.backend", &json!({"id": " fake "})).unwrap();
    s.run("assist.backend", &json!({})).unwrap();
    assert_eq!(requests(&mut s), [json!({"assist": "backend", "id": "fake"}), json!({"assist": "backend", "id": null})]);
    for bad in [json!({"id": ""}), json!({"id": "x".repeat(200)}), json!({"id": "a\u{0}b"}), json!({"id": 1})] {
        assert!(matches!(s.run("assist.backend", &bad), Err(CmdError::Params(_))), "{bad}");
    }
}

/// Without a window (the MCP server, the command line) there is no assistant to ask, and
/// without one set up its buttons say what to do.
#[test]
fn no_window_or_no_assistant_set_up_greys_the_commands_out() {
    let mut s = Session::new(Document::new());
    for id in ["assist.ask", "assist.cancel", "assist.backend"] {
        assert!(disabled(s.run(id, &json!({"prompt": "Hi", "id": "fake"}))).contains("WordCraft window"), "{id}");
    }
    s.assistant.window = true;
    let why = disabled(s.run("assist.ask", &json!({"prompt": "Hi"})));
    assert!(why.contains("Set up an assistant first") && why.contains("sign in"), "{why}");
    assert!(requests(&mut s).is_empty());
}

/// Junk of every shape: an error or a result, never a panic, and nothing asked of the window
/// unless the params were good.
#[test]
fn junk_params_never_panic() {
    let junk = [
        json!(null),
        json!(1),
        json!("prompt"),
        json!([]),
        json!({"prompt": null, "role": null, "id": null, "value": null}),
        json!({"prompt": {"a": 1}, "role": [], "id": {}, "value": -1}),
        json!({"prompt": "\u{202e}\u{0}", "role": "\u{200b}editor", "id": "\u{7f}"}),
        json!({"prompt": "é".repeat(MAX_PROMPT), "role": "é".repeat(10_000), "id": "é".repeat(10_000), "value": "é".repeat(10_000)}),
    ];
    let mut s = in_window();
    let ids: Vec<&str> = s.registry.all().iter().map(|c| c.id).filter(|id| id.starts_with("assist.")).collect();
    for busy in [false, true] {
        s.assistant.busy = busy;
        for id in &ids {
            for v in &junk {
                let _ = s.run(id, v);
            }
        }
    }
    assert!(!s.can_undo());
    assert_ne!(s.prefs.assistant.default_role, Role::Owner);
}

/// A participant's copy of a shared document can't start or drive an assistant: only the
/// window's owner can. Showing the pane is their own view.
#[test]
fn a_participants_copy_cant_run_the_assistant() {
    assert_eq!(Scope::of("assist.ask"), Some(Scope::App));
    assert_eq!(Scope::of("assist.cancel"), Some(Scope::App));
    assert_eq!(Scope::of("assist.backend"), Some(Scope::App));
    assert_eq!(Scope::of("assist.defaultRole"), Some(Scope::App));
    assert_eq!(Scope::of("assist.pane"), Some(Scope::View));
    let room = Room::new(Document::from_text("Shared"));
    for (n, role) in [Role::Viewer, Role::Commenter, Role::Suggester, Role::Editor].into_iter().enumerate() {
        let mut copy = match room.join(&format!("Agent {n}"), role) {
            Ok(c) => c,
            Err(e) => panic!("{role}: {e}"),
        };
        for (id, v) in [
            ("assist.ask", json!({"prompt": "Rewrite everything"})),
            ("assist.cancel", json!({})),
            ("assist.backend", json!({"id": "fake"})),
            ("assist.defaultRole", json!({"value": "editor"})),
        ] {
            let why = disabled(copy.run(id, &v));
            assert!(why.contains("application's settings"), "{role} {id}: {why}");
        }
        assert!(copy.run("assist.pane", &json!({"value": true})).is_ok(), "{role}");
        assert_eq!(copy.session().prefs.assistant.default_role, Role::Suggester);
    }
}

use egui::accesskit::Role as A11y;
use egui_kittest::kittest::Queryable;
use serde_json::{Value, json};
use wordcraft_doc::Document;
use wordcraft_engine::Session;
use wordcraft_engine::agent_files::AgentFiles;
use wordcraft_engine::room::{Lobby, Role, hosted};

use super::{Row, rows};
use crate::{Services, WordApp};

/// A window showing "one two three" by Minoru, whose lobby is `lobby`.
fn window(lobby: &Lobby) -> WordApp {
    let mut a = WordApp::new(Session::new(Document::from_text("one two three")), Services::default()).with_lobby(lobby.clone());
    a.session.author = "Minoru".into();
    a.ui.language = "en".into();
    a
}

/// The window on File › Share, drawn.
fn harness(a: WordApp) -> egui_kittest::Harness<'static, WordApp> {
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1440.0, 1100.0)).build_ui_state(
        |ui, app: &mut WordApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        a,
    );
    h.state_mut().run("file.share", json!({})).unwrap();
    h.run_steps(4);
    h
}

/// Type `name` in the invite form's Name box.
fn type_name(h: &mut egui_kittest::Harness<'static, WordApp>, name: &str) {
    h.get_by_role_and_label(A11y::TextInput, "Name").click();
    h.run_steps(2);
    h.get_by_role_and_label(A11y::TextInput, "Name").type_text(name);
    h.run_steps(2);
    assert_eq!(h.state().share_form.name, name);
}

fn state(a: &WordApp) -> Value {
    a.share_state()
}

fn row(name: &str, role: Role, files: Option<AgentFiles>, copies: usize, you: bool, invited: bool) -> Row {
    Row { name: name.into(), role, files, copies, you, invited }
}

/// The list has the window first, each participant once however many copies they have, and the
/// invites no one joined with, with the files the setting leaves them.
#[test]
fn the_list_shows_each_participant_once_and_the_invites_not_joined() {
    let lobby = Lobby::default();
    let mut a = window(&lobby);
    a.execute("room.share", json!({})).unwrap();
    let ana = a.execute("room.invite", json!({"name": "Ana", "role": "editor"})).unwrap();
    a.execute("room.invite", json!({"name": "Researcher", "role": "viewer", "files": "all"})).unwrap();
    let key = ana["key"].as_str().unwrap();
    let _one = lobby.join_with(key).unwrap();
    let _two = lobby.join_with(key).unwrap();
    assert_eq!(
        rows(&state(&a)),
        [
            row("Minoru", Role::Owner, None, 1, true, false),
            row("Ana", Role::Editor, Some(AgentFiles::None), 2, false, true),
            // The setting gives agents no file, whatever the invite says.
            row("Researcher", Role::Viewer, Some(AgentFiles::None), 0, false, true),
        ]
    );
    // Junk where the state should be gives no rows, and never panics.
    for junk in [json!(null), json!({"shared": true}), json!({"participants": 3, "invited": "x", "you": []}), json!([1, 2])] {
        let _ = rows(&junk);
    }
    assert!(rows(&json!({})).is_empty());
}

/// Every control of the page runs a room command: Share this document, Invite (which shows
/// the key once, with the line an MCP client runs), Remove access and Stop sharing.
#[test]
fn the_page_s_controls_share_invite_remove_and_stop() {
    let lobby = Lobby::default();
    lobby.set_addr(Some("127.0.0.1:7982".parse().unwrap()));
    let mut h = harness(window(&lobby));
    assert_eq!(h.state().ui.backstage_page, "share");
    h.get_by_role_and_label(A11y::Button, "Share this document").click();
    h.run_steps(3);
    assert_eq!(state(h.state())["shared"], true);
    assert!(h.query_by_label_contains("1 change so far").is_none() && h.query_by_label_contains("0 changes so far").is_some());
    // Invite someone: the button is off until they have a name.
    type_name(&mut h, "Fact checker");
    h.get_by_role_and_label(A11y::Button, "Invite").click();
    h.run_steps(3);
    let invited = state(h.state())["invited"].clone();
    assert_eq!(
        invited,
        json!([{"name": "Fact checker", "role": "suggester", "files": null, "joined": 0, "remote": false, "expires": null, "usesLeft": null}])
    );
    let (name, key) = h.state().share_form.shown.clone().expect("the key shows");
    assert_eq!(name, "Fact checker");
    assert_eq!(key.len(), 32);
    assert!(h.query_by_label(&key).is_some(), "the key is on the page");
    let line = format!("WORDCRAFT_CONTROL_KEY={key} wordcraft-cli mcp --connect 127.0.0.1:7982");
    assert!(h.query_by_label(&line).is_some(), "with the line for an MCP client");
    assert!(h.state().share_form.name.is_empty(), "the form is ready for the next");
    // They join with it; the list says they're connected.
    let mut copy = lobby.join_with(&key).unwrap();
    h.run_steps(2);
    assert!(h.query_by_label("Connected").is_some());
    // Done: the key isn't shown again.
    h.get_by_role_and_label(A11y::Button, "Done").click();
    h.run_steps(2);
    assert!(h.state().share_form.shown.is_none());
    assert!(h.query_by_label(&key).is_none());
    // Remove access takes the key back: their copy stops answering.
    h.get_by_role_and_label(A11y::Button, "Remove access").click();
    h.run_steps(2);
    assert_eq!(state(h.state())["invited"], json!([]));
    assert!(hosted::call(&mut copy, "document.inspect", &json!({})).is_err());
    // Stop sharing.
    h.get_by_role_and_label(A11y::Button, "Stop sharing").click();
    h.run_steps(2);
    assert_eq!(state(h.state()), json!({"shared": false}));
    assert!(h.query_by_label("Share this document").is_some());
}

/// A name the room refuses shows why on the page, and invites no one.
#[test]
fn the_page_says_why_an_invite_is_refused() {
    let lobby = Lobby::default();
    let mut h = harness(window(&lobby));
    h.get_by_role_and_label(A11y::Button, "Share this document").click();
    h.run_steps(3);
    // The window's own name is someone's in the room already.
    type_name(&mut h, "minoru");
    h.get_by_role_and_label(A11y::Button, "Invite").click();
    h.run_steps(3);
    assert!(h.state().share_form.error.contains("someone's name"), "{}", h.state().share_form.error);
    assert!(h.state().share_form.shown.is_none());
    assert_eq!(state(h.state())["invited"], json!([]));
}

/// The key shows only while the page stays open: leaving it, or closing File, drops it.
#[test]
fn the_key_is_gone_once_the_page_is_left() {
    let lobby = Lobby::default();
    let mut h = harness(window(&lobby));
    h.state_mut().execute("room.share", json!({})).unwrap();
    let key = h.state_mut().execute("room.invite", json!({"name": "Ana"})).unwrap()["key"].as_str().unwrap().to_string();
    h.state_mut().share_form.shown = Some(("Ana".into(), key.clone()));
    h.run_steps(2);
    assert!(h.query_by_label(&key).is_some());
    h.state_mut().run("ui.backstage", json!({"value": true, "page": "info"})).unwrap();
    h.run_steps(2);
    h.state_mut().run("file.share", json!({})).unwrap();
    h.run_steps(2);
    assert!(h.state().share_form.shown.is_none());
    assert!(h.query_by_label(&key).is_none());
}

/// File › Share and the tab strip's Share button open the Share page, as `file.share` does for
/// a script.
#[test]
fn share_opens_the_share_page() {
    let lobby = Lobby::default();
    let mut a = window(&lobby);
    a.run("file.share", json!({})).unwrap();
    assert!(a.ui.backstage);
    assert_eq!(a.ui.backstage_page, "share");
    a.run("ui.backstage", json!({"value": false})).unwrap();
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1440.0, 900.0)).build_ui_state(
        |ui, app: &mut WordApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        a,
    );
    h.run_steps(3);
    h.get_by_role_and_label(A11y::Button, "Share").click();
    h.run_steps(2);
    assert!(h.state().ui.backstage);
    assert_eq!(h.state().ui.backstage_page, "share");
    // Its keytip: Alt, F, Z.
    assert_eq!(crate::backstage::keytip_page("Z"), Some("share"));
}

/// The room's commands run through `engine.execute` as well as the control channel's methods:
/// a role changes, an invite is taken back by its name, and junk params are refused, never a
/// panic.
#[test]
fn the_room_commands_change_a_role_and_refuse_junk() {
    let lobby = Lobby::default();
    let mut a = window(&lobby);
    a.execute("room.share", json!({})).unwrap();
    let key = a.execute("room.invite", json!({"name": "Ana", "role": "editor"})).unwrap()["key"].as_str().unwrap().to_string();
    let mut ana = lobby.join_with(&key).unwrap();
    assert_eq!(a.execute("room.role", json!({"name": "Ana", "role": "viewer"})).unwrap(), json!({"changed": true, "name": "Ana", "role": "viewer"}));
    let e = hosted::call(&mut ana, "engine.execute", &json!({"command": "text.insert", "params": {"text": "x"}}));
    assert!(e.is_err(), "a viewer changes nothing: {e:?}");
    assert_eq!(rows(&state(&a))[1].role, Role::Viewer);
    // A participant's copy can't change roles.
    assert!(hosted::call(&mut ana, "room.role", &json!({"name": "Ana", "role": "owner"})).unwrap_err().contains("owner's"));
    let junk =
        [json!(null), json!(7), json!("x"), json!([]), json!({}), json!({"name": 3}), json!({"name": "\u{0}", "role": "owner"}), json!({"key": [1]})];
    for id in ["room.share", "room.invite", "room.role", "room.revoke", "room.participants"] {
        for p in &junk {
            let _ = a.execute(id, p.clone());
        }
    }
    for p in [json!({"name": "Ana"}), json!({"name": "Ana", "role": "admin"}), json!({"role": "editor"}), json!({"name": "Minoru", "role": "viewer"})]
    {
        assert!(a.execute("room.role", p.clone()).is_err(), "{p}");
    }
    assert!(a.execute("room.revoke", json!({})).unwrap_err().contains("`key` or `name`"));
    assert_eq!(a.execute("room.revoke", json!({"name": "ANA"})).unwrap(), json!({"revoked": true}));
    assert!(hosted::call(&mut ana, "document.inspect", &json!({})).is_err());
    assert_eq!(a.execute("room.stop", json!({})).unwrap(), json!({"stopped": true}));
    // Without a shared document they say so.
    assert!(a.execute("room.role", json!({"name": "Ana", "role": "viewer"})).unwrap_err().contains("room.share"));
}

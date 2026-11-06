//! A room the process's own session shares ([`Room::host`]): the app's window lends its session
//! to its owner's copy for each step, and participants with a key get copies of their own,
//! which answer their requests ([`Lobby`], [`hosted::call`]).

use std::sync::{Arc, Barrier};
use std::time::Duration;

use serde_json::{Value, json};
use wordcraft_doc::{BlockId, Document, StoryRef};

use super::changes::difference;
use super::room::{join, run, text};
use crate::room::{Lobby, Replica, Role, Room, RoomError, hosted};
use crate::{CmdError, Session};

/// The window's session: "one two three" shared, with another document, "Notes", open beside it.
fn window() -> (Session, u64, u64) {
    let mut s = Session::new(Document::from_text("one two three"));
    s.author = "Minoru".into();
    let shared = s.document_id();
    let other = s.add_document(Document::from_text("Notes"));
    s.activate(shared).unwrap();
    (s, shared, other)
}

/// Run `id` in the window as the app does: through its copy, lent the window's session, which
/// sends what it changed.
fn in_window(own: &mut Replica, win: &mut Session, id: &str, v: Value) -> Value {
    let r = own.lend(win, |c| {
        let r = c.run(id, &v);
        c.send();
        r
    });
    match r {
        Some(Ok(out)) => out,
        Some(Err(e)) => panic!("{id} {v}: {e}"),
        None => panic!("{id}: the shared document isn't the window's active one"),
    }
}

/// One frame of the window: its copy gets what others changed, and sends its own.
fn frame(own: &mut Replica, win: &mut Session) {
    let _ = own.lend(win, Replica::sync);
}

/// Frames and syncs until the window's copy and `others` have every change and nothing unsent.
fn settle_all(own: &mut Replica, win: &mut Session, others: &mut [Replica]) {
    for _ in 0..50 {
        frame(own, win);
        others.iter_mut().for_each(Replica::sync);
        let version = own.room().version();
        if !own.has_unconfirmed() && own.received() == version && others.iter().all(|r| !r.has_unconfirmed() && r.received() == version) {
            return;
        }
    }
    panic!("the copies don't settle");
}

/// The window's shared document is the hub's, under the same block ids.
fn window_is_hub(win: &Session, room: &Room) {
    let (_, hub) = room.document();
    assert!(win.doc == hub, "the window holds other content: {}", difference(&win.doc, &hub));
    assert_eq!(win.doc.block_ids(), hub.block_ids());
}

fn call(copy: &mut Replica, method: &str, params: Value) -> Result<Value, String> {
    hosted::call(copy, method, &params)
}

fn execute(copy: &mut Replica, command: &str, params: Value) -> Result<Value, String> {
    call(copy, "engine.execute", json!({"command": command, "params": params}))
}

/// The bold characters of the first paragraph.
fn bold(doc: &Document) -> String {
    let Some(p) = doc.body.first().and_then(|b| b.as_para()) else { return String::new() };
    let mut out = String::new();
    let mut at = 0;
    for run in &p.runs {
        let end = at + run.len;
        if run.props.bold == Some(true) {
            out.push_str(&p.text[at..end]);
        }
        at = end;
    }
    out
}

#[test]
fn the_window_shares_its_document_and_its_edits_go_through_its_copy() {
    let (mut win, shared, other) = window();
    let (room, mut own) = Room::host(&mut win, "Minoru").unwrap();
    assert_eq!((own.participant().name.as_str(), own.participant().role), ("Minoru", Role::Owner));
    assert_eq!(own.document(), shared);
    window_is_hub(&win, &room);
    // The copy's own session only stands in for the window's.
    assert_eq!(own.session().doc.plain_text(StoryRef::Body), "");
    let key = room.invite("Copy editor", Role::Suggester).unwrap();
    let mut agent = room.join_with(&key).unwrap();
    // The person types at the end while the agent suggests "2" for "two".
    in_window(&mut own, &mut win, "caret.docEnd", json!({}));
    in_window(&mut own, &mut win, "text.insert", json!({"text": " four"}));
    run(&mut agent, "select.text", json!({"text": "two"}));
    run(&mut agent, "text.insert", json!({"text": "2"}));
    agent.sync();
    settle_all(&mut own, &mut win, std::slice::from_mut(&mut agent));
    window_is_hub(&win, &room);
    assert_eq!(text(&agent), win.doc.plain_text(StoryRef::Body));
    // The agent's edit is a suggestion in their name; the person's isn't tracked.
    let authors: Vec<&str> = win.doc.revisions.iter().map(|r| r.author.as_str()).collect();
    assert_eq!(authors, ["Copy editor", "Copy editor"]);
    let mut accepted = Session::new(win.doc.clone());
    run_in(&mut accepted, "review.acceptAll");
    assert_eq!(accepted.doc.plain_text(StoryRef::Body), "one 2 three four");
    // The window's other document stays open, as it was.
    assert!(win.is_open(other));
    win.activate(other).unwrap();
    assert_eq!(win.doc.plain_text(StoryRef::Body), "Notes");
    assert_eq!((own.stats().lost, own.stats().read_again), (0, 0));
}

fn run_in(s: &mut Session, id: &str) {
    if let Err(e) = s.run(id, &json!({})) {
        panic!("{id}: {e}");
    }
}

#[test]
fn the_window_s_edit_that_clashes_is_made_again_not_lost() {
    let (mut win, _, other) = window();
    let (room, mut own) = Room::host(&mut win, "Minoru").unwrap();
    let mut ben = join(&room, "Ben");
    // The person bolds "two" while Ben types over it; Ben's change gets to the hub first.
    run(&mut ben, "select.text", json!({"text": "two"}));
    run(&mut ben, "text.insert", json!({"text": "2"}));
    ben.send();
    in_window(&mut own, &mut win, "select.text", json!({"text": "two"}));
    in_window(&mut own, &mut win, "format.bold", json!({}));
    // The hub sent the person's change back; their copy bolded what took the place of "two".
    assert_eq!((own.stats().bounced, own.stats().made_again, own.stats().lost), (1, 1, 0));
    assert_eq!(win.doc.plain_text(StoryRef::Body), "one 2 three");
    assert_eq!(bold(&win.doc), "2");
    settle_all(&mut own, &mut win, std::slice::from_mut(&mut ben));
    window_is_hub(&win, &room);
    assert_eq!(bold(&ben.session().doc), "2");
    assert!(win.is_open(other), "making the edit again keeps the window's other documents");
}

#[test]
fn the_window_catches_up_when_it_shows_the_shared_document_again() {
    let (mut win, shared, other) = window();
    let (room, mut own) = Room::host(&mut win, "Minoru").unwrap();
    let mut ben = join(&room, "Ben");
    // The person works in their notes while Ben edits the shared document.
    win.activate(other).unwrap();
    run_in(&mut win, "caret.docEnd");
    win.run("text.insert", &json!({"text": " to self"})).unwrap();
    run(&mut ben, "caret.docStart", json!({}));
    run(&mut ben, "text.insert", json!({"text": "zero "}));
    ben.sync();
    assert!(own.lend(&mut win, Replica::sync).is_none(), "the copy waits while the notes are shown");
    assert_eq!(win.doc.plain_text(StoryRef::Body), "Notes to self");
    // Back on the shared document, the window has Ben's edit; the notes never went to the hub.
    win.activate(shared).unwrap();
    settle_all(&mut own, &mut win, std::slice::from_mut(&mut ben));
    assert_eq!(win.doc.plain_text(StoryRef::Body), "zero one two three");
    window_is_hub(&win, &room);
    assert_eq!(own.stats().sent, 0);
    // What the window changed outside its copy (a direct command) goes as its own edit.
    win.run("caret.docEnd", &json!({})).unwrap();
    win.run("text.insert", &json!({"text": "!"})).unwrap();
    settle_all(&mut own, &mut win, std::slice::from_mut(&mut ben));
    assert_eq!(text(&ben), "zero one two three!");
    assert_eq!(own.stats().sent, 1);
    // A copy made by joining isn't lent a session.
    assert!(ben.lend(&mut win, Replica::sync).is_none());
}

#[test]
fn the_window_s_copy_catches_up_on_the_shared_document_while_another_is_shown() {
    let (mut win, shared, other) = window();
    let (room, mut own) = Room::host(&mut win, "Minoru").unwrap();
    let mut ben = join(&room, "Ben");
    win.activate(other).unwrap();
    run(&mut ben, "caret.docStart", json!({}));
    run(&mut ben, "text.insert", json!({"text": "zero "}));
    ben.sync();
    // Lent with the shared document brought forward, the copy gets Ben's edit; the notes
    // are shown again after, as they were.
    assert_eq!(own.lend_document(&mut win, Replica::receive), Some(()));
    assert_eq!(win.document_id(), other);
    assert_eq!(win.doc.plain_text(StoryRef::Body), "Notes");
    win.activate(shared).unwrap();
    assert_eq!(win.doc.plain_text(StoryRef::Body), "zero one two three");
    window_is_hub(&win, &room);
    // Not for a copy made by joining, nor once the shared document is closed.
    assert!(ben.lend_document(&mut win, Replica::receive).is_none());
    win.activate(other).unwrap();
    win.close_document(shared).unwrap();
    assert!(own.lend_document(&mut win, Replica::receive).is_none());
}

#[test]
fn a_participant_s_commands_reach_no_files() {
    let (mut win, _, _) = window();
    let (room, _own) = Room::host(&mut win, "Minoru").unwrap();
    let lobby = Lobby::default();
    lobby.open(room.clone());
    let dir = std::env::temp_dir().join(format!("wordcraft-hosted-files-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let secret = dir.join("recipients.csv");
    std::fs::write(&secret, "Name,Key\nAda,s3cret\n").unwrap();
    let path = secret.to_string_lossy().to_string();
    // Whatever their role, a document command that reads a file it names is refused, and
    // nothing of the file reaches them; data they send along goes in.
    for role in [Role::Viewer, Role::Suggester, Role::Editor] {
        let key = room.invite(&format!("Agent {role}"), role).unwrap();
        let mut agent = lobby.join_with(&key).unwrap();
        let read = execute(&mut agent, "mailings.recipients", json!({"path": path}));
        assert!(read.as_ref().is_err_and(|e| e.contains("not granted")), "{role}: {read:?}");
        let rows = execute(&mut agent, "mailings.editRecipients", json!({})).unwrap_or_default();
        assert!(!rows.to_string().contains("s3cret"), "{role}: {rows}");
        if role >= Role::Editor {
            execute(&mut agent, "mailings.recipients", json!({"rows": [{"Name": "Bob"}]})).unwrap();
        }
    }
    // The person at the window reads it as before.
    win.run("mailings.recipients", &json!({"path": path})).unwrap();
    let rows = win.run("mailings.editRecipients", &json!({})).unwrap();
    assert!(rows.to_string().contains("s3cret"), "{rows}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_participant_with_a_key_is_answered_by_a_copy_of_their_own() {
    let (mut win, shared, _) = window();
    let (room, mut own) = Room::host(&mut win, "Minoru").unwrap();
    let lobby = Lobby::default();
    assert!(matches!(lobby.join_with("nonsense"), Err(RoomError::Closed)), "nothing is shared yet");
    lobby.open(room.clone());
    let key = room.invite("Copy editor", Role::Suggester).unwrap();
    assert!(matches!(lobby.join_with("nonsense"), Err(RoomError::UnknownKey)));
    let mut agent = lobby.join_with(&key).unwrap();
    // Commands run in their copy, and what they change reaches the hub before the reply.
    execute(&mut agent, "select.text", json!({"text": "two"})).unwrap();
    execute(&mut agent, "text.insert", json!({"text": "2"})).unwrap();
    assert!(!agent.has_unconfirmed());
    assert_eq!(room.version(), 1);
    let inspected = call(&mut agent, "document.inspect", json!({"text": true})).unwrap();
    assert!(inspected.to_string().contains("one 2two three"), "{inspected}");
    // It sees what others changed since.
    in_window(&mut own, &mut win, "caret.docEnd", json!({}));
    in_window(&mut own, &mut win, "text.insert", json!({"text": " four"}));
    let doc = agent.document();
    let inspected = call(&mut agent, "document.inspect", json!({"doc": doc})).unwrap();
    assert!(inspected.to_string().contains("four"), "{inspected}");
    let who = call(&mut agent, "room.participants", json!({})).unwrap();
    assert_eq!(who["you"]["name"], "Copy editor");
    assert_eq!(who["you"]["role"], "suggester");
    assert_eq!(who["version"], 2);
    let names: Vec<&str> = who["participants"].as_array().unwrap().iter().filter_map(|p| p["name"].as_str()).collect();
    assert_eq!(names, ["Minoru", "Copy editor"]);
    // Commands they may run are enabled; files and the window aren't theirs.
    let commands = call(&mut agent, "engine.commands", json!({})).unwrap();
    let enabled = |id: &str| commands.as_array().unwrap().iter().find(|c| c["id"] == id).map(|c| c["enabled"].clone());
    assert_eq!(enabled("text.insert"), Some(json!(true)));
    assert_eq!(enabled("file.save"), Some(json!(false)));
    assert_eq!(enabled("tools.autocorrect"), Some(json!(false)), "the application's settings are the owner's");
    let found = call(&mut agent, "engine.commands", json!({"search": "accept all"})).unwrap();
    assert!(found.as_array().unwrap().iter().all(|c| c.to_string().to_lowercase().contains("accept all")), "{found}");
    // Refused: what their role doesn't allow, other documents, the window and the owner's methods.
    let refused = [
        execute(&mut agent, "file.save", json!({"path": "/tmp/x.docx"})),
        execute(&mut agent, "review.acceptAll", json!({})),
        call(&mut agent, "engine.execute", json!({"command": "text.insert", "params": {"text": "x"}, "doc": shared + 1000})),
        call(&mut agent, "engine.execute", json!({"params": {}})),
        call(&mut agent, "ui.click", json!({"x": 1, "y": 1})),
        call(&mut agent, "app.quit", json!({})),
        call(&mut agent, "room.invite", json!({"name": "Eve", "role": "owner"})),
        call(&mut agent, "nonsense", json!({})),
    ];
    for r in &refused {
        assert!(r.is_err(), "{r:?}");
    }
    assert!(refused[4].as_ref().unwrap_err().contains("window"));
    assert!(refused[6].as_ref().unwrap_err().contains("owner's"));
    settle_all(&mut own, &mut win, std::slice::from_mut(&mut agent));
    window_is_hub(&win, &room);
    // A viewer reads, and changes nothing.
    let key = room.invite("Reader", Role::Viewer).unwrap();
    let mut reader = lobby.join_with(&key).unwrap();
    assert!(execute(&mut reader, "text.insert", json!({"text": "x"})).is_err());
    let commands = call(&mut reader, "engine.commands", json!({})).unwrap();
    let enabled = |id: &str| commands.as_array().unwrap().iter().find(|c| c["id"] == id).map(|c| c["enabled"].clone());
    assert_eq!((enabled("text.insert"), enabled("select.all")), (Some(json!(false)), Some(json!(true))));
    assert!(call(&mut reader, "document.inspect", json!({})).is_ok());
}

/// A shared "one two" with a text box holding "box", whose agent typed in the box `edits` times;
/// before that got to the hub, the person typed over the whole body, which takes the box away.
fn typed_in_a_box_that_went(edits: usize) -> (Session, Replica, Arc<Room>, Replica) {
    let mut win = Session::new(Document::from_text("one two"));
    win.author = "Minoru".into();
    run_in(&mut win, "caret.docEnd");
    run_in(&mut win, "insert.textBox");
    win.run("text.insert", &json!({"text": "box"})).unwrap();
    let in_box = win.sel.focus.clone();
    win.run("caret.set", &json!({"pos": {"story": "body", "path": [0], "off": 0}})).unwrap();
    let (room, mut own) = Room::host(&mut win, "Minoru").unwrap();
    let key = room.invite("Writer", Role::Editor).unwrap();
    let mut agent = room.join_with(&key).unwrap();
    // Run in the copy and not sent yet, as an edit is that others' changes kept sending back.
    agent.run("caret.set", &json!({"pos": in_box})).unwrap();
    for _ in 0..edits {
        agent.run("text.insert", &json!({"text": " more"})).unwrap();
    }
    in_window(&mut own, &mut win, "select.all", json!({}));
    in_window(&mut own, &mut win, "text.insert", json!({"text": "gone"}));
    (win, own, room, agent)
}

#[test]
fn an_edit_whose_text_went_meanwhile_is_reported_lost() {
    let (mut win, mut own, room, mut agent) = typed_in_a_box_that_went(1);
    // The agent's next request says its typing couldn't be made again, and the command it
    // asked for doesn't run, so the error is about the earlier edit alone.
    let r = execute(&mut agent, "caret.docEnd", json!({}));
    assert_eq!(agent.stats().lost, 1);
    let why = r.unwrap_err();
    assert!(why.contains("couldn't be made again (text.insert)"), "{why}");
    assert!(why.contains("caret.docEnd didn't run"), "{why}");
    execute(&mut agent, "caret.docEnd", json!({})).unwrap();
    settle_all(&mut own, &mut win, std::slice::from_mut(&mut agent));
    window_is_hub(&win, &room);
    assert_eq!(win.doc.plain_text(StoryRef::Body), "gone");
}

#[test]
fn a_request_that_only_reads_says_when_an_earlier_edit_was_lost() {
    for method in ["document.inspect", "engine.commands", "ui.render", "room.participants"] {
        let (_win, _own, _room, mut agent) = typed_in_a_box_that_went(1);
        let params = if method == "ui.render" { json!({"scale": 0.25}) } else { json!({}) };
        // Bringing in what the person changed gives the agent's typing up: the request says so,
        // and answers nothing else.
        let why = call(&mut agent, method, params.clone()).unwrap_err();
        assert!(why.contains("couldn't be made again (text.insert)") && why.contains(&format!("{method} didn't run")), "{method}: {why}");
        assert_eq!(agent.stats().lost, 1);
        // Said once: asked again, it answers.
        call(&mut agent, method, params).unwrap();
    }
    // Several are counted.
    let (_win, _own, _room, mut agent) = typed_in_a_box_that_went(3);
    let why = call(&mut agent, "document.inspect", json!({})).unwrap_err();
    assert!(why.contains("couldn't be made again (text.insert)") && why.contains("(3 edits in all)"), "{why}");
    assert_eq!(agent.stats().lost, 3);
}

#[test]
fn every_request_first_sends_what_an_earlier_command_left() {
    let (mut win, _, _) = window();
    let (room, _own) = Room::host(&mut win, "Minoru").unwrap();
    let key = room.invite("Agent", Role::Editor).unwrap();
    let mut agent = room.join_with(&key).unwrap();
    agent.run("caret.docEnd", &json!({})).unwrap();
    for method in ["document.inspect", "engine.commands", "ui.render", "room.participants", "engine.execute"] {
        // An edit the hub hasn't taken yet, as others' changes sending it back leave one.
        agent.run("text.insert", &json!({"text": "!"})).unwrap();
        assert!(agent.has_unconfirmed());
        let params = match method {
            "ui.render" => json!({"scale": 0.25}),
            "engine.execute" => json!({"command": "caret.docEnd"}),
            _ => json!({}),
        };
        call(&mut agent, method, params).unwrap();
        assert!(!agent.has_unconfirmed(), "{method} left an edit unsent");
    }
    let (version, hub) = room.document();
    assert_eq!(hub.plain_text(StoryRef::Body), "one two three!!!!!");
    assert_eq!(agent.received(), version);
}

#[test]
fn an_edit_the_hub_keeps_refusing_is_given_up_with_a_reason() {
    let (mut win, _, _) = window();
    let (room, _own) = Room::host(&mut win, "Minoru").unwrap();
    let key = room.invite("Agent", Role::Editor).unwrap();
    let mut agent = room.join_with(&key).unwrap();
    agent.run("text.insert", &json!({"text": "x"})).unwrap();
    // Out of the room, the agent's sends are refused: each time the copy reads the document again
    // and makes the edit again, until it gives up, saying which command it was.
    assert!(room.revoke(&key));
    for _ in 0..10 {
        agent.send();
    }
    assert_eq!(agent.stats().lost, 1);
    assert!(!agent.has_unconfirmed());
    let why = agent.last_lost();
    assert!(why.contains("couldn't be made again (text.insert): the shared document kept sending it back"), "{why}");
    assert_eq!(agent.session().status, why);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_file_opened_over_a_shared_blank_document_opens_beside_it() {
    let dir = std::env::temp_dir().join(format!("wordcraft-open-over-shared-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("other.txt");
    std::fs::write(&file, "Other").unwrap();
    let mut win = Session::new(Document::new());
    assert!(win.is_untouched_blank());
    let (room, _own) = Room::host(&mut win, "Minoru").unwrap();
    let shared = win.document_id();
    assert!(!win.is_untouched_blank(), "a shared document counts as used");
    win.run("file.open", &json!({"path": file.to_string_lossy()})).unwrap();
    assert!(win.is_open(shared), "the shared document is still open");
    assert_ne!(win.document_id(), shared);
    assert_eq!(win.doc.plain_text(StoryRef::Body), "Other");
    // Once its room closes, a blank document is an untouched one again.
    room.close();
    win.activate(shared).unwrap();
    assert!(win.is_untouched_blank());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_long_macro_is_all_in_the_hub_before_the_reply() {
    let (mut win, _, _) = window();
    let (room, _own) = Room::host(&mut win, "Minoru").unwrap();
    let key = room.invite("Agent", Role::Owner).unwrap();
    let mut agent = room.join_with(&key).unwrap();
    // More steps than four of the hub's batches: the reply comes once it has every one.
    let n = 4 * crate::room::MAX_BATCH + 100;
    let steps: Vec<Value> = (0..n).map(|_| json!({"command": "text.insert", "params": {"text": "x"}})).collect();
    execute(&mut agent, "tools.macros", json!({"define": {"name": "many", "steps": steps}})).unwrap();
    execute(&mut agent, "caret.docEnd", json!({})).unwrap();
    let ran = execute(&mut agent, "tools.macros", json!({"run": "many"})).unwrap();
    assert_eq!(ran["steps"], n);
    assert!(!agent.has_unconfirmed(), "{} changes the hub hasn't taken", agent.unconfirmed());
    let (version, hub) = room.document();
    assert_eq!(hub.plain_text(StoryRef::Body), format!("one two three{}", "x".repeat(n)));
    assert_eq!(agent.received(), version);
}

#[test]
fn revoking_an_invite_or_closing_the_room_ends_the_copies() {
    let (mut win, _, _) = window();
    let (room, mut own) = Room::host(&mut win, "Minoru").unwrap();
    let lobby = Lobby::default();
    lobby.open(room.clone());
    let key = room.invite("Ana", Role::Editor).unwrap();
    let mut ana = lobby.join_with(&key).unwrap();
    let mut ana_too = lobby.join_with(&key).unwrap();
    let ben_key = room.invite("Ben", Role::Editor).unwrap();
    let mut ben = lobby.join_with(&ben_key).unwrap();
    assert_eq!(room.participants().len(), 4);
    // Revoked: both of Ana's copies are out; the hub takes no change of theirs.
    assert!(room.revoke(&key));
    for a in [&mut ana, &mut ana_too] {
        assert!(hosted::ended(a).is_some_and(|why| why.contains("taken back")));
        assert!(execute(a, "text.insert", json!({"text": "x"})).is_err());
        a.run("text.insert", &json!({"text": "x"})).unwrap();
        a.sync();
    }
    assert_eq!(room.version(), 0);
    assert!(!room.is_in(ana.id()));
    let names: Vec<String> = room.participants().into_iter().map(|p| p.name).collect();
    assert_eq!(names, ["Minoru", "Ben"]);
    // Their name stays theirs: their suggestions and comments carry it.
    assert!(matches!(room.invite("ana", Role::Viewer), Err(RoomError::Name(_))));
    // A copy that's dropped leaves.
    let cy = join(&room, "Cy");
    let cy_id = cy.id();
    assert!(room.is_in(cy_id));
    drop(cy);
    assert!(!room.is_in(cy_id));
    // Closed: no one joins, the hub takes nothing, hosted copies stop answering.
    execute(&mut ben, "text.insert", json!({"text": "Ben "})).unwrap();
    lobby.close();
    assert!(room.is_closed());
    assert!(lobby.room().is_none());
    assert!(matches!(lobby.join_with(&ben_key), Err(RoomError::Closed)));
    assert!(matches!(room.join_with(&ben_key), Err(RoomError::Closed)));
    assert!(matches!(room.join("Dee", Role::Editor), Err(RoomError::Closed)));
    assert!(matches!(room.invite("Dee", Role::Editor), Err(RoomError::Closed)));
    assert!(hosted::ended(&ben).is_some_and(|why| why.contains("isn't shared")));
    assert!(call(&mut ben, "document.inspect", json!({})).is_err());
    let version = room.version();
    in_window(&mut own, &mut win, "text.insert", json!({"text": "late "}));
    assert_eq!(room.version(), version);
    // It stays readable, and a wait for a change doesn't wait.
    assert!(room.document().1.plain_text(StoryRef::Body).contains("Ben"));
    let t0 = std::time::Instant::now();
    room.wait_past(version, Duration::from_secs(5));
    assert!(t0.elapsed() < Duration::from_secs(1));
}

#[test]
fn a_lobby_opened_on_another_room_closes_the_one_before() {
    let lobby = Lobby::default();
    let first = Room::new(Document::from_text("first"));
    let second = Room::new(Document::from_text("second"));
    lobby.open(first.clone());
    lobby.open(first.clone());
    assert!(!first.is_closed(), "opening the same room again keeps it");
    lobby.open(second.clone());
    assert!(first.is_closed());
    assert!(lobby.room().is_some_and(|r| Arc::ptr_eq(&r, &second)));
    assert_eq!(format!("{lobby:?}"), "Lobby { open: true }");
}

#[test]
fn the_room_tells_the_window_when_it_has_changes() {
    let room = Room::new(Document::from_text("Hello"));
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    room.on_change(move || {
        seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    let mut ana = join(&room, "Ana");
    run(&mut ana, "text.insert", json!({"text": "Hi "}));
    ana.sync();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    // Nothing to send: no call.
    ana.sync();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    room.close();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[test]
fn a_window_whose_session_has_unnamed_blocks_shares_them_with_ids() {
    // A document whose blocks have no ids yet, put in outside a command.
    let mut win = Session::new(Document::new());
    win.doc = Document::from_text("one\ntwo");
    assert!(win.doc.block_ids().contains(&BlockId::UNSET));
    let (room, mut own) = Room::host(&mut win, "Minoru").unwrap();
    window_is_hub(&win, &room);
    assert!(!win.doc.block_ids().contains(&BlockId::UNSET));
    in_window(&mut own, &mut win, "text.insert", json!({"text": "zero "}));
    frame(&mut own, &mut win);
    window_is_hub(&win, &room);
    assert_eq!((own.stats().lost, own.stats().read_again), (0, 0));
}

#[test]
fn hosted_copies_on_threads_and_the_window_typing_at_once_end_up_the_same() {
    const AGENTS: usize = 2;
    let (mut win, _, _) = window();
    let (room, mut own) = Room::host(&mut win, "Minoru").unwrap();
    let lobby = Lobby::default();
    lobby.open(room.clone());
    let keys: Vec<String> =
        (0..AGENTS).map(|i| room.invite(&format!("agent {i}"), if i == 0 { Role::Editor } else { Role::Suggester }).unwrap()).collect();
    let start = Arc::new(Barrier::new(AGENTS + 1));
    let docs: Vec<Result<Document, String>> = std::thread::scope(|scope| {
        let agents: Vec<_> = keys
            .iter()
            .enumerate()
            .map(|(i, key)| {
                let (lobby, start) = (lobby.clone(), start.clone());
                scope.spawn(move || {
                    let mut copy = lobby.join_with(key).map_err(|e| e.to_string())?;
                    start.wait();
                    let place = if i == 0 { "caret.docStart" } else { "caret.docEnd" };
                    execute(&mut copy, place, json!({}))?;
                    for n in 0..25 {
                        execute(&mut copy, "text.insert", json!({"text": format!("{i}{}", n % 10)}))?;
                        if n % 8 == 5 {
                            execute(&mut copy, "text.newParagraph", json!({}))?;
                        }
                    }
                    copy.receive();
                    Ok(copy.session().doc.clone())
                })
            })
            .collect();
        start.wait();
        // The person types in the middle meanwhile, a frame between keys.
        in_window(&mut own, &mut win, "select.text", json!({"text": "two"}));
        in_window(&mut own, &mut win, "caret.right", json!({}));
        for n in 0..40 {
            in_window(&mut own, &mut win, "text.insert", json!({"text": format!("{}", n % 10)}));
            frame(&mut own, &mut win);
            if n % 10 == 9 {
                room.wait_past(own.received(), Duration::from_millis(1));
            }
        }
        agents.into_iter().map(|t| t.join().unwrap_or_else(|_| Err("an agent panicked".into()))).collect()
    });
    settle_all(&mut own, &mut win, &mut []);
    window_is_hub(&win, &room);
    assert_eq!((own.stats().lost, own.stats().read_again), (0, 0));
    let typed: String = (0..40).map(|n| char::from(b'0' + (n % 10) as u8)).collect();
    assert!(win.doc.plain_text(StoryRef::Body).contains(&format!("two{typed}")), "the person's keys stay in order");
    for (i, doc) in docs.into_iter().enumerate() {
        let doc = doc.unwrap();
        assert!(doc.plain_text(StoryRef::Body).contains(&format!("{i}9")), "agent {i}'s copy has its own edits");
    }
    let mut accepted = Session::new(win.doc.clone());
    run_in(&mut accepted, "review.acceptAll");
    let all = accepted.doc.plain_text(StoryRef::Body);
    for i in 0..AGENTS {
        assert_eq!(all.matches(&format!("{i}9")).count(), 2, "agent {i}'s edits all went in: {all}");
    }
}

#[test]
fn a_copy_made_for_a_session_refuses_what_a_copy_refuses() {
    let (mut win, _, _) = window();
    let (_room, mut own) = Room::host(&mut win, "Minoru").unwrap();
    let r = own.lend(&mut win, |c| c.run("file.save", &json!({"path": "/tmp/x.docx"}))).unwrap();
    assert!(matches!(r, Err(CmdError::Disabled(_))));
    // The owner changes the application's settings.
    let r = own.lend(&mut win, |c| c.run("tools.autocorrect", &json!({"capSentences": false}))).unwrap();
    assert!(r.is_ok(), "{r:?}");
}

#[test]
fn a_participant_s_copy_renders_pages_follows_changes_and_takes_command_ids_as_methods() {
    let (mut win, _, _) = window();
    let (room, mut own) = Room::host(&mut win, "Minoru").unwrap();
    let lobby = Lobby::default();
    lobby.open(room.clone());
    let key = room.invite("Agent", Role::Editor).unwrap();
    let mut agent = lobby.join_with(&key).unwrap();
    // A command id is a method, with its params, as the window takes it.
    call(&mut agent, "caret.docEnd", json!({})).unwrap();
    call(&mut agent, "text.insert", json!({"text": " four"})).unwrap();
    assert_eq!(room.version(), 1);
    // A page comes back as PNG in the reply, with what others changed since: here the window
    // bolds everything, " four" included once a frame brought it in.
    frame(&mut own, &mut win);
    in_window(&mut own, &mut win, "select.all", json!({}));
    in_window(&mut own, &mut win, "format.bold", json!({}));
    let page = call(&mut agent, "ui.render", json!({"page": 1, "scale": 0.5})).unwrap();
    assert_eq!(bold(&agent.session().doc), "one two three four");
    let png = crate::cmd::insert::base64_decode(page["png"].as_str().unwrap()).unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(page["pages"], 1);
    let (w, h) = (page["width"].as_u64().unwrap(), page["height"].as_u64().unwrap());
    assert!(w > 100 && h > w, "{w}×{h}");
    // The default scale is 2 pixels a point, and a hostile one is held to 0.1–8.
    let width = |v: Value| v["width"].as_f64().unwrap();
    let near = |got: f64, want: f64| (got - want).abs() <= 2.0;
    let wide = width(call(&mut agent, "ui.render", json!({})).unwrap());
    assert!(near(wide, w as f64 * 4.0), "{wide}");
    let narrow = width(call(&mut agent, "ui.render", json!({"scale": -1e30})).unwrap());
    assert!(near(narrow, w as f64 / 5.0), "{narrow}");
    // A copy writes no files, and has the pages its document has.
    assert!(call(&mut agent, "ui.render", json!({"path": "/tmp/page.png"})).unwrap_err().contains("path"));
    assert!(call(&mut agent, "ui.render", json!({"page": 9})).unwrap_err().contains("no page 9"));
    // What changed, by the agent and the window alike, in the copy's own versions.
    let changes = call(&mut agent, "document.changes", json!({"since": 0})).unwrap();
    assert!(changes["changes"].as_array().is_some_and(|c| c.len() >= 2), "{changes}");
    // The feature catalog's parity, as the window gives it.
    let parity = call(&mut agent, "ui.parity", json!({})).unwrap();
    assert!(parity["live"].as_u64().unwrap() > 100, "{parity}");
    // Ids that aren't commands, or are commands the copy refuses, stay errors.
    assert!(call(&mut agent, "nonsense", json!({})).is_err());
    assert!(call(&mut agent, "file.save", json!({"path": "/tmp/x.docx"})).is_err());
    assert!(call(&mut agent, "ui.screenshot", json!({})).unwrap_err().contains("window"));
    settle_all(&mut own, &mut win, std::slice::from_mut(&mut agent));
    window_is_hub(&win, &room);
}

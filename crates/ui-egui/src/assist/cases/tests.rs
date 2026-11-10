//! What a turn gets from the case bound to its document, and what it never gets: another client's
//! text, a model's unconfirmed note from another case, or anything while someone else can read
//! the document.

use serde_json::{Value, json};
use wordcraft_assist::cases::{BalanceDraft, Body, CaseRef, Click, Day, ItemId, NoteDraft, PersonOp, Quoted, Shelf, Stance, Topics, WallId};
use wordcraft_assist::knowledge::SourceOrigin;
use wordcraft_assist::memory::Memory;
use wordcraft_assist::{Caps, Context, Entry, Fake, Seen, Step, Turn};
use wordcraft_engine::room::Role;

use crate::WordApp;
use crate::assist::memory::tests::{Folder, settle, window};

const HERE: &str = "Cap liability at twelve months of fees, and say so early.";
const NEIGHBOR: &str = "The indemnity carve-out for IP claims held once we offered a cap.";
const UNCONFIRMED_CANARY: &str = "Canary: a model's unconfirmed note in the neighbour.";
const OTHER_WALL_CANARY: &str = "Canary: another client's note on the liability cap.";
const TODAY: &str = "2026-11-10";

fn today() -> Day {
    Day::parse(TODAY).unwrap()
}

fn click() -> Click {
    Click::by_person(true).unwrap()
}

fn draft(text: &str) -> NoteDraft {
    NoteDraft { body: Body::Pattern { rule: Quoted::parse(text), traps: None }, how: None, topics: Topics::default() }
}

/// Two walls: the bound case and a neighbour in one, a case with a canary in the other.
struct Cases {
    here: CaseRef,
    neighbor: CaseRef,
}

fn cases(folder: &Folder) -> Cases {
    let shelf = Shelf::at(&folder.0);
    let wall = |n: u64| -> WallId { shelf.new_wall(click(), &format!("Client {n}"), TODAY, n).unwrap() };
    let case = |wall: &WallId, title: &str, n: u64| shelf.new_case(click(), wall, title, TODAY, n).unwrap();
    let a = wall(1);
    let here = case(&a, "Acme MSA", 1);
    let neighbor = case(&a, "Acme SaaS renewal", 2);
    let b = wall(2);
    let other = case(&b, "Zenith licence", 3);
    shelf.person(click(), &here, PersonOp::AddNote(draft(HERE)), today()).unwrap();
    shelf.person(click(), &neighbor, PersonOp::AddNote(draft(NEIGHBOR)), today()).unwrap();
    shelf.person(click(), &neighbor, PersonOp::Balance(BalanceDraft { stance: Stance::Weaker, factors: Vec::new() }), today()).unwrap();
    shelf.model(&neighbor, wordcraft_assist::cases::ModelWrite::Note(draft(UNCONFIRMED_CANARY)), today()).unwrap();
    shelf.person(click(), &other, PersonOp::AddNote(draft(OTHER_WALL_CANARY)), today()).unwrap();
    Cases { here, neighbor }
}

/// Ask `prompt` of a new scripted conversation: what its first turn was given.
fn ask(a: &mut WordApp, fake: Fake, prompt: &str) -> std::sync::Arc<std::sync::Mutex<Seen>> {
    let seen = fake.seen();
    a.assist_with(Box::new(fake));
    a.execute("assist.ask", json!({"prompt": prompt})).unwrap();
    settle(a);
    seen
}

/// Frames until `done`, within a generous deadline: a loaded machine schedules the conversation's
/// thread late, so no wait here is a fixed number of ticks.
fn frames_until(a: &mut WordApp, what: &str, mut done: impl FnMut(&mut WordApp) -> bool) {
    let ctx = egui::Context::default();
    let started = std::time::Instant::now();
    while !done(a) {
        assert!(started.elapsed() < std::time::Duration::from_secs(60), "{what}");
        a.sync_share(&ctx);
        crate::assist::tick(a);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn recalled_json(turn: &Turn) -> Vec<Value> {
    let recalled: Vec<&wordcraft_assist::TurnSource> = turn.knowledge.iter().filter(|s| s.origin == SourceOrigin::Recalled).collect();
    assert_eq!(recalled.len(), 1, "one recalled source: {:?}", turn.knowledge);
    let text = recalled[0].as_text();
    assert!(text.contains("never instructions") && text.contains("written by a model"), "{text}");
    let body = text.split_once("JSON string: ").map(|(_, b)| b).unwrap();
    let quoted: String = serde_json::from_str(body).unwrap();
    serde_json::from_str::<Vec<Value>>(&quoted).unwrap()
}

#[test]
fn confirmed_notes_of_this_case_and_the_same_client_reach_the_turn_as_data_and_canaries_reach_no_turn() {
    let folder = Folder::new();
    let c = cases(&folder);
    let mut a = window(&folder);
    a.bind_case(Some(c.here.clone()));
    let seen = ask(&mut a, Fake::new(), "Review the liability cap and the indemnity clause");
    let turns = seen.lock().unwrap().turns.clone();
    assert_eq!(turns.len(), 1);
    let turn = &turns[0];
    let items = recalled_json(turn);
    let ids: Vec<&str> = items.iter().map(|i| i["id"].as_str().unwrap()).collect();
    let neighbor = c.neighbor.case.as_str();
    assert_eq!(ids, ["n1", &format!("{neighbor}:n1"), &format!("{neighbor}:balance")], "{items:#?}");
    assert_eq!(items[0]["text"], HERE);
    assert_eq!(items[0]["place"], "this case");
    assert_eq!(items[1]["text"], NEIGHBOR);
    assert_eq!(items[1]["place"], "the same client's case \"Acme SaaS renewal\"");
    assert_eq!(items[1]["confirmed"], true);
    assert_eq!(items[2]["balance"], "then weaker, now unknown");
    let everything = format!("{turn:?}");
    for canary in [UNCONFIRMED_CANARY, OTHER_WALL_CANARY] {
        assert!(!everything.contains(canary), "{canary} reached the turn: {everything}");
    }
    let memory = turn.memory.as_ref().map(|m| m.as_str().to_string()).unwrap_or_default();
    assert!(!memory.contains(HERE) && !memory.contains(NEIGHBOR), "recalled text is never an instruction: {memory}");
    assert!(!turn.prompt.contains(HERE) && !turn.context.as_deref().unwrap_or_default().contains(HERE));

    let details = crate::assist::details_for(&mut a);
    let recalled = details.turns[0].recalled.as_ref().unwrap();
    assert_eq!(recalled.items.len(), 3);
    assert_eq!(recalled.items[0].id, ItemId::Here { note: wordcraft_assist::cases::NoteId::parse("n1").unwrap() });
    assert_eq!(recalled.items[1].title.as_deref(), Some("Acme SaaS renewal"));
    assert_eq!(details.turns[0].context.as_ref().unwrap().recall_withheld, None);
}

/// A neighbour whose `case.json` is a link to another wall's case is read as damaged, not followed:
/// the folder is the ethical wall, and a link would carry another client's text across it.
#[cfg(unix)]
#[test]
fn a_neighbour_whose_case_file_links_to_another_wall_contributes_nothing() {
    let folder = Folder::new();
    let c = cases(&folder);
    let shelf = Shelf::at(&folder.0);
    let listing = shelf.list();
    let other = listing.walls.iter().find(|w| w.id != c.here.wall).unwrap();
    let other_file = folder.0.join("assistant-cases").join(other.id.as_str()).join(other.cases[0].id.as_str()).join("case.json");
    assert!(std::fs::read_to_string(&other_file).unwrap().contains(OTHER_WALL_CANARY));
    let linked = shelf.new_case(click(), &c.here.wall, "Acme linked", TODAY, 9).unwrap();
    let linked_file = folder.0.join("assistant-cases").join(linked.wall.as_str()).join(linked.case.as_str()).join("case.json");
    std::fs::remove_file(&linked_file).unwrap();
    std::os::unix::fs::symlink(&other_file, &linked_file).unwrap();
    let mut a = window(&folder);
    a.bind_case(Some(c.here.clone()));
    let seen = ask(&mut a, Fake::new(), "Review the liability cap and the indemnity clause");
    let turn = seen.lock().unwrap().turns[0].clone();
    let everything = format!("{turn:?}");
    assert!(!everything.contains(OTHER_WALL_CANARY), "the link was followed: {everything}");
    assert_eq!(recalled_json(&turn).len(), 3, "this case and the real neighbour still recall");
    let details = crate::assist::details_for(&mut a);
    let recalled = details.turns[0].recalled.as_ref().unwrap();
    assert_eq!(recalled.problem.as_deref(), Some("1 of this client's cases couldn't be read"));
}

/// A conversation that hasn't read case notes yet runs its turn without recall while someone else
/// can read the document, and Details says who. (One that has read them is refused instead.)
#[test]
fn a_second_participant_or_an_invite_not_yet_used_withholds_recall_and_details_says_who() {
    let folder = Folder::new();
    let c = cases(&folder);
    let mut a = window(&folder);
    a.bind_case(Some(c.here.clone()));
    a.share_document(None).unwrap();
    let room = a.shared_room().unwrap();
    let key = room.invite("Bob", Role::Viewer).unwrap();
    let seen = ask(&mut a, Fake::new(), "Review the liability cap");
    let turns = seen.lock().unwrap().turns.clone();
    assert!(turns[0].knowledge.is_empty(), "an invite not yet used withholds recall: {:?}", turns[0].knowledge);
    let details = crate::assist::details_for(&mut a);
    assert_eq!(details.turns[0].context.as_ref().unwrap().recall_withheld.as_deref(), Some("Bob can read this document"));
    assert!(details.turns[0].recalled.is_none());

    let _bob = room.join_with(&key).unwrap();
    a.execute("assist.ask", json!({"prompt": "Review the liability cap once more"})).unwrap();
    settle(&mut a);
    let turns = seen.lock().unwrap().turns.clone();
    assert_eq!(turns.len(), 2);
    assert!(turns[1].knowledge.is_empty(), "a participant withholds recall");
    let details = crate::assist::details_for(&mut a);
    assert_eq!(details.turns[1].context.as_ref().unwrap().recall_withheld.as_deref(), Some("Bob can read this document"));
}

#[test]
fn with_no_case_bound_a_turn_is_exactly_as_today() {
    let folder = Folder::new();
    let _ = cases(&folder);
    Memory::update(&folder.0, |m| m.add("Use numbered lists.", "2026-11-09T00:00:00Z")).unwrap();
    let mut a = window(&folder);
    let expected = Context::of(&a.session, &Caps::default()).rendered(&Caps::default()).0;
    let seen = ask(&mut a, Fake::new(), "Review the liability cap");
    let turn = seen.lock().unwrap().turns[0].clone();
    assert!(turn.knowledge.is_empty(), "{:?}", turn.knowledge);
    assert_eq!(turn.context.as_deref(), Some(expected.as_str()));
    assert_eq!(turn.memory, Memory::load(&folder.0).for_model().text);
    let details = crate::assist::details_for(&mut a);
    assert_eq!(details.turns[0].context.as_ref().unwrap().recall_withheld.as_deref(), Some("no case is bound to this document"));
    assert!(details.turns[0].recalled.is_none());
}

#[test]
fn a_document_bound_to_another_case_gets_a_new_conversation() {
    let folder = Folder::new();
    let c = cases(&folder);
    let mut a = window(&folder);
    a.bind_case(Some(c.here.clone()));
    let seen = ask(&mut a, Fake::new(), "Hello");
    assert_eq!(a.assist.conversation.as_ref().unwrap().case, Some(c.here.clone()));
    a.bind_case(Some(c.neighbor.clone()));
    a.execute("assist.ask", json!({"prompt": "Hello again"})).unwrap();
    settle(&mut a);
    assert_eq!(seen.lock().unwrap().turns.len(), 1, "the first conversation had no second turn");
    assert_eq!(a.assist.conversation.as_ref().unwrap().case, Some(c.neighbor.clone()));
    a.bind_case(None);
    a.execute("assist.ask", json!({"prompt": "Hello once more"})).unwrap();
    settle(&mut a);
    assert_eq!(a.assist.conversation.as_ref().unwrap().case, None, "unbinding starts another");
}

#[test]
fn a_used_marker_for_a_recalled_item_becomes_a_chip_and_one_the_turn_didnt_send_does_nothing() {
    let folder = Folder::new();
    let c = cases(&folder);
    let mut a = window(&folder);
    a.bind_case(Some(c.here.clone()));
    let fake = Fake::new().then(vec![Step::say("Cap it early[[used: n1]], as before[[used: n99]].")]);
    ask(&mut a, fake, "Review the liability cap");
    let transcript = a.conversation().unwrap().transcript();
    let used: Vec<&str> = transcript.entries().filter_map(|e| if let Entry::Used { id } = e { Some(id.as_str()) } else { None }).collect();
    assert_eq!(used, ["n1"]);
    let answer = transcript.last_answer().unwrap();
    assert!(answer.contains("[[used: 1 n1]]") && !answer.contains("n99"), "{answer}");
    let entry = transcript.entries().position(|e| matches!(e, Entry::Answer { .. })).unwrap();
    assert_eq!(transcript.sent_preference(entry, "n1"), Some(HERE), "the chip's words");
}

/// Once a conversation has read case notes, the model holds them, so someone joining the document
/// doesn't just turn recall off: the next turn is refused and nothing reaches the backend.
fn refused_once_someone_can_read(join: bool) {
    let folder = Folder::new();
    let c = cases(&folder);
    let mut a = window(&folder);
    a.bind_case(Some(c.here.clone()));
    let seen = ask(&mut a, Fake::new(), "Review the liability cap");
    assert_eq!(recalled_json(&seen.lock().unwrap().turns[0]).len(), 3, "alone, it recalls");
    let room = a.shared_room().unwrap();
    let key = room.invite("Bob", Role::Viewer).unwrap();
    let _bob = join.then(|| room.join_with(&key).unwrap());
    let refused = a.execute("assist.ask", json!({"prompt": "Review the liability cap again"})).unwrap_err();
    assert!(refused.contains("Bob can now read this document") && refused.contains("Start a new conversation"), "{refused}");
    settle(&mut a);
    assert_eq!(seen.lock().unwrap().turns.len(), 1, "nothing reached the backend");
    assert!(!a.conversation().unwrap().is_busy());
}

#[test]
fn a_conversation_that_recalled_is_refused_a_turn_once_an_invite_is_out() {
    refused_once_someone_can_read(false);
}

#[test]
fn a_conversation_that_recalled_is_refused_a_turn_once_someone_joined() {
    refused_once_someone_can_read(true);
}

/// The flag that a conversation recalled comes from the event itself, not from a scan of entries:
/// one answer piece with a marker makes more entries than events, which a scan by event count
/// would miss, and the refusal would fail open.
#[test]
fn a_turn_whose_answer_makes_more_entries_than_events_still_counts_as_having_recalled() {
    let folder = Folder::new();
    let c = cases(&folder);
    let mut a = window(&folder);
    a.bind_case(Some(c.here.clone()));
    let fake = Fake::new().then(vec![Step::say("Cap it[[remember: Cap liability early.]] early[[used: n1]] now.")]);
    let seen = fake.seen();
    a.assist_with(Box::new(fake));
    a.execute("assist.ask", json!({"prompt": "Review the liability cap"})).unwrap();
    // Every event of the turn is queued before the window polls once.
    let started = std::time::Instant::now();
    while seen.lock().unwrap().turns.is_empty() && started.elapsed() < std::time::Duration::from_secs(20) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    settle(&mut a);
    assert_eq!(recalled_json(&seen.lock().unwrap().turns[0]).len(), 3);
    let room = a.shared_room().unwrap();
    room.invite("Bob", Role::Viewer).unwrap();
    let refused = a.execute("assist.ask", json!({"prompt": "Again"})).unwrap_err();
    assert!(refused.contains("Start a new conversation"), "{refused}");
    assert_eq!(seen.lock().unwrap().turns.len(), 1);
}

/// Someone joining while a turn that recalled is still running: the frame stops the turn, ends the
/// conversation and says why, so the newcomer reads no edit made with the case notes in mind.
#[test]
fn someone_joining_mid_turn_ends_a_conversation_that_recalled_and_the_pane_says_why() {
    let folder = Folder::new();
    let c = cases(&folder);
    let mut a = window(&folder);
    a.bind_case(Some(c.here.clone()));
    let fake = Fake::new().then(vec![Step::say("Working on it."), Step::Hold]);
    let seen = fake.seen();
    a.assist_with(Box::new(fake));
    a.execute("assist.ask", json!({"prompt": "Review the liability cap"})).unwrap();
    // The Recalled event comes before the backend's turn starts, so the turn is waited for too.
    frames_until(&mut a, "the turn never recalled", |a| a.conversation().unwrap().recalled());
    frames_until(&mut a, "the backend never got the turn", |_| !seen.lock().unwrap().turns.is_empty());
    assert!(a.conversation().unwrap().is_busy(), "the fake holds its turn");
    assert_eq!(recalled_json(&seen.lock().unwrap().turns[0]).len(), 3);
    let room = a.shared_room().unwrap();
    let key = room.invite("Bob", Role::Viewer).unwrap();
    let _bob = room.join_with(&key).unwrap();
    crate::assist::tick(&mut a);
    assert!(a.conversation().unwrap().has_ended(), "the conversation ended on the frame");
    let notice = a.assist.notice.clone().unwrap_or_default();
    assert!(notice.contains("Bob can now read this document") && notice.contains("case notes"), "{notice}");
    settle(&mut a);
    assert!(!a.conversation().unwrap().is_busy());
    // Ending never waits for the conversation's thread, which tells the backend.
    frames_until(&mut a, "the backend was never told to stop", |_| seen.lock().unwrap().ended);
}

#[test]
fn someone_joining_while_another_document_is_shown_still_ends_a_conversation_that_recalled() {
    let folder = Folder::new();
    let c = cases(&folder);
    let mut a = window(&folder);
    a.bind_case(Some(c.here.clone()));
    let fake = Fake::new().then(vec![Step::say("Working on it."), Step::Hold]);
    let seen = fake.seen();
    a.assist_with(Box::new(fake));
    a.execute("assist.ask", json!({"prompt": "Review the liability cap"})).unwrap();
    frames_until(&mut a, "the turn never recalled", |a| a.assist.conversation.as_ref().unwrap().assistant.recalled());
    frames_until(&mut a, "the backend never got the turn", |_| !seen.lock().unwrap().turns.is_empty());
    let room = a.shared_room().unwrap();
    a.execute("file.new", json!({})).unwrap();
    let key = room.invite("Bob", Role::Viewer).unwrap();
    let _bob = room.join_with(&key).unwrap();
    crate::assist::tick(&mut a);
    assert!(a.assist.conversation.as_ref().unwrap().assistant.has_ended(), "the conversation ended though its document isn't shown");
    settle(&mut a);
    frames_until(&mut a, "the backend was never told to stop", |_| seen.lock().unwrap().ended);
}

/// The conversation counts as having recalled from the moment it sends a turn with a recall ask,
/// not from the frame that polls the `Recalled` event: the thread may be reading the files when
/// someone joins, and the guard must already cover it.
#[test]
fn someone_joining_before_the_recalled_event_is_polled_ends_a_conversation_that_asked_to_recall() {
    let folder = Folder::new();
    let c = cases(&folder);
    let mut a = window(&folder);
    a.bind_case(Some(c.here.clone()));
    let fake = Fake::new().then(vec![Step::say("Working on it."), Step::Hold]);
    let seen = fake.seen();
    a.assist_with(Box::new(fake));
    a.execute("assist.ask", json!({"prompt": "Review the liability cap"})).unwrap();
    assert!(a.conversation().unwrap().recalled(), "a turn sent with a recall ask counts, before any event is polled");
    let room = a.shared_room().unwrap();
    room.invite("Bob", Role::Viewer).unwrap();
    crate::assist::tick(&mut a);
    assert!(a.conversation().unwrap().has_ended(), "the conversation ended on the frame");
    let notice = a.assist.notice.clone().unwrap_or_default();
    assert!(notice.contains("Bob can now read this document"), "{notice}");
    frames_until(&mut a, "the backend was never told to stop", |_| seen.lock().unwrap().ended);
}

/// Binding the document to another case ends the conversation bound to the first: a running
/// turn holds the first case's notes, and the next ask would only replace the conversation.
#[test]
fn rebinding_the_document_ends_the_conversation_bound_to_the_other_case_and_stops_its_turn() {
    let folder = Folder::new();
    let c = cases(&folder);
    let mut a = window(&folder);
    a.bind_case(Some(c.here.clone()));
    let fake = Fake::new().then(vec![Step::say("Working on it."), Step::Hold]);
    let seen = fake.seen();
    a.assist_with(Box::new(fake));
    a.execute("assist.ask", json!({"prompt": "Review the liability cap"})).unwrap();
    frames_until(&mut a, "the backend never got the turn", |_| !seen.lock().unwrap().turns.is_empty());
    assert!(a.conversation().unwrap().is_busy(), "the fake holds its turn");
    a.bind_case(Some(c.neighbor.clone()));
    assert!(a.conversation().unwrap().has_ended(), "the conversation bound to the other case ended");
    frames_until(&mut a, "the backend was never told to stop", |_| seen.lock().unwrap().ended);
    assert!(!a.conversation().unwrap().is_busy());
}

/// The pane's writes, with what a person types in it.
fn writes() -> Vec<(&'static str, Value)> {
    let id = "0123456789abcdef";
    vec![
        ("assist.memory.wall.new", json!({"name": "Planted client"})),
        ("assist.memory.case.new", json!({"title": "Planted case"})),
        ("assist.memory.case.bind", json!({"wall": id, "case": id})),
        ("assist.memory.case.unbind", json!({})),
        ("assist.memory.case.facts", json!({"facts": {"kind": "saas"}})),
        ("assist.memory.case.balance", json!({"stance": "weaker"})),
        ("assist.memory.case.plan", json!({"goal": "Planted"})),
        ("assist.memory.case.note", json!({"note": {"kind": "pattern", "rule": "Planted"}})),
        ("assist.memory.case.noteEdit", json!({"id": "n1", "note": {"kind": "pattern", "rule": "Planted"}})),
        ("assist.memory.case.noteConfirm", json!({"id": "n1"})),
        ("assist.memory.case.noteRemove", json!({"id": "n1"})),
        ("assist.memory.case.delete", json!({})),
        ("assist.memory.wall.delete", json!({"wall": id})),
    ]
}

fn cases_dir(folder: &Folder) -> std::path::PathBuf {
    folder.0.join(wordcraft_assist::cases::store::FOLDER)
}

/// A script with the window's key, and a macro's step, write no case: a document could plant one,
/// and a case outlasts the document. The person's own click does.
#[test]
fn a_script_or_a_macro_step_cant_write_a_case() {
    let folder = Folder::new();
    let mut a = window(&folder);
    for (id, params) in writes() {
        assert_eq!(a.execute(id, params.clone()), Err(crate::assist::memory::YOURS.to_string()), "{id}");
    }
    a.assist.by_person = true;
    for (id, params) in writes() {
        let mut req = params.clone();
        req["assist"] = json!(id.trim_start_matches("assist."));
        a.handle_request(&req);
    }
    a.assist.by_person = false;
    assert!(!cases_dir(&folder).exists(), "a script or a macro's step made a case folder");
    a.run("assist.memory.case.new", json!({"title": "Mine", "client": "Acme"})).unwrap();
    let listing = Shelf::at(&folder.0).list();
    assert_eq!(listing.walls.len(), 1);
    assert_eq!(listing.walls[0].cases[0].title.for_person(), "Mine");
    assert_eq!(crate::assist::cases::bound_for(&a), Some(CaseRef { wall: listing.walls[0].id.clone(), case: listing.walls[0].cases[0].id.clone() }));
}

/// A click the control channel made on Create makes no case, and the person's own does.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_control_click_on_create_makes_no_case() {
    use egui_kittest::kittest::Queryable;
    let folder = Folder::new();
    let mut a = window(&folder);
    a.session.view.assistant_pane = true;
    a.assist.view = crate::assist::PaneView::Details;
    a.assist.cases.drafts.new_case.title = "Planted".into();
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1440.0, 3000.0)).build_ui_state(
        |ui, app: &mut WordApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        a,
    );
    h.run_steps(4);
    crate::assist::memory::tests::control_click(&mut h, "Create");
    assert!(!cases_dir(&folder).exists(), "the control channel's click made a case");
    h.get_by_label("Create").click();
    h.run_steps(3);
    let listing = Shelf::at(&folder.0).list();
    assert_eq!(listing.walls[0].cases[0].title.for_person(), "Planted", "the person's click did");
    assert_eq!(listing.walls[0].name.for_person(), "My cases");
    assert!(crate::assist::cases::bound_for(h.state()).is_some(), "and bound the document to it");
}

/// Create by client name while the first listing is still being read finds the client in a
/// listing made on the spot, so no second client of the name appears.
#[test]
fn creating_a_case_by_client_name_while_the_listing_is_read_makes_no_second_client() {
    let folder = Folder::new();
    let _ = cases(&folder);
    let mut a = window(&folder);
    let (_held, rx) = std::sync::mpsc::channel();
    a.assist.cases.listing = super::ListState { last: None, pending: Some(rx) };
    a.assist.cases.listed_at = Some(crate::clock::now_ms());
    a.run("assist.memory.case.new", json!({"title": "Acme NDA", "client": "Client 1"})).unwrap();
    let listing = Shelf::at(&folder.0).list();
    assert_eq!(listing.walls.iter().filter(|w| w.name.for_person() == "Client 1").count(), 1, "{listing:?}");
    assert_eq!(listing.walls.len(), 2);
    let at = crate::assist::cases::bound_for(&a).unwrap();
    let wall = listing.walls.iter().find(|w| w.id == at.wall).unwrap();
    assert_eq!(wall.name.for_person(), "Client 1");
    assert_eq!(wall.cases.len(), 3, "the new case joined the client's two");
}

/// Binding through the command makes the next turn recall, unbinding stops it, and binding another
/// case ends the conversation that ran under the first.
#[test]
fn binding_through_the_commands_turns_recall_on_and_off_and_rebinding_ends_the_conversation() {
    let folder = Folder::new();
    let c = cases(&folder);
    let mut a = window(&folder);
    a.run("assist.memory.case.bind", json!({"wall": c.here.wall.as_str(), "case": c.here.case.as_str()})).unwrap();
    let seen = ask(&mut a, Fake::new(), "Review the liability cap");
    assert_eq!(recalled_json(&seen.lock().unwrap().turns[0]).len(), 3);
    a.run("assist.memory.case.unbind", json!({})).unwrap();
    let seen = ask(&mut a, Fake::new(), "Review the liability cap");
    assert!(seen.lock().unwrap().turns[0].knowledge.is_empty(), "unbound, nothing is recalled");
    let details = crate::assist::details_for(&mut a);
    assert_eq!(details.turns[0].context.as_ref().unwrap().recall_withheld.as_deref(), Some("no case is bound to this document"));
    let gone = a.run("assist.memory.case.bind", json!({"wall": c.here.wall.as_str(), "case": "0123456789abcdef"})).unwrap_err();
    assert!(gone.contains("deleted"), "{gone}");

    a.run("assist.memory.case.bind", json!({"wall": c.here.wall.as_str(), "case": c.here.case.as_str()})).unwrap();
    let fake = Fake::new().then(vec![Step::say("Working on it."), Step::Hold]);
    let seen = fake.seen();
    a.assist_with(Box::new(fake));
    a.execute("assist.ask", json!({"prompt": "Review the liability cap"})).unwrap();
    frames_until(&mut a, "the backend never got the turn", |_| !seen.lock().unwrap().turns.is_empty());
    a.run("assist.memory.case.bind", json!({"wall": c.neighbor.wall.as_str(), "case": c.neighbor.case.as_str()})).unwrap();
    assert!(a.conversation().unwrap().has_ended(), "the conversation bound to the first case ended");
    frames_until(&mut a, "the backend was never told to stop", |_| seen.lock().unwrap().ended);
}

/// What the person set through the commands is there in a window started later, bound again.
#[test]
fn a_case_made_through_the_commands_survives_a_restart() {
    let folder = Folder::new();
    let mut a = window(&folder);
    a.run("assist.memory.case.new", json!({"title": "Acme MSA", "client": "Acme"})).unwrap();
    let at = crate::assist::cases::bound_for(&a).unwrap();
    a.run(
        "assist.memory.case.facts",
        json!({"facts": {"kind": "saas", "side": "customer", "paper": "theirs", "law": "JP", "stage": "negotiating", "parties": ["Acme KK", " "]}}),
    )
    .unwrap();
    a.run("assist.memory.case.balance", json!({"stance": "weaker", "factors": [{"kind": "soleSource", "favors": "them", "why": "one vendor"}, {"kind": "deadline", "favors": "them"}]}))
        .unwrap();
    a.run("assist.memory.case.plan", json!({"goal": "Cap liability", "next": ["Send redline", "Call Friday"]})).unwrap();
    a.run("assist.memory.case.note", json!({"note": {"kind": "pattern", "rule": "They broaden Affiliate in the schedules."}, "how": "seen twice"}))
        .unwrap();
    a.run("assist.memory.case.note", json!({"note": {"kind": "position", "topic": "liability", "moved": "them", "terms": "12 months of fees", "traded_for": "quarterly terms", "landed": true}})).unwrap();
    a.run(
        "assist.memory.case.noteEdit",
        json!({"id": "n1", "note": {"kind": "pitfall", "trap": "Affiliate widens in the schedules.", "guard": "Fix it in clause 1."}}),
    )
    .unwrap();
    drop(a);

    let mut a = window(&folder);
    assert_eq!(crate::assist::cases::bound_for(&a), None, "a binding lives for the document's life in its window");
    a.run("assist.memory.case.bind", json!({"wall": at.wall.as_str(), "case": at.case.as_str()})).unwrap();
    let (_, case) = crate::assist::cases::open_case(&mut a, false).unwrap();
    assert_eq!(case.title.for_person(), "Acme MSA");
    assert_eq!(case.facts.kind, Some(wordcraft_assist::cases::ContractKind::Saas));
    assert_eq!(case.facts.law.map(String::from).as_deref(), Some("JP"));
    assert_eq!(case.facts.parties.iter().map(|p| p.for_person()).collect::<Vec<_>>(), ["Acme KK"]);
    let b = case.balance.last().unwrap();
    assert_eq!((b.stance, b.factors.len()), (Stance::Weaker, 2));
    assert_eq!(b.factors[0].why.as_ref().unwrap().for_person(), "one vendor");
    assert!(matches!(b.standing, wordcraft_assist::cases::Standing::Confirmed { .. }), "the person's assessment is confirmed");
    let plan = case.plan.as_ref().unwrap();
    assert_eq!(plan.goal.for_person(), "Cap liability");
    assert_eq!(plan.next.len(), 2);
    assert!(matches!(plan.standing, wordcraft_assist::cases::Standing::Confirmed { .. }), "the person's plan is confirmed");
    assert_eq!(case.notes.len(), 2);
    assert!(
        matches!(&case.notes[0].body, Body::Pitfall { trap, .. } if trap.for_person() == "Affiliate widens in the schedules."),
        "{:?}",
        case.notes[0].body
    );
    assert!(
        matches!(&case.notes[1].body, Body::Position { landed: true, traded_for: Some(t), .. } if t.for_person() == "quarterly terms"),
        "{:?}",
        case.notes[1].body
    );
    let listing = Shelf::at(&folder.0).list();
    assert_eq!(listing.walls[0].name.for_person(), "Acme");
}

/// Delete removes the case's folder with its notes, and a later turn of that document recalls
/// nothing.
#[test]
fn delete_removes_the_case_and_its_notes_and_a_later_turn_recalls_nothing() {
    let folder = Folder::new();
    let mut a = window(&folder);
    a.run("assist.memory.case.new", json!({"title": "Acme MSA", "client": "Acme"})).unwrap();
    a.run("assist.memory.case.note", json!({"note": {"kind": "pattern", "rule": HERE}})).unwrap();
    let at = crate::assist::cases::bound_for(&a).unwrap();
    let case_dir = cases_dir(&folder).join(at.wall.as_str()).join(at.case.as_str());
    assert!(std::fs::read_to_string(case_dir.join("case.json")).unwrap().contains(HERE));
    let seen = ask(&mut a, Fake::new(), "Review the liability cap");
    assert_eq!(recalled_json(&seen.lock().unwrap().turns[0]).len(), 1);
    a.run("assist.memory.case.delete", json!({})).unwrap();
    assert!(!case_dir.exists(), "the folder is gone");
    assert!(a.conversation().unwrap().has_ended(), "the conversation that read the notes ended");
    let listing = Shelf::at(&folder.0).list();
    assert!(listing.walls[0].cases.is_empty(), "{listing:?}");
    for entry in walkdir(&folder.0) {
        assert!(!std::fs::read_to_string(&entry).unwrap_or_default().contains(HERE), "{} still holds the note", entry.display());
    }
    let seen = ask(&mut a, Fake::new(), "Review the liability cap");
    assert!(seen.lock().unwrap().turns[0].knowledge.is_empty());
    let details = crate::assist::details_for(&mut a);
    assert_eq!(details.turns[0].context.as_ref().unwrap().recall_withheld.as_deref(), Some("no case is bound to this document"));
    assert_eq!(
        a.run("assist.memory.case.note", json!({"note": {"kind": "pattern", "rule": "x"}})).unwrap_err(),
        "no case is bound to this document: bind one in the Assistant pane's Details › Case"
    );
}

fn walkdir(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.path().is_dir() { out.extend(walkdir(&entry.path())) } else { out.push(entry.path()) }
    }
    out
}

/// Junk the commands pass through as objects is refused where it is parsed, and never panics.
#[test]
fn junk_in_a_case_write_is_refused_not_crashed() {
    let folder = Folder::new();
    let mut a = window(&folder);
    a.run("assist.memory.case.new", json!({"title": "Acme MSA"})).unwrap();
    for (id, bad) in [
        ("assist.memory.case.facts", json!({"facts": {"kind": "weapon"}})),
        ("assist.memory.case.facts", json!({"facts": {"unknown": 1}})),
        ("assist.memory.case.facts", json!({"facts": {"law": "../../etc"}})),
        ("assist.memory.case.facts", json!({"facts": {"parties": vec!["x"; 17]}})),
        ("assist.memory.case.balance", json!({"stance": "even", "factors": [{"kind": "nope", "favors": "us"}]})),
        ("assist.memory.case.balance", json!({"stance": "even", "factors": [{"kind": "pen", "favors": "us", "why": "x".repeat(121)}]})),
        ("assist.memory.case.plan", json!({"goal": "x".repeat(400), "next": vec!["x".repeat(401)]})),
        ("assist.memory.case.note", json!({"note": {"kind": "pattern", "rule": "x", "extra": 1}})),
        ("assist.memory.case.note", json!({"note": {"kind": "leverage", "what": "x", "favors": "us", "until": "yesterday"}})),
        ("assist.memory.case.note", json!({"note": {"kind": "position", "topic": "price", "moved": "us", "terms": "x", "landed": false}})),
        ("assist.memory.case.noteEdit", json!({"id": "n999", "note": {"kind": "pattern", "rule": "x"}})),
        ("assist.memory.case.noteConfirm", json!({"id": "n999"})),
    ] {
        assert!(a.run(id, bad.clone()).is_err(), "{id} {bad}");
    }
    let (_, case) = crate::assist::cases::open_case(&mut a, true).unwrap();
    assert!(case.notes.is_empty() && case.balance.is_empty() && case.plan.is_none() && case.facts == Default::default(), "nothing of the junk stuck");
}

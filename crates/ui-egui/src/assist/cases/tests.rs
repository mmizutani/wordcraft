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

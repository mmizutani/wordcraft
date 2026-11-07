//! Presence in a room ([`crate::room::Presence`]): each copy tells the room where its caret and
//! selection are, and every other copy sees them there, moved with the text.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use wordcraft_doc::change::Anchor;
use wordcraft_doc::{BlockId, Document, Path, Pos, StoryRef};

use super::room::{join, run, settle};
use crate::room::{COLORS, Hub, MAX_PRESENCES, ParticipantId, Presence, Replica, Role, Room, hosted, participant_json};
use crate::{Selection, Session};

/// Where `r`'s own selection is, as anchors.
fn mine(r: &Replica) -> Presence {
    match r.presence() {
        Some(p) => p,
        None => panic!("{} has no selection in the room's document", r.id()),
    }
}

/// Where `r` sees participant `id`'s selection, as anchors in its document.
fn seen(r: &mut Replica, id: ParticipantId) -> Option<Presence> {
    r.others().into_iter().find(|o| o.who.id == id).and_then(|o| o.anchors)
}

/// The selection `r` sees for participant `id`, as places.
fn seen_at(r: &mut Replica, id: ParticipantId) -> Option<Selection> {
    r.others().into_iter().find(|o| o.who.id == id).and_then(|o| o.at)
}

fn caret(p: Pos) -> Selection {
    Selection { anchor: p.clone(), focus: p }
}

fn call(copy: &mut Replica, method: &str, params: Value) -> Value {
    match hosted::call(copy, method, &params) {
        Ok(v) => v,
        Err(e) => panic!("{method} {params}: {e}"),
    }
}

fn execute(copy: &mut Replica, command: &str, params: Value) -> Value {
    call(copy, "engine.execute", json!({"command": command, "params": params}))
}

/// What `room.participants` in `copy` says of participant `id`.
fn listed(copy: &mut Replica, id: ParticipantId) -> Value {
    let all = call(copy, "room.participants", json!({}));
    let found = all["participants"].as_array().and_then(|ps| ps.iter().find(|p| p["id"] == json!(id)).cloned());
    match found {
        Some(p) => p,
        None => panic!("{id} isn't in {all}"),
    }
}

/// An invited participant's copy: they join with a key, as agents do.
fn agent(room: &Arc<Room>, name: &str, role: Role) -> Replica {
    let key = match room.invite(name, role) {
        Ok(k) => k,
        Err(e) => panic!("{name}: {e}"),
    };
    match room.join_with(&key) {
        Ok(r) => r,
        Err(e) => panic!("{name}: {e}"),
    }
}

#[test]
fn a_copy_s_selection_reaches_the_others_without_its_text() {
    let room = Room::new(Document::from_text("one two three\nfour"));
    let mut ada = agent(&room, "Ada", Role::Editor);
    let mut bob = agent(&room, "Bob", Role::Editor);
    execute(&mut ada, "select.text", json!({"text": "two"}));
    assert_eq!(seen_at(&mut bob, ada.id()), Some(Selection { anchor: Pos::body(0, 4), focus: Pos::body(0, 7) }));
    let block = room.document().1.body.first().map(|b| b.id()).unwrap();
    let p = listed(&mut bob, ada.id());
    assert_eq!(p["selection"], json!({"anchor": {"block": block, "off": 4}, "focus": {"block": block, "off": 7}, "story": "body", "index": 0}));
    assert_eq!((p["name"].as_str(), p["role"].as_str()), (Some("Ada"), Some("editor")));
    assert!(p["color"].as_u64().is_some_and(|c| c < u64::from(COLORS)), "{p}");
    // No text goes with it.
    let all = call(&mut bob, "room.participants", json!({}));
    assert!(!all.to_string().contains("two"), "{all}");
    // Ada's own entry is where her copy has her.
    let id = ada.id();
    assert_eq!(listed(&mut ada, id)["selection"], p["selection"]);
}

#[test]
fn others_see_a_place_where_its_own_copy_has_it_after_their_edits() {
    // Ada's caret stays put while Bob edits around it: where Bob sees it is where Ada's copy
    // has it once her copy has Bob's edit, after each kind of edit.
    let room = Room::new(Document::from_text("alpha beta gamma\nsecond line here\nthird"));
    let mut ada = join(&room, "Ada");
    let mut bob = join(&room, "Bob");
    run(&mut ada, "caret.set", json!({"pos": Pos::body(1, 7)}));
    ada.present();
    let steps: Vec<(&str, Vec<(&str, Value)>)> = vec![
        ("typing before it", vec![("caret.set", json!({"pos": Pos::body(1, 0)})), ("text.insert", json!({"text": "XY"}))]),
        ("typing right at it", vec![("caret.set", json!({"pos": Pos::body(1, 9)})), ("text.insert", json!({"text": "Q"}))]),
        ("Enter before it", vec![("caret.set", json!({"pos": Pos::body(1, 3)})), ("text.newParagraph", json!({}))]),
        ("Backspace joining its paragraph to the one before", vec![("caret.set", json!({"pos": Pos::body(2, 0)})), ("text.backspace", json!({}))]),
        ("a deletion around it", vec![("select.range", json!({"anchor": Pos::body(1, 5), "focus": Pos::body(1, 12)})), ("text.delete", json!({}))]),
        ("Enter right at it", vec![("caret.set", json!({"pos": Pos::body(1, 5)})), ("text.newParagraph", json!({}))]),
    ];
    for (what, commands) in steps {
        for (id, params) in commands {
            run(&mut bob, id, params);
        }
        bob.send();
        ada.receive();
        assert_eq!(seen(&mut bob, ada.id()), Some(mine(&ada)), "after {what}");
    }
    // Its paragraph deleted: it goes to where the paragraph was, within the text.
    let at = mine(&ada);
    let path = ada.session().doc.pos_of(at.focus, None).unwrap().path;
    let row = path.0.first().copied().unwrap();
    run(&mut bob, "select.range", json!({"anchor": Pos::body(row as usize - 1, 2), "focus": Pos::body(row as usize + 1, 0)}));
    run(&mut bob, "text.delete", json!({}));
    bob.send();
    ada.receive();
    assert_eq!(seen(&mut bob, ada.id()), Some(mine(&ada)), "after its paragraph was deleted");
    let p = listed_from(&mut bob, ada.id());
    let off = p["selection"]["focus"]["off"].as_u64().unwrap();
    assert!(off <= 64, "an offset past the text: {p}");
}

/// [`listed`] for a copy that isn't hosted: what `room.participants` would say.
fn listed_from(copy: &mut Replica, id: ParticipantId) -> Value {
    let others = copy.others();
    let o = others.iter().find(|o| o.who.id == id).unwrap();
    participant_json(&o.who, o.color, o.anchors.as_ref().zip(o.at.as_ref()))
}

#[test]
fn a_place_that_ends_up_past_a_paragraph_s_end_is_at_its_end() {
    // Ada is in the last paragraph; Bob deletes it from the end of the one before, so her
    // caret is left at the end of that one, and Bob then types there: as in Ada's copy, she
    // stays before what Bob typed.
    let room = Room::new(Document::from_text("first\nlast one"));
    let mut ada = join(&room, "Ada");
    let mut bob = join(&room, "Bob");
    run(&mut ada, "caret.set", json!({"pos": Pos::body(1, 4)}));
    ada.present();
    run(&mut bob, "select.range", json!({"anchor": Pos::body(0, 5), "focus": Pos::body(1, 8)}));
    run(&mut bob, "text.delete", json!({}));
    bob.send();
    ada.receive();
    assert_eq!(seen(&mut bob, ada.id()), Some(mine(&ada)));
    run(&mut bob, "text.insert", json!({"text": "!!"}));
    bob.send();
    ada.receive();
    assert_eq!(seen(&mut bob, ada.id()), Some(mine(&ada)));
    assert_eq!(mine(&ada).focus.off, 5);
}

#[test]
fn a_place_left_at_a_paragraph_s_end_stays_before_what_is_typed_there_in_the_same_batch() {
    // As above, but Bob's deletion and his typing reach the hub together, and nobody reads
    // where Ada is in between: the hub moves her through both at once.
    let room = Room::new(Document::from_text("first\nlast one"));
    let mut ada = join(&room, "Ada");
    let mut bob = join(&room, "Bob");
    run(&mut ada, "caret.set", json!({"pos": Pos::body(1, 4)}));
    ada.present();
    run(&mut bob, "select.range", json!({"anchor": Pos::body(0, 5), "focus": Pos::body(1, 8)}));
    run(&mut bob, "text.delete", json!({}));
    run(&mut bob, "text.insert", json!({"text": "!!"}));
    run(&mut bob, "text.backspace", json!({}));
    run(&mut bob, "text.insert", json!({"text": "?"}));
    bob.send();
    ada.receive();
    assert_eq!(mine(&ada).focus.off, 5);
    assert_eq!(seen(&mut bob, ada.id()), Some(mine(&ada)));
}

#[test]
fn a_new_selection_with_the_numbers_of_an_old_place_reaches_the_others() {
    // Ada selects "two"; Bob types before it, so the room has her further on; she then selects
    // "one", which has the offsets "two" had: the others see her on "one".
    let room = Room::new(Document::from_text("one two three"));
    let mut ada = agent(&room, "Ada", Role::Editor);
    let mut bob = join(&room, "Bob");
    execute(&mut ada, "select.text", json!({"text": "two"}));
    run(&mut bob, "caret.set", json!({"pos": Pos::body(0, 0)}));
    run(&mut bob, "text.insert", json!({"text": "abc "}));
    bob.send();
    execute(&mut ada, "select.text", json!({"text": "one"}));
    assert_eq!(mine(&ada).anchor.off, 4);
    assert_eq!(seen(&mut bob, ada.id()), Some(mine(&ada)));
}

#[test]
fn others_edits_during_a_request_that_only_reads_show_nobody() {
    // An agent that never moved, and the selection put in a copy outside a request (as the
    // assistant takes over the person's at the start of a turn): others' edits move them in the
    // copy, and a request that only reads still shows neither.
    let room = Room::new(Document::from_text("one two three"));
    let mut ada = agent(&room, "Ada", Role::Editor);
    let mut cy = agent(&room, "Cy", Role::Editor);
    let mut bob = join(&room, "Bob");
    execute(&mut ada, "select.text", json!({"text": "two"}));
    run(&mut ada, "select.range", json!({"anchor": Pos::body(0, 8), "focus": Pos::body(0, 13)}));
    run(&mut bob, "caret.set", json!({"pos": Pos::body(0, 0)}));
    run(&mut bob, "text.insert", json!({"text": "X "}));
    bob.send();
    call(&mut ada, "document.inspect", json!({}));
    call(&mut cy, "room.participants", json!({}));
    assert_eq!(seen_at(&mut bob, ada.id()), Some(Selection { anchor: Pos::body(0, 6), focus: Pos::body(0, 9) }), "still on \"two\"");
    assert_eq!(seen(&mut bob, cy.id()), None);
}

#[test]
fn an_edit_that_leaves_the_caret_shows_where_the_agent_is() {
    let room = Room::new(Document::from_text("one two"));
    let mut ada = agent(&room, "Ada", Role::Editor);
    let mut bob = join(&room, "Bob");
    // Centring the paragraph her caret is in, where a new copy's caret is: an edit, which shows
    // her there though her caret didn't move.
    execute(&mut ada, "para.alignCenter", json!({}));
    assert_eq!(seen_at(&mut bob, ada.id()), Some(caret(Pos::body(0, 0))));
}

#[test]
fn a_reader_s_own_typing_at_another_s_caret_goes_after_them() {
    // Bob types where Ada's caret is and hasn't sent it: in his copy she stays before his text,
    // as she will once the hub has it.
    let room = Room::new(Document::from_text("one two"));
    let mut ada = join(&room, "Ada");
    let mut bob = join(&room, "Bob");
    run(&mut ada, "caret.set", json!({"pos": Pos::body(0, 4)}));
    ada.present();
    run(&mut bob, "caret.set", json!({"pos": Pos::body(0, 4)}));
    run(&mut bob, "text.insert", json!({"text": "big "}));
    assert_eq!(seen_at(&mut bob, ada.id()), Some(caret(Pos::body(0, 4))));
    bob.send();
    ada.receive();
    assert_eq!(seen_at(&mut bob, ada.id()), Some(caret(Pos::body(0, 4))));
    assert_eq!(mine(&ada).focus.off, 4);
}

#[test]
fn a_place_in_a_table_cell_or_a_text_box_says_where_it_is() {
    let mut s = Session::new(Document::from_text("before\nr1\tx\nr2\ty"));
    s.run("select.range", &json!({"anchor": Pos::body(1, 0), "focus": Pos::body(2, 4)})).unwrap();
    s.run("table.fromText", &json!({})).unwrap();
    s.run("caret.set", &json!({"pos": Pos::body(0, 0)})).unwrap();
    let story = s.run("insert.textBox", &json!({"text": "boxed"})).unwrap()["story"].as_u64().unwrap() as u32;
    let room = Room::new(s.doc.clone());
    let mut ada = agent(&room, "Ada", Role::Editor);
    let mut bob = agent(&room, "Bob", Role::Editor);
    execute(&mut ada, "select.text", json!({"text": "y"}));
    let p = listed(&mut bob, ada.id());
    assert_eq!((p["selection"]["story"].as_str(), p["selection"]["index"].as_u64()), (Some("body"), Some(1)), "{p}");
    let cell = ada.session().doc.anchor_at(&ada.session().sel.focus).unwrap();
    assert_eq!(p["selection"]["focus"], json!(cell));
    let in_box = Pos { story: StoryRef::Part(story), path: Path::top(0), off: 2 };
    execute(&mut ada, "select.range", json!({"anchor": in_box, "focus": in_box}));
    let p = listed(&mut bob, ada.id());
    assert_eq!((&p["selection"]["story"], p["selection"]["index"].as_u64()), (&json!({"part": story}), Some(0)), "{p}");
    // The story is as a place gives it: the caret, put back together, is where she is.
    let back: Pos = serde_json::from_value(json!({"story": p["selection"]["story"], "path": [0], "off": 2})).unwrap();
    assert_eq!(back, in_box);
    assert_eq!(seen_at(&mut bob, ada.id()), Some(caret(in_box)));
}

#[test]
fn the_room_gives_each_place_as_of_the_version_asked_for() {
    let room = Room::new(Document::from_text("one two"));
    let mut ada = join(&room, "Ada");
    let mut bob = join(&room, "Bob");
    run(&mut ada, "caret.set", json!({"pos": Pos::body(0, 4)}));
    ada.present();
    run(&mut bob, "caret.set", json!({"pos": Pos::body(0, 0)}));
    run(&mut bob, "text.insert", json!({"text": "zero "}));
    bob.send();
    let id = ada.id();
    let at = |v: u64| room.presences_at(v).1.into_iter().find(|s| s.who.id == id).unwrap();
    // As of version 0, she is where she said; as of 1, past what Bob put in before her.
    assert_eq!(at(0).at.map(|p| p.focus.off), Some(4));
    assert_eq!(at(1).at.map(|p| p.focus.off), Some(9));
    // Told as of version 1, her place isn't known as of 0: the reader gets version 1 first.
    ada.receive();
    run(&mut ada, "caret.set", json!({"pos": Pos::body(0, 1)}));
    ada.present();
    let old = at(0);
    assert_eq!((old.at, old.later), (None, true));
    assert_eq!(at(1).at.map(|p| p.focus.off), Some(1));
    // Asked for a version the document never had: nobody's is known.
    assert!(room.presences_at(u64::MAX).1.iter().all(|s| s.at.is_none() && !s.later));
}

#[test]
fn a_reader_s_own_edits_move_the_places_it_sees() {
    let room = Room::new(Document::from_text("one two"));
    let mut ada = join(&room, "Ada");
    let mut bob = join(&room, "Bob");
    run(&mut ada, "caret.set", json!({"pos": Pos::body(0, 4)}));
    ada.present();
    // Bob types before her without sending: in his copy, she is past his text.
    run(&mut bob, "caret.set", json!({"pos": Pos::body(0, 0)}));
    run(&mut bob, "text.insert", json!({"text": "AB"}));
    assert_eq!(seen_at(&mut bob, ada.id()), Some(caret(Pos::body(0, 6))));
    bob.send();
    assert_eq!(seen_at(&mut bob, ada.id()), Some(caret(Pos::body(0, 6))));
}

#[test]
fn a_participant_s_own_typing_moves_their_place_past_it_until_they_say_where_they_are() {
    let room = Room::new(Document::from_text("one two"));
    let mut ada = join(&room, "Ada");
    let mut bob = join(&room, "Bob");
    run(&mut ada, "caret.set", json!({"pos": Pos::body(0, 4)}));
    ada.present();
    run(&mut ada, "text.insert", json!({"text": "XYZ"}));
    // Not sent: the room doesn't know her edit, so she can't say where she is in it yet.
    ada.present();
    assert_eq!(seen_at(&mut bob, ada.id()), Some(caret(Pos::body(0, 4))));
    ada.send();
    assert_eq!(seen_at(&mut bob, ada.id()), Some(caret(Pos::body(0, 7))));
    ada.present();
    assert_eq!(seen_at(&mut bob, ada.id()), Some(caret(Pos::body(0, 7))));
}

#[test]
fn a_copy_with_edits_the_room_has_not_given_back_tells_its_place_once_it_has_them() {
    let room = Room::new(Document::from_text("one two"));
    let mut ada = agent(&room, "Ada", Role::Editor);
    let mut bob = join(&room, "Bob");
    execute(&mut ada, "select.text", json!({"text": "one"}));
    // An edit of hers the room hasn't had yet (made outside a request): she can't say where she
    // is in it until the room has it.
    run(&mut ada, "caret.set", json!({"pos": Pos::body(0, 7)}));
    run(&mut ada, "text.insert", json!({"text": "!"}));
    ada.present();
    assert_eq!(seen_at(&mut bob, ada.id()), Some(Selection { anchor: Pos::body(0, 0), focus: Pos::body(0, 3) }));
    // Her next request sends it first, and then she says where she is, though that request
    // only reads and her caret didn't move in it.
    call(&mut ada, "document.inspect", json!({}));
    assert_eq!(seen_at(&mut bob, ada.id()), Some(caret(Pos::body(0, 8))));
}

#[test]
fn telling_the_place_the_room_moved_a_participant_to_already_wakes_no_one() {
    let room = Room::new(Document::from_text("one two"));
    let moves = Arc::new(AtomicUsize::new(0));
    let m = moves.clone();
    room.on_presence(move || {
        m.fetch_add(1, Ordering::SeqCst);
    });
    let mut ada = join(&room, "Ada");
    let mut bob = join(&room, "Bob");
    run(&mut ada, "caret.set", json!({"pos": Pos::body(0, 4)}));
    ada.present();
    assert_eq!(moves.load(Ordering::SeqCst), 1);
    run(&mut bob, "caret.set", json!({"pos": Pos::body(0, 0)}));
    run(&mut bob, "text.insert", json!({"text": "zero "}));
    bob.send();
    // Her caret moved with Bob's text in her copy, as the room moved her place: telling it
    // again changes nothing.
    ada.receive();
    ada.present();
    assert_eq!(moves.load(Ordering::SeqCst), 1);
    assert_eq!(seen(&mut bob, ada.id()), Some(mine(&ada)));
}

#[test]
fn places_of_those_who_left_take_no_room_from_those_still_here() {
    let room = Room::new(Document::from_text("one two three"));
    let mut window = join(&room, "Minoru");
    window.present();
    let mut stays = agent(&room, "Stays", Role::Viewer);
    execute(&mut stays, "caret.set", json!({"pos": Pos::body(0, 2)}));
    for i in 0..MAX_PRESENCES {
        let mut gone = agent(&room, &format!("Gone {i}"), Role::Viewer);
        execute(&mut gone, "caret.set", json!({"pos": Pos::body(0, 4)}));
    }
    let mut late = agent(&room, "Late", Role::Viewer);
    execute(&mut late, "caret.set", json!({"pos": Pos::body(0, 6)}));
    let seen = room.presences_at(room.version()).1;
    for who in [window.id(), stays.id(), late.id()] {
        assert!(seen.iter().any(|s| s.who.id == who && s.at.is_some()), "{who}'s place went");
    }
}

#[test]
fn requests_that_only_read_leave_where_the_others_see_an_agent() {
    let room = Room::new(Document::from_text("one two three"));
    let mut ada = agent(&room, "Ada", Role::Editor);
    let mut bob = join(&room, "Bob");
    // An agent that only reads isn't seen at the start of the document.
    call(&mut ada, "document.inspect", json!({"text": true}));
    call(&mut ada, "room.participants", json!({}));
    assert_eq!(seen(&mut bob, ada.id()), None);
    let id = ada.id();
    assert_eq!(listed(&mut ada, id)["selection"]["focus"]["off"], json!(0), "her own copy knows where she is");
    execute(&mut ada, "select.text", json!({"text": "two"}));
    assert_eq!(seen_at(&mut bob, ada.id()), Some(Selection { anchor: Pos::body(0, 4), focus: Pos::body(0, 7) }));
    // A selection put in her copy outside a request (the assistant takes over the person's at
    // the start of a turn), then a request that only reads: the others still see her where her
    // own commands left her.
    run(&mut ada, "select.range", json!({"anchor": Pos::body(0, 8), "focus": Pos::body(0, 13)}));
    call(&mut ada, "document.inspect", json!({}));
    assert_eq!(seen_at(&mut bob, ada.id()), Some(Selection { anchor: Pos::body(0, 4), focus: Pos::body(0, 7) }));
    // Typing moves her, and the others see it.
    execute(&mut ada, "text.insert", json!({"text": "3"}));
    assert_eq!(seen_at(&mut bob, ada.id()), Some(caret(Pos::body(0, 9))));
}

#[test]
fn leaving_and_revoking_take_a_place_away_and_say_so() {
    let room = Room::new(Document::from_text("one two"));
    let changes = Arc::new(AtomicUsize::new(0));
    let counted = changes.clone();
    room.on_change(move || {
        counted.fetch_add(1, Ordering::SeqCst);
    });
    let stamp = room.presence_stamp();
    let mut bob = join(&room, "Bob");
    assert!(room.presence_stamp() > stamp && changes.load(Ordering::SeqCst) == 1, "a join is seen");
    let mut ada = join(&room, "Ada");
    run(&mut ada, "caret.set", json!({"pos": Pos::body(0, 2)}));
    ada.present();
    assert!(seen(&mut bob, ada.id()).is_some());
    let (stamp, called) = (room.presence_stamp(), changes.load(Ordering::SeqCst));
    let id = ada.id();
    drop(ada);
    assert!(room.presence_stamp() > stamp && changes.load(Ordering::SeqCst) > called, "a leave is seen");
    assert!(bob.others().iter().all(|o| o.who.id != id));
    assert!(room.presences_at(room.version()).1.iter().all(|s| s.who.id != id));

    let key = room.invite("Cy", Role::Editor).unwrap();
    let mut cy = room.join_with(&key).unwrap();
    execute(&mut cy, "select.text", json!({"text": "two"}));
    assert!(seen(&mut bob, cy.id()).is_some());
    let (stamp, called) = (room.presence_stamp(), changes.load(Ordering::SeqCst));
    assert!(room.revoke(&key));
    assert!(room.presence_stamp() > stamp && changes.load(Ordering::SeqCst) > called, "a revoke is seen");
    assert!(bob.others().iter().all(|o| o.who.id != cy.id()));
}

#[test]
fn a_moved_place_wakes_who_waits_for_presence_and_not_for_changes() {
    let room = Room::new(Document::from_text("one two"));
    let (changes, moves) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (c, m) = (changes.clone(), moves.clone());
    room.on_change(move || {
        c.fetch_add(1, Ordering::SeqCst);
    });
    room.on_presence(move || {
        m.fetch_add(1, Ordering::SeqCst);
    });
    let mut ada = agent(&room, "Ada", Role::Editor);
    let mut bob = join(&room, "Bob");
    let called = changes.load(Ordering::SeqCst);
    let stamp = room.presence_stamp();
    execute(&mut ada, "select.text", json!({"text": "two"}));
    assert_eq!((changes.load(Ordering::SeqCst), moves.load(Ordering::SeqCst)), (called, 1));
    assert!(room.presence_stamp() > stamp, "a moved place changes the stamp");
    // An accepted change moves everyone's: the stamp changes, and who waits for changes wakes.
    let stamp = room.presence_stamp();
    run(&mut bob, "text.insert", json!({"text": "x"}));
    bob.sync();
    assert!(room.presence_stamp() > stamp && changes.load(Ordering::SeqCst) == called + 1);
    let called = called + 1;
    // Telling the same place again changes nothing: no stamp, no wake.
    let stamp = room.presence_stamp();
    for _ in 0..20 {
        bob.sync();
        bob.present();
        ada.present();
        call(&mut ada, "document.inspect", json!({}));
        let _ = bob.others();
    }
    bob.present();
    let stamp = (stamp, room.presence_stamp());
    for _ in 0..20 {
        bob.sync();
        bob.present();
        ada.present();
        call(&mut ada, "document.inspect", json!({}));
        let _ = bob.others();
    }
    assert_eq!(room.presence_stamp(), stamp.1, "idle copies keep the stamp");
    assert_eq!(moves.load(Ordering::SeqCst), 2, "Bob told his place once");
    assert_eq!(changes.load(Ordering::SeqCst), called);
}

#[test]
fn a_copy_alone_costs_no_stamp_while_idle() {
    let mut s = Session::new(Document::from_text("one two"));
    let (room, mut own) = Room::host(&mut s, "Minoru").unwrap();
    let _ = own.lend(&mut s, |c| {
        c.sync();
        c.present();
    });
    let stamp = room.presence_stamp();
    for _ in 0..50 {
        let _ = own.lend(&mut s, |c| {
            c.sync();
            c.present();
        });
    }
    assert_eq!(room.presence_stamp(), stamp);
}

#[test]
fn a_change_the_hub_sends_back_leaves_every_place_as_it_was() {
    let mut hub = Hub::new(Document::from_text("one two"));
    let ada = hub.admit("Ada", Role::Editor).unwrap();
    let bob = hub.admit("Bob", Role::Viewer).unwrap();
    let cy = hub.admit("Cy", Role::Commenter).unwrap();
    let doc = hub.document().clone();
    let at = doc.anchor_at(&Pos::body(0, 4)).unwrap();
    assert_eq!(hub.set_presence(ada.id, 0, Some(Presence { anchor: at, focus: at })), Some(true));
    let before = hub.presences_at(0);
    let mut edited = doc.clone();
    edited.insert_text(&Pos::body(0, 0), "x", &Default::default()).unwrap();
    let change = wordcraft_doc::ChangeSet::between(&doc, &edited);
    // A viewer changes nothing, a commenter's edit of the text is sent back once the hub made
    // it to check it, a change made on a version the document never had doesn't apply, and
    // the second of two changes that doesn't apply after the first sends both back.
    assert!(hub.submit(0, &change, bob.id).is_err());
    assert!(hub.submit(0, &change, cy.id).is_err());
    assert!(hub.submit(5, &change, ada.id).is_err());
    assert!(hub.submit_all(0, &[change.clone(), change.clone()], ada.id).is_err());
    assert_eq!(hub.presences_at(0), before);
    assert_eq!(hub.version(), 0);
}

#[test]
fn a_place_told_as_of_a_version_the_hub_no_longer_keeps_is_not_kept() {
    let room = Room::with_hub(Hub::new(Document::from_text("one two")).keeping(2));
    let ada = join(&room, "Ada");
    let mut bob = join(&room, "Bob");
    let at = ada.session().doc.anchor_at(&Pos::body(0, 4)).unwrap();
    for _ in 0..5 {
        run(&mut bob, "text.insert", json!({"text": "x"}));
        bob.sync();
    }
    assert_eq!(room.version(), 5);
    assert!(!room.set_presence(ada.id(), 0, Some(Presence { anchor: at, focus: at })));
    assert_eq!(seen(&mut bob, ada.id()), None);
}

#[test]
fn places_stay_known_while_the_changes_they_were_told_at_go() {
    // The hub keeps two changes; Ada said where she is before Bob's five. Her place moves
    // through each before it goes, so it stays known, past what Bob typed before her.
    let room = Room::with_hub(Hub::new(Document::from_text("one two")).keeping(2));
    let mut ada = join(&room, "Ada");
    let mut bob = join(&room, "Bob");
    run(&mut ada, "caret.set", json!({"pos": Pos::body(0, 4)}));
    ada.present();
    run(&mut bob, "caret.set", json!({"pos": Pos::body(0, 0)}));
    for _ in 0..5 {
        run(&mut bob, "text.insert", json!({"text": "x"}));
        bob.sync();
    }
    assert_eq!(room.version(), 5);
    assert_eq!(seen_at(&mut bob, ada.id()), Some(caret(Pos::body(0, 9))));
}

#[test]
fn a_copy_that_reads_the_document_again_tells_its_place_again() {
    // Ada's caret is between "a" and "b". Bob deletes the "a" and types it again, which leaves
    // her place before it in the hub, which moves her through each change, then types at the
    // end, past the two changes the hub keeps. Ada's copy reads the document again and finds
    // her by what changed between the two versions, the "c"s alone, so she stays after the
    // "a": the others see her where her copy has her, where her next edit goes.
    let room = Room::with_hub(Hub::new(Document::from_text("ab")).keeping(2));
    let mut ada = agent(&room, "Ada", Role::Editor);
    let mut bob = join(&room, "Bob");
    execute(&mut ada, "caret.set", json!({"pos": Pos::body(0, 1)}));
    let edits = [
        ("select.range", json!({"anchor": Pos::body(0, 0), "focus": Pos::body(0, 1)})),
        ("text.delete", json!({})),
        ("text.insert", json!({"text": "a"})),
        ("caret.docEnd", json!({})),
        ("text.insert", json!({"text": "c"})),
        ("text.insert", json!({"text": "c"})),
        ("text.insert", json!({"text": "c"})),
    ];
    for (id, params) in edits {
        run(&mut bob, id, params);
        bob.sync();
    }
    assert_eq!(room.version(), 5);
    call(&mut ada, "document.inspect", json!({}));
    assert!(ada.stats().read_again > 0);
    let hers = ada.session().sel.clone();
    assert_eq!(seen_at(&mut bob, ada.id()), Some(hers));
}

#[test]
fn the_room_keeps_a_bounded_number_of_places_and_always_its_own_process_s() {
    let room = Room::new(Document::from_text("one two three four five six seven eight nine ten"));
    let mut window = join(&room, "Minoru");
    run(&mut window, "caret.set", json!({"pos": Pos::body(0, 3)}));
    window.present();
    let mut agents: Vec<Replica> = (0..100).map(|i| agent(&room, &format!("Agent {i}"), Role::Viewer)).collect();
    for (i, a) in agents.iter_mut().enumerate() {
        // Each moves its caret from where a copy starts.
        execute(a, "caret.set", json!({"pos": Pos::body(0, i % 40 + 1)}));
    }
    let seen = room.presences_at(room.version()).1;
    let known: Vec<&str> = seen.iter().filter(|s| s.at.is_some()).map(|s| s.who.name.as_str()).collect();
    assert_eq!(known.len(), MAX_PRESENCES, "{known:?}");
    assert!(seen.iter().any(|s| s.who.id == window.id() && s.at.is_some()), "the window's place goes");
    // The places told last are those kept: the window's and the last 63 agents'.
    let last: Vec<String> = (100 - (MAX_PRESENCES - 1)..100).map(|i| format!("Agent {i}")).collect();
    assert_eq!(known.iter().skip(1).copied().collect::<Vec<_>>(), last.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn each_name_keeps_its_colour_and_names_in_the_room_get_different_ones() {
    let room = Room::new(Document::from_text("one"));
    let key = room.invite("Ada", Role::Editor).unwrap();
    let ada = room.join_with(&key).unwrap();
    let colour = ada.color();
    let again = room.join_with(&key).unwrap();
    assert_eq!(again.color(), colour, "another copy of hers");
    drop(ada);
    drop(again);
    let back = room.join_with(&key).unwrap();
    assert_eq!(back.color(), colour, "back after she left");
    let others: Vec<Replica> = (1..COLORS).map(|i| join(&room, &format!("P{i}"))).collect();
    let mut colours: Vec<u8> = others.iter().map(Replica::color).chain([colour]).collect();
    colours.sort_unstable();
    colours.dedup();
    assert_eq!(colours.len(), usize::from(COLORS), "eight names, eight colours");
    // A ninth shares the first of those fewest have: with one each, the first colour.
    let ninth = join(&room, "Ninth");
    assert_eq!(ninth.color(), 0);
    // One leaving frees theirs for the next newcomer.
    let mut others = others;
    let p3 = others.remove(2);
    let freed = p3.color();
    drop(p3);
    let next = join(&room, "Next");
    assert_eq!(next.color(), freed);
    drop(back);
}

#[test]
fn a_participant_back_after_someone_took_their_colour_gets_another() {
    let room = Room::new(Document::from_text("one"));
    let key = room.invite("Ada", Role::Editor).unwrap();
    let ada = room.join_with(&key).unwrap();
    let bob = join(&room, "Bob");
    assert_eq!((ada.color(), bob.color()), (0, 1));
    drop(ada);
    // Cy joins while Ada is away and gets the first colour no one in the room has: hers.
    let cy = join(&room, "Cy");
    assert_eq!(cy.color(), 0);
    let back = room.join_with(&key).unwrap();
    assert_eq!(back.color(), 2, "two in the room at once with one colour");
    assert_eq!(room.join_with(&key).unwrap().color(), 2, "her copies share hers");
    drop((bob, cy));
}

#[test]
fn junk_places_and_versions_never_panic() {
    let room = Room::new(Document::from_text("one two\nthree"));
    let mut ada = join(&room, "Ada");
    let mut bob = join(&room, "Bob");
    let junk = [
        Anchor { block: BlockId::UNSET, off: 0 },
        Anchor { block: BlockId::UNSET, off: usize::MAX },
        Anchor { block: ada.session().doc.body.first().map(|b| b.id()).unwrap(), off: usize::MAX },
        Anchor { block: ada.session().doc.body.first().map(|b| b.id()).unwrap(), off: 3 },
    ];
    for id in [ParticipantId(0), ParticipantId(u32::MAX), ada.id(), bob.id()] {
        for version in [0, 1, u64::MAX] {
            for a in junk {
                for f in junk {
                    let _ = room.set_presence(id, version, Some(Presence { anchor: a, focus: f }));
                    let _ = room.presences_at(version);
                    let _ = bob.others();
                    let _ = room.participants_now();
                }
            }
            let _ = room.set_presence(id, version, None);
        }
    }
    run(&mut ada, "text.insert", json!({"text": "x"}));
    ada.send();
    let mut copies = [ada, bob];
    settle(&mut copies).unwrap();
    for r in &mut copies {
        for o in r.others() {
            let _ = participant_json(&o.who, o.color, o.anchors.as_ref().zip(o.at.as_ref()));
        }
    }
}

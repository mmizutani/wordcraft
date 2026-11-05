//! Rooms ([`crate::room`]): participants edit copies of their own at once, the hub puts their
//! changes in one order, and every copy ends up holding the same document.

use std::sync::{Arc, Barrier};
use std::time::Duration;

use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::TestRunner;
use serde_json::{Value, json};
use wordcraft_doc::{ChangeSet, Document, Pos, StoryRef};

use super::block_ids::{Step, step};
use super::changes::{difference, edit_step};
use crate::room::{Bounce, Hub, ParticipantId, Replica, Role, Room};
use crate::sample::sample_document;
use crate::{CmdError, Session};

/// Join `room` as an editor called `name`.
pub(super) fn join(room: &Arc<Room>, name: &str) -> Replica {
    match room.join(name, Role::Editor) {
        Ok(r) => r,
        Err(e) => panic!("{name} can't join: {e}"),
    }
}

pub(super) fn run(r: &mut Replica, id: &str, v: Value) -> Value {
    match r.run(id, &v) {
        Ok(out) => out,
        Err(e) => panic!("{id} {v}: {e}"),
    }
}

pub(super) fn text(r: &Replica) -> String {
    r.session().doc.plain_text(StoryRef::Body)
}

/// The bold characters of the first paragraph.
fn bold(r: &Replica) -> String {
    let Some(p) = r.session().doc.body.first().and_then(|b| b.as_para()) else { return String::new() };
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

/// Sync every copy until none has edits the hub hasn't accepted and each has every change.
pub(super) fn settle(copies: &mut [Replica]) -> Result<(), String> {
    for _ in 0..50 {
        for r in copies.iter_mut() {
            r.sync();
        }
        let Some(room) = copies.first().map(|r| r.room().clone()) else { return Ok(()) };
        let version = room.version();
        if copies.iter().all(|r| !r.has_unconfirmed() && r.received() == version) {
            return Ok(());
        }
    }
    Err("the copies don't settle".into())
}

/// No copy lost an edit or had to read the whole document again.
pub(super) fn smooth(copies: &[Replica]) {
    for r in copies {
        assert_eq!((r.stats().lost, r.stats().read_again), (0, 0), "{}", r.id());
    }
}

/// Every copy holds the hub's document, under the same block ids, and is as it should be.
pub(super) fn same_everywhere(copies: &[Replica]) -> Result<(), String> {
    let Some(room) = copies.first().map(|r| r.room().clone()) else { return Ok(()) };
    let (_, hub) = room.document();
    for r in copies {
        r.check().map_err(|e| format!("{}: {e}", r.id()))?;
        if r.session().doc != hub {
            return Err(format!("{} holds other content than the hub: {}", r.id(), difference(&r.session().doc, &hub)));
        }
        if r.session().doc.block_ids() != hub.block_ids() {
            return Err(format!("{} holds the hub's content under other block ids", r.id()));
        }
    }
    Ok(())
}

#[test]
fn a_session_makes_another_copy_s_change_with_its_caret_in_the_same_text() {
    let doc = Document::from_text("Hello world\nSecond");
    let mut s = Session::new(doc.clone());
    let base = s.doc.clone();
    s.run("caret.docStart", &json!({})).unwrap();
    s.run("text.insert", &json!({"text": "Hi "})).unwrap();
    s.run("select.text", &json!({"text": "world"})).unwrap();
    assert!(s.can_undo());
    let version = s.changes().version();
    // Another copy of the same version puts a paragraph first, and types there and at the end.
    let mut other = Session::new(base.clone());
    for (id, v) in [
        ("caret.docStart", json!({})),
        ("text.newParagraph", json!({})),
        ("caret.docStart", json!({})),
        ("text.insert", json!({"text": "Title"})),
        ("caret.docEnd", json!({})),
        ("text.insert", json!({"text": "!"})),
    ] {
        other.run(id, &v).unwrap();
    }
    let theirs = other.changes().since(0).unwrap();
    assert_eq!(theirs.len(), 3);
    // Made on the version before "Hi ": each moved past it (and it past each), they go in.
    let mut mine = ChangeSet::between(&base, &s.doc);
    for (_, cs) in &theirs {
        let moved = cs.rebase(&mine, wordcraft_doc::change::Side::Before).unwrap();
        mine = mine.rebase(cs, wordcraft_doc::change::Side::After).unwrap();
        s.apply_change(&moved).unwrap();
    }
    assert_eq!(s.doc.plain_text(StoryRef::Body), "Title\nHi Hello world\nSecond!");
    let (a, f) = (&s.sel.anchor, &s.sel.focus);
    assert_eq!((a.path.0.clone(), a.off, f.path.0.clone(), f.off), (vec![1], 9, vec![1], 14));
    // The session's followers see them; its undo step stays, to take "Hi " back past them
    // (`tests::room_undo`).
    assert_eq!(s.changes().version(), version + 3);
    assert!(s.can_undo());
    // A change that doesn't apply leaves the session as it was.
    let before = (s.doc.clone(), s.sel.clone(), s.changes().version());
    let stale = ChangeSet::between(&base, &other.doc);
    assert!(s.apply_change(&stale).is_err());
    assert_eq!((s.doc.clone(), s.sel.clone(), s.changes().version()), before);
}

#[test]
fn a_copy_starts_with_the_room_s_document() {
    let room = Room::new(sample_document());
    let r = join(&room, "Ana");
    let (version, doc) = room.document();
    assert_eq!(version, 0);
    assert_eq!(r.session().doc, doc);
    assert_eq!(r.session().doc.block_ids(), doc.block_ids());
    assert_eq!(r.session().author, "Ana");
    assert_eq!(r.id(), ParticipantId(1));
    assert_eq!(join(&room, "Ben").id(), ParticipantId(2));
    r.check().unwrap();
}

#[test]
fn two_copies_typing_in_one_paragraph_both_get_through() {
    let room = Room::new(Document::from_text("Hello world"));
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    run(&mut a, "caret.docEnd", json!({}));
    run(&mut a, "text.insert", json!({"text": " from Ana"}));
    run(&mut b, "caret.docStart", json!({}));
    run(&mut b, "text.insert", json!({"text": "Ben: "}));
    // Each sees its own edit at once.
    assert_eq!(text(&a), "Hello world from Ana");
    assert_eq!(text(&b), "Ben: Hello world");
    // Ana's change goes first; Ben's, made on the version before it, is moved past it.
    a.send();
    b.send();
    assert_eq!(room.version(), 2);
    assert_eq!((a.stats().moved, b.stats().moved), (0, 1));
    a.receive();
    b.receive();
    for r in [&a, &b] {
        assert_eq!(text(r), "Ben: Hello world from Ana");
        assert!(!r.has_unconfirmed());
        assert_eq!((r.stats().bounced, r.stats().clashes, r.stats().made_again), (0, 0, 0));
    }
    let copies = [a, b];
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn text_typed_at_the_same_place_goes_in_the_order_the_hub_took_it() {
    let room = Room::new(Document::from_text("ab"));
    let mut copies = [join(&room, "Ana"), join(&room, "Ben"), join(&room, "Cy")];
    for (r, t) in copies.iter_mut().zip(["1", "2", "3"]) {
        run(r, "caret.docEnd", json!({}));
        run(r, "text.insert", json!({"text": t}));
    }
    // The hub takes Cy's, then Ana's, then Ben's.
    for i in [2, 0, 1] {
        copies[i].send();
    }
    settle(&mut copies).unwrap();
    assert_eq!(text(&copies[0]), "ab312");
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn a_clash_makes_the_edit_again_on_the_new_version() {
    let room = Room::new(Document::from_text("one two three"));
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    // Ana bolds "two" and puts her caret before "three", while Ben types over "two".
    run(&mut a, "select.text", json!({"text": "two"}));
    run(&mut a, "format.bold", json!({}));
    run(&mut a, "select.text", json!({"text": "three"}));
    run(&mut a, "caret.left", json!({}));
    assert_eq!(a.session().sel.focus.off, "one two ".len());
    run(&mut b, "select.text", json!({"text": "two"}));
    run(&mut b, "text.insert", json!({"text": "2"}));
    b.send();
    // Ana's change clashes with Ben's: it comes back, and Ana's copy bolds what took the place
    // of "two" instead. Her caret stays before "three".
    a.send();
    assert_eq!(a.stats().bounced, 1);
    assert_eq!(a.stats().clashes, 1);
    assert_eq!(a.stats().made_again, 1);
    assert_eq!(text(&a), "one 2 three");
    assert_eq!(bold(&a), "2");
    let s = &a.session().sel;
    assert_eq!((s.anchor.off, s.focus.off), ("one 2 ".len(), "one 2 ".len()));
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    assert_eq!(bold(&copies[1]), "2");
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn edits_made_again_after_a_clash_each_start_where_they_were_made() {
    let room = Room::new(Document::from_text("one two three four"));
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    // Ana bolds "two", then selects "three" and types "X" over it, while Ben types "2" over
    // "two" and sends first. Both of Ana's edits are made again: the second from "three" as it
    // is after Ben's change and her bold made again, not from the offsets it had before them.
    run(&mut a, "select.text", json!({"text": "two"}));
    run(&mut a, "format.bold", json!({}));
    run(&mut a, "select.text", json!({"text": "three"}));
    run(&mut a, "text.insert", json!({"text": "X"}));
    run(&mut a, "caret.docStart", json!({}));
    run(&mut b, "select.text", json!({"text": "two"}));
    run(&mut b, "text.insert", json!({"text": "2"}));
    b.send();
    a.send();
    assert_eq!((a.stats().clashes, a.stats().made_again, a.stats().lost), (1, 2, 0));
    assert_eq!(text(&a), "one 2 X four");
    assert_eq!(bold(&a), "2");
    // Her caret, which she put at the start after her last edit, stays there.
    let s = &a.session().sel;
    assert_eq!((s.anchor.path.0.clone(), s.anchor.off, s.focus.off), (vec![0], 0, 0));
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    assert_eq!(text(&copies[1]), "one 2 X four");
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn a_paste_made_again_pastes_what_was_copied_then() {
    let room = Room::new(Document::from_text("one two three"));
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    // Ana copies "one", pastes it over "two" and copies "three"; Ben types "2" over "two" and
    // sends first. Ana's paste is made again with "one", and her clipboard still holds "three".
    run(&mut a, "select.text", json!({"text": "one"}));
    run(&mut a, "edit.copy", json!({}));
    run(&mut a, "select.text", json!({"text": "two"}));
    run(&mut a, "edit.paste", json!({}));
    run(&mut a, "select.text", json!({"text": "three"}));
    run(&mut a, "edit.copy", json!({}));
    run(&mut b, "select.text", json!({"text": "two"}));
    run(&mut b, "text.insert", json!({"text": "2"}));
    b.send();
    a.send();
    assert_eq!((a.stats().clashes, a.stats().made_again, a.stats().lost), (1, 1, 0));
    assert_eq!(text(&a), "one one three");
    assert_eq!(a.session().clipboard_text, "three");
    assert_eq!(a.session().clip_history.len(), 2);
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn a_cut_made_again_leaves_the_clipboard_as_the_participant_has_it() {
    let room = Room::new(Document::from_text("one two three"));
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    // Ana cuts "two" and copies "three"; Ben types "2" over "two" first. Making her cut again
    // cuts "2", but her clipboard keeps "three" and its pane the two items she put there.
    run(&mut a, "select.text", json!({"text": "two"}));
    run(&mut a, "edit.cut", json!({}));
    run(&mut a, "select.text", json!({"text": "three"}));
    run(&mut a, "edit.copy", json!({}));
    run(&mut b, "select.text", json!({"text": "two"}));
    run(&mut b, "text.insert", json!({"text": "2"}));
    b.send();
    a.send();
    assert_eq!((a.stats().clashes, a.stats().made_again, a.stats().lost), (1, 1, 0));
    assert_eq!(text(&a), "one  three");
    assert_eq!(a.session().clipboard_text, "three");
    assert_eq!(a.session().clip_history.len(), 2);
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn a_column_selection_edit_made_again_keeps_its_column() {
    let room = Room::new(Document::from_text("abc\nabc"));
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    // Ana selects the "b" of both lines as a column and deletes them, while Ben types "B" over
    // the first. Her delete, made again, still takes the column away, not the text between its
    // corners.
    run(&mut a, "select.column", json!({"anchor": Pos::body(0, 1), "focus": Pos::body(1, 2)}));
    run(&mut a, "text.delete", json!({}));
    assert_eq!(text(&a), "ac\nac");
    run(&mut b, "select.text", json!({"text": "b"}));
    run(&mut b, "text.insert", json!({"text": "B"}));
    b.send();
    a.send();
    assert_eq!((a.stats().clashes, a.stats().made_again, a.stats().lost), (1, 1, 0));
    assert_eq!(text(&a), "ac\nac");
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn a_column_edit_kept_past_one_change_and_made_again_after_another_keeps_its_column() {
    let room = Room::new(Document::from_text("abc\nabc"));
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    // Ana deletes the column of "b"s. Ben first types "Z" at the start, which her delete moves
    // past, then types "B" over the first "b", which it clashes with. Made again, it still takes
    // the column away where it moved to.
    run(&mut a, "select.column", json!({"anchor": Pos::body(0, 1), "focus": Pos::body(1, 2)}));
    run(&mut a, "text.delete", json!({}));
    run(&mut b, "caret.docStart", json!({}));
    run(&mut b, "text.insert", json!({"text": "Z"}));
    b.send();
    b.receive();
    run(&mut b, "select.text", json!({"text": "b"}));
    run(&mut b, "text.insert", json!({"text": "B"}));
    b.send();
    a.receive();
    assert_eq!((a.stats().received, a.stats().clashes, a.stats().made_again, a.stats().lost), (2, 1, 1, 0));
    assert_eq!(text(&a), "Zac\nac");
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn an_edit_made_again_after_a_column_edit_takes_its_own_selection() {
    let room = Room::new(Document::from_text("abc\nabc"));
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    // Ana bolds the column of "b"s, leaves the column, selects the text between the same
    // corners and deletes it, while Ben types "B" over the first "b". Made again, her delete
    // takes that text away, not the column her bold had.
    run(&mut a, "select.column", json!({"anchor": Pos::body(0, 1), "focus": Pos::body(1, 2)}));
    run(&mut a, "format.bold", json!({}));
    run(&mut a, "caret.set", json!({"pos": Pos::body(0, 0)}));
    run(&mut a, "select.range", json!({"anchor": Pos::body(0, 1), "focus": Pos::body(1, 2)}));
    run(&mut a, "text.delete", json!({}));
    assert_eq!(text(&a), "ac");
    run(&mut b, "select.text", json!({"text": "b"}));
    run(&mut b, "text.insert", json!({"text": "B"}));
    b.send();
    a.send();
    assert_eq!((a.stats().clashes, a.stats().made_again, a.stats().lost), (1, 2, 0));
    assert_eq!(text(&a), "ac");
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn an_edit_made_again_types_with_the_options_it_was_made_with() {
    let room = Room::new(Document::from_text("one two three"));
    // Overtype is an application option: Ana's an owner.
    let mut a = room.join("Ana", Role::Owner).unwrap();
    let mut b = join(&room, "Ben");
    // Ana bolds "two" and types "X" before "three" in insert mode, then turns Overtype on;
    // Ben types "2" over "two" first. Her "X" is made again in insert mode, and Overtype stays
    // on for her.
    run(&mut a, "select.text", json!({"text": "two"}));
    run(&mut a, "format.bold", json!({}));
    run(&mut a, "select.text", json!({"text": "three"}));
    run(&mut a, "caret.left", json!({}));
    run(&mut a, "text.insert", json!({"text": "X"}));
    run(&mut a, "text.overtype", json!({"value": true}));
    run(&mut b, "select.text", json!({"text": "two"}));
    run(&mut b, "text.insert", json!({"text": "2"}));
    b.send();
    a.send();
    assert_eq!((a.stats().clashes, a.stats().made_again, a.stats().lost), (1, 2, 0));
    assert_eq!(text(&a), "one 2 Xthree");
    assert!(a.session().prefs.overtype);
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn a_cells_edit_made_again_clears_the_same_cells_after_rows_put_in_before_them() {
    let mut s = Session::new(Document::from_text("r1\tx\nr2\ty\nr3\tz"));
    s.run("select.all", &json!({})).unwrap();
    s.run("table.fromText", &json!({})).unwrap();
    let room = Room::new(s.doc.clone());
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    // Ana selects the second column and clears it while Ben puts a row in above the first.
    // Made again, her edit clears the same cells, now one row further down.
    run(&mut a, "select.text", json!({"text": "x"}));
    run(&mut a, "table.selectColumn", json!({}));
    run(&mut a, "text.delete", json!({}));
    assert_eq!(text(&a).split_whitespace().collect::<Vec<_>>(), ["r1", "r2", "r3"]);
    run(&mut b, "select.text", json!({"text": "r1"}));
    run(&mut b, "table.insertRowAbove", json!({}));
    b.send();
    a.send();
    assert_eq!((a.stats().lost, a.stats().read_again), (0, 0));
    assert_eq!(text(&a).split_whitespace().collect::<Vec<_>>(), ["r1", "r2", "r3"]);
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn an_edit_in_a_cell_whose_text_changed_in_a_table_put_back_whole_is_not_made_elsewhere() {
    let mut s = Session::new(Document::from_text("r1\tone three"));
    s.run("select.all", &json!({})).unwrap();
    s.run("table.fromText", &json!({})).unwrap();
    let room = Room::with_hub(Hub::new(s.doc.clone()).keeping(1));
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    // Ana types "X" over "three" while Ben puts a row in above and types "Zone" over "one". Her
    // copy misses both changes and reads the document again: the cell's text changed with the
    // table sent whole, so where "three" was isn't known and her edit is given up, not made
    // over "e thre".
    run(&mut a, "select.text", json!({"text": "three"}));
    run(&mut a, "text.insert", json!({"text": "X"}));
    run(&mut b, "select.text", json!({"text": "r1"}));
    run(&mut b, "table.insertRowAbove", json!({}));
    b.sync();
    run(&mut b, "select.text", json!({"text": "one"}));
    run(&mut b, "text.insert", json!({"text": "Zone"}));
    b.sync();
    assert_eq!(room.version(), 2);
    a.send();
    assert_eq!((a.stats().read_again, a.stats().lost), (1, 1));
    assert_eq!(text(&a).split_whitespace().collect::<Vec<_>>(), ["r1", "Zone", "three"]);
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    same_everywhere(&copies).unwrap();
}

#[test]
fn typing_made_again_keeps_the_formatting_picked_for_it() {
    let room = Room::new(Document::from_text("one two three "));
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    // Ana bolds "two", types "X" over "three", then goes to the end and turns italic on for what
    // she types next; Ben types "2" over "two" first. Her "X" is made again upright, and italic
    // stays picked for her next typing.
    run(&mut a, "select.text", json!({"text": "two"}));
    run(&mut a, "format.bold", json!({}));
    run(&mut a, "select.text", json!({"text": "three"}));
    run(&mut a, "text.insert", json!({"text": "X"}));
    run(&mut a, "caret.docEnd", json!({}));
    run(&mut a, "format.italic", json!({}));
    run(&mut b, "select.text", json!({"text": "two"}));
    run(&mut b, "text.insert", json!({"text": "2"}));
    b.send();
    a.send();
    assert_eq!((a.stats().clashes, a.stats().made_again, a.stats().lost), (1, 2, 0));
    assert_eq!(text(&a), "one 2 X ");
    let p = a.session().doc.body.first().and_then(|b| b.as_para()).unwrap();
    assert_ne!(p.props_of_char("one 2 ".len()).italic, Some(true));
    assert_eq!(a.session().pending.as_ref().and_then(|c| c.italic), Some(true));
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn typing_in_an_equation_made_again_is_typed_in_that_equation() {
    let mut s = Session::new(Document::from_text("one two"));
    s.run("caret.docEnd", &json!({})).unwrap();
    s.run("text.newParagraph", &json!({})).unwrap();
    s.run("insert.equation", &json!({"linear": "x"})).unwrap();
    s.run("equation.exit", &json!({})).unwrap();
    let base = s.doc.clone();
    let room = Room::new(base.clone());
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    // Ana bolds "two", then types in the equation and leaves it; Ben types "2" over "two"
    // first. Her typing is made again in the equation, and she is left outside it.
    run(&mut a, "select.text", json!({"text": "two"}));
    run(&mut a, "format.bold", json!({}));
    run(&mut a, "caret.docEnd", json!({}));
    run(&mut a, "equation.edit", json!({}));
    run(&mut a, "equation.type", json!({"text": "+1"}));
    run(&mut a, "equation.exit", json!({}));
    let typed = a.session().doc.body.get(1).cloned();
    assert_ne!(typed, base.body.get(1).cloned());
    run(&mut b, "select.text", json!({"text": "two"}));
    run(&mut b, "text.insert", json!({"text": "2"}));
    b.send();
    a.send();
    assert_eq!((a.stats().clashes, a.stats().lost), (1, 0));
    assert_eq!(a.session().doc.body.get(1).cloned(), typed);
    assert!(a.session().math.is_none());
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn grouping_made_again_groups_every_object_that_was_selected() {
    let mut s = Session::new(Document::from_text("one two"));
    s.run("caret.docEnd", &json!({})).unwrap();
    for x in [100, 300] {
        s.run("insert.shape", &json!({"kind": "rectangle", "width": 40, "height": 40})).unwrap();
        s.run("arrange.bounds", &json!({"page": 0, "x": x, "y": 100})).unwrap();
        s.run("select.collapse", &json!({"end": true})).unwrap();
    }
    let room = Room::new(s.doc.clone());
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    // Ana bolds "two" and groups the two shapes; Ben types "2" over "two" first. Her grouping
    // is made again with both shapes selected.
    run(&mut a, "select.text", json!({"text": "two"}));
    run(&mut a, "format.bold", json!({}));
    run(&mut a, "select.addObject", json!({"index": 0}));
    run(&mut a, "select.addObject", json!({"index": 1}));
    run(&mut a, "arrange.group", json!({}));
    let grouped = a.session().doc.clone();
    run(&mut b, "select.text", json!({"text": "two"}));
    run(&mut b, "text.insert", json!({"text": "2"}));
    b.send();
    a.send();
    assert_eq!((a.stats().clashes, a.stats().made_again, a.stats().lost), (1, 2, 0));
    let count = |d: &Document| d.body.iter().filter_map(|b| b.as_para()).map(|p| p.text.matches(wordcraft_doc::para::OBJ).count()).sum::<usize>();
    assert_eq!(count(&a.session().doc), count(&grouped));
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn an_edit_whose_text_is_gone_is_not_made_elsewhere() {
    let mut s = Session::new(Document::from_text("one two"));
    s.run("caret.docEnd", &json!({})).unwrap();
    s.run("insert.textBox", &json!({})).unwrap();
    s.run("text.insert", &json!({"text": "box"})).unwrap();
    let in_box = s.sel.focus.clone();
    assert_ne!(in_box.story, StoryRef::Body);
    let room = Room::new(s.doc.clone());
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    // Ana types in the text box while Ben types over the whole body, which takes the box away
    // with its anchor. Ana's typing is given up, not made in the body.
    run(&mut a, "caret.set", json!({"pos": in_box}));
    run(&mut a, "text.insert", json!({"text": " more"}));
    run(&mut b, "select.all", json!({}));
    run(&mut b, "text.insert", json!({"text": "gone"}));
    b.send();
    a.send();
    assert_eq!(a.stats().clashes, 1);
    assert_eq!((a.stats().made_again, a.stats().lost), (0, 1));
    assert_eq!(text(&a), "gone");
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    same_everywhere(&copies).unwrap();
}

#[test]
fn the_hub_takes_the_edits_before_a_clash_and_sends_back_the_rest() {
    let room = Room::new(Document::from_text("one two three"));
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    // Ana types at the end, bolds "two" and types at the start, without sending.
    run(&mut a, "caret.docEnd", json!({}));
    run(&mut a, "text.insert", json!({"text": "!"}));
    run(&mut a, "select.text", json!({"text": "two"}));
    run(&mut a, "format.bold", json!({}));
    run(&mut a, "caret.docStart", json!({}));
    run(&mut a, "text.insert", json!({"text": "> "}));
    // Ben types over "two" first.
    run(&mut b, "select.text", json!({"text": "two"}));
    run(&mut b, "text.insert", json!({"text": "2"}));
    b.send();
    // The hub takes Ana's "!", and sends back her bold and what she typed after it, which
    // her copy makes again after Ben's change.
    a.send();
    assert_eq!(room.version(), 2);
    let st = a.stats();
    assert_eq!((st.sent, st.bounced, st.clashes, st.made_again, st.lost), (3, 2, 1, 2, 0));
    assert_eq!(text(&a), "> one 2 three!");
    assert_eq!(bold(&a), "2");
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    assert_eq!(text(&copies[1]), "> one 2 three!");
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn edits_not_sent_yet_move_past_others_as_they_arrive() {
    let room = Room::new(Document::from_text("Hello world"));
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    run(&mut a, "caret.docEnd", json!({}));
    for t in [" and", " more", " words"] {
        run(&mut a, "text.insert", json!({"text": t}));
    }
    run(&mut b, "caret.docStart", json!({}));
    run(&mut b, "text.insert", json!({"text": "Oh, "}));
    run(&mut b, "text.newParagraph", json!({}));
    b.sync();
    // Ana gets Ben's changes before sending: hers move past them, and her caret stays after
    // what she typed.
    a.receive();
    assert_eq!(text(&a), "Oh, \nHello world and more words");
    assert_eq!(a.session().sel.focus.path.0, vec![1]);
    assert_eq!(a.session().sel.focus.off, "Hello world and more words".len());
    a.check().unwrap();
    a.sync();
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn the_caret_and_selection_move_with_the_text_they_are_in() {
    let room = Room::new(Document::from_text("Hello world"));
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    // Ana selects "world".
    run(&mut a, "select.text", json!({"text": "world"}));
    let id = a.session().doc.body[0].id();
    // Ben puts a paragraph before hers, and types in hers inside the selection.
    run(&mut b, "caret.docStart", json!({}));
    run(&mut b, "text.newParagraph", json!({}));
    run(&mut b, "caret.docStart", json!({}));
    run(&mut b, "text.insert", json!({"text": "Oh, "}));
    run(&mut b, "caret.docEnd", json!({}));
    run(&mut b, "caret.left", json!({}));
    run(&mut b, "text.insert", json!({"text": "-"}));
    b.sync();
    a.receive();
    assert_eq!(text(&a), "Oh, \nHello worl-d");
    let s = &a.session().sel;
    assert_eq!(a.session().doc.para_at(&s.focus).map(|p| p.id), Some(id));
    assert_eq!((s.anchor.path.0.clone(), s.anchor.off, s.focus.off), (vec![1], 6, 12));
    // Enter before the selection in her paragraph: the selection goes with its text into the
    // new paragraph.
    run(&mut b, "caret.docEnd", json!({}));
    for _ in 0.." worl-d".len() {
        run(&mut b, "caret.left", json!({}));
    }
    run(&mut b, "text.newParagraph", json!({}));
    b.sync();
    a.receive();
    assert_eq!(text(&a), "Oh, \nHello\n worl-d");
    let s = &a.session().sel;
    assert_eq!((s.anchor.path.0.clone(), s.anchor.off, s.focus.path.0.clone(), s.focus.off), (vec![2], 1, vec![2], 7));
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn a_copy_that_falls_behind_reads_the_document_again() {
    let room = Room::with_hub(Hub::new(Document::from_text("first\nsecond")).keeping(2));
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    run(&mut b, "caret.docEnd", json!({}));
    run(&mut b, "text.insert", json!({"text": " paragraph"}));
    run(&mut a, "caret.docStart", json!({}));
    for t in ["1", "2", "3"] {
        run(&mut a, "text.insert", json!({"text": t}));
        a.sync();
        a.sync();
    }
    assert_eq!(room.version(), 3);
    // Ben missed changes the hub no longer keeps: his copy reads the document again and
    // types his words again.
    b.receive();
    assert_eq!(b.stats().read_again, 1);
    assert_eq!(b.stats().made_again, 1);
    assert_eq!(text(&b), "123first\nsecond paragraph");
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    same_everywhere(&copies).unwrap();
}

#[test]
fn the_hub_sends_back_what_it_cannot_take() {
    let doc = Document::from_text("Hello");
    let mut hub = Hub::new(doc.clone());
    let base = hub.document().clone();
    // A paragraph put in after the first.
    let mut s = Session::new(base.clone());
    s.run("caret.docEnd", &json!({})).unwrap();
    s.run("text.newParagraph", &json!({})).unwrap();
    let change = ChangeSet::between(&base, &s.doc);
    let by = hub.admit("Ana", Role::Editor).unwrap().id;
    assert!(matches!(hub.submit(1, &change, by), Err(Bounce::Version(_))), "a version the document never had");
    assert!(matches!(hub.submit(0, &ChangeSet::default(), by), Err(Bounce::Refused(_))), "a change that changes nothing");
    assert!(matches!(hub.submit_all(0, &[], by), Err(Bounce::Refused(_))));
    let too_many = vec![change.clone(); crate::room::MAX_BATCH + 1];
    assert!(matches!(hub.submit_all(0, &too_many, by), Err(Bounce::Refused(_))));
    assert!(matches!(hub.submit(u64::MAX, &change, by), Err(Bounce::Version(_))));
    assert_eq!(hub.submit(0, &change, by).map(|t| t.version), Ok(1));
    // The same change again: it puts the paragraph in under the id it has now.
    assert!(matches!(hub.submit(0, &change, by), Err(Bounce::Refused(_))));
    // A change made on another document doesn't apply.
    let other = Document::from_text("Other");
    let mut s = Session::new(other.clone());
    s.run("text.insert", &json!({"text": "x"})).unwrap();
    assert!(matches!(hub.submit(1, &ChangeSet::between(&other, &s.doc), by), Err(Bounce::Refused(_))));
    assert_eq!(hub.version(), 1);
    assert!(hub.since(2).is_err());
    assert_eq!(hub.since(0).map(|v| v.len()), Ok(1));
    // Older than the changes it keeps.
    let mut hub = hub.keeping(1);
    let mut s = Session::new(hub.document().clone());
    let base = s.doc.clone();
    s.run("text.insert", &json!({"text": "x"})).unwrap();
    assert_eq!(hub.submit(1, &ChangeSet::between(&base, &s.doc), by).map(|t| t.version), Ok(2));
    assert!(matches!(hub.since(0), Err(Bounce::Version(_))));
    assert!(matches!(hub.submit(0, &change, by), Err(Bounce::Version(_))));
}

#[test]
fn a_copy_has_the_room_s_document_only() {
    let room = Room::new(sample_document());
    let mut r = join(&room, "Ana");
    for id in ["file.new", "file.open", "file.save", "file.close", "view.switchWindows", "review.compare", "ui.anything"] {
        assert!(matches!(r.run(id, &json!({})), Err(CmdError::Disabled(_))), "{id}");
    }
    // Its own view and selection are its own.
    run(&mut r, "view.zoom", json!({"value": 150}));
    run(&mut r, "select.all", json!({}));
    assert!(!r.has_unconfirmed());
    assert_eq!(room.version(), 0);
}

#[test]
fn copies_on_threads_end_up_holding_the_same_document() {
    const WRITERS: usize = 3;
    let room = Room::new(sample_document());
    let done = Arc::new(Barrier::new(WRITERS));
    let quiet = Arc::new(Barrier::new(WRITERS));
    let docs: Vec<Result<Document, String>> = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..WRITERS)
            .map(|w| {
                let (room, done, quiet) = (room.clone(), done.clone(), quiet.clone());
                scope.spawn(move || {
                    let mut r = join(&room, &format!("writer {w}"));
                    let places = [("caret.docStart", json!({})), ("select.text", json!({"text": "Studio"})), ("caret.docEnd", json!({}))];
                    let (place, at) = &places[w % places.len()];
                    let _ = r.run(place, at);
                    for i in 0..40 {
                        let _ = r.run("text.insert", &json!({"text": format!("{w}{}", i % 10)}));
                        if i % 7 == 3 {
                            let _ = r.run("text.newParagraph", &json!({}));
                        }
                        if i % 5 == 0 {
                            let _ = r.run("caret.left", &json!({"extend": true}));
                            let _ = r.run("format.bold", &json!({}));
                            let _ = r.run("caret.right", &json!({}));
                        }
                        r.sync();
                        if i % 3 == 0 {
                            room.wait_past(r.received(), Duration::from_millis(1));
                        }
                        r.check()?;
                    }
                    done.wait();
                    for _ in 0..200 {
                        if !r.has_unconfirmed() {
                            break;
                        }
                        r.sync();
                    }
                    quiet.wait();
                    r.receive();
                    r.check()?;
                    if r.has_unconfirmed() {
                        return Err(format!("{} kept edits the hub didn't take", r.id()));
                    }
                    Ok(r.session().doc.clone())
                })
            })
            .collect();
        threads.into_iter().map(|t| t.join().unwrap_or_else(|_| Err("a writer panicked".into()))).collect()
    });
    let (_, hub) = room.document();
    for doc in docs {
        let doc = doc.unwrap();
        assert!(doc == hub, "a copy holds other content: {}", difference(&doc, &hub));
        assert_eq!(doc.block_ids(), hub.block_ids());
    }
}

/// What a copy does next in the random test below.
#[derive(Clone, Debug)]
enum Act {
    Edit(Step),
    Send,
    Receive,
}

fn act() -> impl Strategy<Value = Act> {
    prop_oneof![
        6 => edit_step().prop_map(Act::Edit),
        2 => Just(Act::Send),
        2 => Just(Act::Receive),
    ]
}

/// What happened in one case of the random test.
#[derive(Default)]
struct Seen {
    moved: u64,
    bounced: u64,
    clashes: u64,
    made_again: u64,
    lost: u64,
    read_again: u64,
}

/// Three copies of a document (after `warm`), each starting at one of three places, edit,
/// send and receive in the order `acts` gives; then they sync until they settle. Every copy
/// stays as it should be throughout, and they all end up holding the hub's document.
fn copies_at_once(warm: &[Step], places: &[usize], acts: &[(usize, Act)]) -> Result<Seen, String> {
    let mut s = Session::new(sample_document());
    for st in warm {
        match st {
            Step::Type(t) => _ = s.run("text.insert", &json!({"text": t})),
            Step::Command(id, v) => _ = s.run(id, v),
            Step::UndoRedo => {
                _ = s.run("edit.undo", &json!({}));
                _ = s.run("edit.redo", &json!({}));
            }
        }
    }
    let room = Room::new(s.doc.clone());
    let at = [("caret.docStart", json!({})), ("select.text", json!({"text": "Studio"})), ("caret.docEnd", json!({}))];
    let mut copies: Vec<Replica> = places
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mut r = join(&room, &format!("copy {i}"));
            if let Some((id, v)) = at.get(*p) {
                _ = r.run(id, v);
            }
            r
        })
        .collect();
    for (k, (i, a)) in acts.iter().enumerate() {
        let Some(r) = copies.get_mut(*i) else { continue };
        match a {
            Act::Edit(Step::Type(t)) => _ = r.run("text.insert", &json!({"text": t})),
            Act::Edit(Step::Command(id, v)) => _ = r.run(id, v),
            Act::Edit(Step::UndoRedo) => {
                _ = r.run("edit.undo", &json!({}));
                _ = r.run("edit.redo", &json!({}));
            }
            Act::Send => r.send(),
            Act::Receive => r.receive(),
        }
        r.check().map_err(|e| format!("after act {k} ({a:?}) of {}: {e}", r.id()))?;
    }
    settle(&mut copies)?;
    same_everywhere(&copies)?;
    let mut seen = Seen::default();
    for r in &copies {
        let st = r.stats();
        seen.moved += st.moved;
        seen.bounced += st.bounced;
        seen.lost += st.lost;
        seen.clashes += st.clashes;
        seen.made_again += st.made_again;
        seen.read_again += st.read_again;
    }
    Ok(seen)
}

/// Copies editing at once, sending and receiving in random orders (typing, selecting, deleting
/// and formatting, with paragraph breaks, cut and paste, tables, notes, comments, text boxes,
/// undo and redo among the edits; 150 cases, the same each run): each copy stays its last
/// version with its own edits made in it, and they all end up holding the same document under
/// the same ids. In many cases the hub moves changes past others; in some, edits clash and are
/// made again; no copy ever has to read the whole document again.
#[test]
fn copies_editing_at_once_end_up_holding_the_same_document() {
    const CASES: usize = 150;
    let mut runner = TestRunner::deterministic();
    let cases =
        (proptest::collection::vec(step(), 0..5), proptest::collection::vec(0usize..3, 3), proptest::collection::vec((0usize..3, act()), 10..40));
    let mut failures = Vec::new();
    let (mut moved, mut clashed, mut made_again, mut bounced, mut lost) = (0, 0, 0, 0, 0);
    for case in 0..CASES {
        let Ok(tree) = cases.new_tree(&mut runner) else { continue };
        let (warm, places, acts) = tree.current();
        match copies_at_once(&warm, &places, &acts) {
            Ok(seen) => {
                moved += usize::from(seen.moved > 0);
                clashed += usize::from(seen.clashes > 0);
                made_again += seen.made_again;
                bounced += usize::from(seen.bounced > 0);
                lost += seen.lost;
                if seen.read_again > 0 {
                    failures.push(format!("case {case}: a copy read the document again\n  warm {warm:?}\n  places {places:?}\n  acts {acts:?}"));
                }
            }
            Err(e) => failures.push(format!("case {case}: {e}\n  warm {warm:?}\n  places {places:?}\n  acts {acts:?}")),
        }
    }
    assert!(failures.is_empty(), "{} of {CASES} cases failed:\n{}", failures.len(), failures.join("\n"));
    assert!(moved >= CASES / 2, "too few cases where the hub moved a change past others: {moved}");
    assert!(clashed >= CASES / 20, "too few cases with a clash: {clashed}");
    eprintln!(
        "{CASES} cases: {moved} with changes moved past others, {clashed} with clashes ({made_again} commands made again, {lost} lost), {bounced} with changes the hub sent back"
    );
}

#[test]
fn deleting_ink_made_again_deletes_the_ink_lassoed_for_it() {
    let mut s = Session::new(Document::from_text("one two three"));
    s.run("draw.stroke", &json!({"page": 0, "points": [[100, 100], [140, 110]]})).unwrap();
    s.run("draw.stroke", &json!({"page": 0, "points": [[300, 300], [340, 310]]})).unwrap();
    let room = Room::new(s.doc.clone());
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    let ink =
        |r: &Replica| r.session().doc.body.iter().filter_map(|b| b.as_para()).flat_map(|p| p.objects.iter()).filter(|o| o.ink().is_some()).count();
    // Ana bolds "two", lassoes the first stroke and deletes it; Ben types "2" over "two"
    // first. Her delete is made again on the stroke she lassoed, and the other stays.
    run(&mut a, "select.text", json!({"text": "two"}));
    run(&mut a, "format.bold", json!({}));
    run(&mut a, "draw.lasso", json!({"page": 0, "points": [[80, 80], [200, 80], [200, 150], [80, 150]]}));
    run(&mut a, "draw.lassoDelete", json!({}));
    assert_eq!(ink(&a), 1);
    run(&mut b, "select.text", json!({"text": "two"}));
    run(&mut b, "text.insert", json!({"text": "2"}));
    b.send();
    a.send();
    assert_eq!((a.stats().clashes, a.stats().lost), (1, 0));
    assert_eq!((text(&a).as_str(), ink(&a)), ("one 2 three", 1));
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn highlighting_made_again_uses_the_colour_picked_for_it() {
    let room = Room::new(Document::from_text("one two three"));
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    let highlight = |r: &Replica, word: &str| {
        let d = &r.session().doc;
        let p = d.body.iter().find_map(|b| b.as_para())?;
        let at = p.text.find(word)?;
        p.props_of_char(at).highlight
    };
    // Ana highlights "two" with the last colour (yellow), then picks green with the caret
    // elsewhere; Ben types "2" over "two" first. Her highlight is made again in yellow, and
    // green stays picked for her.
    run(&mut a, "select.text", json!({"text": "two"}));
    run(&mut a, "format.highlight", json!({}));
    let yellow = highlight(&a, "two");
    assert!(yellow.is_some_and(|h| h != wordcraft_doc::props::Highlight::None));
    run(&mut a, "select.collapse", json!({"end": true}));
    run(&mut a, "format.highlight", json!({"color": "green"}));
    run(&mut b, "select.text", json!({"text": "two"}));
    run(&mut b, "text.insert", json!({"text": "2"}));
    b.send();
    a.send();
    assert_eq!((a.stats().clashes, a.stats().lost), (1, 0));
    assert_eq!(text(&a), "one 2 three");
    assert_eq!(highlight(&a, "2"), yellow);
    assert_eq!(a.session().last_highlight, wordcraft_doc::props::Highlight::BrightGreen);
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn an_edit_of_a_new_comment_made_again_edits_that_comment() {
    let room = Room::new(Document::from_text("one two three"));
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    let comments = |r: &Replica| {
        let d = &r.session().doc;
        let mut t: Vec<String> = d.comments.values().map(|c| d.plain_text(wordcraft_doc::StoryRef::Part(c.part))).collect();
        t.sort();
        t
    };
    // Ana comments on "two" and edits her comment; Ben comments on "one" first, under the same
    // new id. Ana's comment is made again under another id, and her edit edits it, not Ben's.
    run(&mut a, "select.text", json!({"text": "two"}));
    run(&mut a, "review.newComment", json!({"text": "A"}));
    let id = a.session().doc.comments.keys().next().copied().unwrap();
    run(&mut a, "review.editComment", json!({"id": id, "text": "A2"}));
    run(&mut b, "select.text", json!({"text": "one"}));
    run(&mut b, "review.newComment", json!({"text": "B"}));
    assert_eq!(b.session().doc.comments.keys().next().copied(), Some(id));
    b.send();
    a.send();
    assert_eq!((a.stats().clashes, a.stats().lost), (1, 0));
    assert_eq!(comments(&a), ["A2", "B"]);
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn a_repeated_reply_made_again_replies_to_the_comment_it_was_made_on() {
    let room = Room::new(Document::from_text("one two three"));
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    // Ana comments on "two", replies to her comment and repeats the reply; Ben comments on
    // "one" first, under the same new id. Ana's comment is made again under another id, and
    // both replies, the repeated one too, are replies to it.
    run(&mut a, "select.text", json!({"text": "two"}));
    run(&mut a, "review.newComment", json!({"text": "A"}));
    let id = a.session().doc.comments.keys().next().copied().unwrap();
    run(&mut a, "review.reply", json!({"id": id, "text": "R"}));
    run(&mut a, "edit.repeat", json!({}));
    run(&mut b, "select.text", json!({"text": "one"}));
    run(&mut b, "review.newComment", json!({"text": "B"}));
    b.send();
    a.send();
    assert_eq!((a.stats().clashes, a.stats().lost), (1, 0));
    let d = &a.session().doc;
    let by_text = |t: &str| d.comments.iter().find(|(_, c)| d.plain_text(StoryRef::Part(c.part)) == t).map(|(id, _)| *id);
    let ana = by_text("A").unwrap();
    let parents: Vec<Option<u32>> = d.comments.values().filter(|c| d.plain_text(StoryRef::Part(c.part)) == "R").map(|c| c.parent).collect();
    assert_eq!(parents, [Some(ana), Some(ana)]);
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn repeat_after_a_reply_made_again_replies_to_the_comment_it_was_made_on() {
    let room = Room::new(Document::from_text("one two three"));
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    // Ana comments on "two" and replies to her comment; Ben comments on "one" first, under the
    // same new id. Once her edits are made again, Repeat replies to her comment again, not to
    // Ben's.
    run(&mut a, "select.text", json!({"text": "two"}));
    run(&mut a, "review.newComment", json!({"text": "A"}));
    let id = a.session().doc.comments.keys().next().copied().unwrap();
    run(&mut a, "review.reply", json!({"id": id, "text": "R"}));
    run(&mut b, "select.text", json!({"text": "one"}));
    run(&mut b, "review.newComment", json!({"text": "B"}));
    b.send();
    a.send();
    assert_eq!((a.stats().clashes, a.stats().lost), (1, 0));
    run(&mut a, "edit.repeat", json!({}));
    let d = &a.session().doc;
    let by_text = |t: &str| d.comments.iter().find(|(_, c)| d.plain_text(StoryRef::Part(c.part)) == t).map(|(id, _)| *id);
    let ana = by_text("A").unwrap();
    let parents: Vec<Option<u32>> = d.comments.values().filter(|c| d.plain_text(StoryRef::Part(c.part)) == "R").map(|c| c.parent).collect();
    assert_eq!(parents, [Some(ana), Some(ana)]);
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn edits_in_a_new_text_box_made_again_are_made_in_that_box() {
    let room = Room::new(Document::from_text("one two"));
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    let boxes = |r: &Replica| {
        let d = &r.session().doc;
        let mut t: Vec<String> = d.parts.keys().map(|k| d.plain_text(StoryRef::Part(*k))).filter(|t| !t.is_empty()).collect();
        t.sort();
        t
    };
    // Ana and Ben each put a text box in and type "abc" in it, Ben first; then Ana types "X"
    // over her "b". Ana's box is made again as another part, and her "X" goes in it, not in
    // Ben's box.
    for r in [&mut a, &mut b] {
        run(r, "caret.docEnd", json!({}));
        run(r, "insert.textBox", json!({}));
        run(r, "text.insert", json!({"text": "abc"}));
    }
    let end = a.session().sel.focus.clone();
    assert_ne!(end.story, StoryRef::Body);
    let at = |off| Pos { off, ..end.clone() };
    run(&mut a, "select.range", json!({"anchor": at(1), "focus": at(2)}));
    run(&mut a, "text.insert", json!({"text": "X"}));
    assert_eq!(boxes(&a), ["aXc"]);
    b.send();
    a.send();
    assert_eq!((a.stats().clashes, a.stats().lost), (1, 0));
    assert_eq!(boxes(&a), ["aXc", "abc"]);
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn a_copy_refuses_file_commands_run_inside_a_macro() {
    let room = Room::new(Document::from_text("one"));
    // An owner, whose role runs macros (the application's settings).
    let mut r = room.join("Ana", Role::Owner).unwrap();
    let dir = std::env::temp_dir().join(format!("wordcraft-room-macro-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("saved.docx");
    let _ = std::fs::remove_file(&path);
    // A macro whose steps save the document: running it is refused at the save, which writes
    // nothing.
    let steps = json!([{"command": "text.insert", "params": {"text": "x"}}, {"command": "file.saveAs", "params": {"path": path}}]);
    run(&mut r, "tools.macros", json!({"define": {"name": "save", "steps": steps}}));
    assert!(matches!(r.run("tools.macros", &json!({"run": "save"})), Err(CmdError::Disabled(_))));
    assert!(!path.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_macro_made_again_makes_its_steps_where_they_were_made() {
    let room = Room::new(Document::from_text("one two three four"));
    // Macros are the application's: an owner's to run.
    let mut a = room.join("Ana", Role::Owner).unwrap();
    let mut b = join(&room, "Ben");
    // Ana bolds "two", then runs a macro that selects "three" by its offsets and types "X" over
    // it; Ben types "2" over "two" first. The macro's selection moves with "three".
    run(&mut a, "select.text", json!({"text": "two"}));
    run(&mut a, "format.bold", json!({}));
    let steps = json!([
        {"command": "select.range", "params": {"anchor": Pos::body(0, 8), "focus": Pos::body(0, 13)}},
        {"command": "text.insert", "params": {"text": "X"}},
    ]);
    run(&mut a, "tools.macros", json!({"define": {"name": "x", "steps": steps}}));
    run(&mut a, "tools.macros", json!({"run": "x"}));
    assert_eq!(text(&a), "one two X four");
    run(&mut b, "select.text", json!({"text": "two"}));
    run(&mut b, "text.insert", json!({"text": "2"}));
    b.send();
    a.send();
    assert_eq!((a.stats().clashes, a.stats().lost), (1, 0));
    assert_eq!(text(&a), "one 2 X four");
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn a_comment_made_again_keeps_the_name_it_was_made_under() {
    let room = Room::new(Document::from_text("one two three"));
    // The user name is the application's: an owner's to change.
    let mut a = room.join("Ana", Role::Owner).unwrap();
    let mut b = join(&room, "Ben");
    // Ana comments on "two" and then changes her name to Zoe; Ben comments on "one" first,
    // under the same new id. Ana's comment is made again under her name then.
    run(&mut a, "select.text", json!({"text": "two"}));
    run(&mut a, "review.newComment", json!({"text": "A"}));
    run(&mut a, "file.setAuthor", json!({"name": "Zoe"}));
    run(&mut b, "select.text", json!({"text": "one"}));
    run(&mut b, "review.newComment", json!({"text": "B"}));
    b.send();
    a.send();
    assert_eq!((a.stats().clashes, a.stats().lost), (1, 0));
    let d = &a.session().doc;
    let mut by: Vec<(String, String)> = d.comments.values().map(|c| (d.plain_text(StoryRef::Part(c.part)), c.author.clone())).collect();
    by.sort();
    assert_eq!(by, [("A".to_string(), "Ana".to_string()), ("B".to_string(), "Ben".to_string())]);
    assert_eq!(a.session().author, "Zoe");
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn a_tracked_cut_made_again_is_the_one_its_paste_moves() {
    let room = Room::new(Document::from_text("First one.\nSecond one.\nThird one.\nFourth one."));
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    run(&mut a, "review.trackChanges", json!({"value": true}));
    a.send();
    a.receive();
    b.receive();
    // Ana and Ben each cut a paragraph under Track Changes, Ben first, both under the first new
    // revision. Ana's cut is made again under another revision; when she then pastes, her
    // paste moves what she cut, not what Ben did.
    run(&mut a, "select.range", json!({"anchor": Pos::body(1, 0), "focus": Pos::body(2, 0)}));
    run(&mut a, "edit.cut", json!({}));
    run(&mut b, "select.range", json!({"anchor": Pos::body(3, 0), "focus": Pos::body(3, "Fourth one.".len())}));
    run(&mut b, "edit.cut", json!({}));
    b.send();
    a.send();
    assert_eq!((a.stats().clashes, a.stats().lost), (1, 0));
    run(&mut a, "caret.set", json!({"pos": Pos::body(0, 0)}));
    run(&mut a, "edit.paste", json!({}));
    let d = &a.session().doc;
    let moved: Vec<&str> = d.revisions.iter().filter(|r| r.move_name.is_some()).map(|r| r.author.as_str()).collect();
    assert_eq!(moved, ["Ana", "Ana"]);
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn a_paste_that_moved_nothing_made_again_after_its_cut_still_moves_nothing() {
    let room = Room::new(Document::from_text("First one.\nSecond one.\nThird one. "));
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    run(&mut a, "review.trackChanges", json!({"value": true}));
    a.send();
    a.receive();
    b.receive();
    // Ana cuts the second paragraph, turns Bold on at the end of the last (which changes no
    // text, but lets the cut go: the paste after it is no move) and pastes; Ben types at the
    // end of the first first. Made again, her cut is a cut a paste could move, but her paste
    // still isn't a move.
    run(&mut a, "select.range", json!({"anchor": Pos::body(1, 0), "focus": Pos::body(2, 0)}));
    run(&mut a, "edit.cut", json!({}));
    run(&mut a, "caret.set", json!({"pos": Pos::body(2, "Third one. ".len())}));
    run(&mut a, "format.bold", json!({}));
    run(&mut a, "edit.paste", json!({}));
    let moves = |r: &Replica| r.session().doc.revisions.iter().filter(|r| r.move_name.is_some()).count();
    assert_eq!(moves(&a), 0);
    run(&mut b, "caret.set", json!({"pos": Pos::body(0, "First one.".len())}));
    run(&mut b, "text.insert", json!({"text": " Ben"}));
    b.send();
    a.send();
    assert_eq!((a.stats().clashes, a.stats().lost), (1, 0));
    assert_eq!(moves(&a), 0);
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn a_cut_let_go_before_its_edits_are_made_again_stays_let_go() {
    let room = Room::new(Document::from_text("First one.\nSecond one.\nThird one. "));
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    run(&mut a, "review.trackChanges", json!({"value": true}));
    a.send();
    a.receive();
    b.receive();
    // Ana cuts the second paragraph and turns Bold on at the end of the last, which lets the
    // cut go; Ben types at the end of the first first. Once her cut is made again, the paste
    // she makes next still isn't a move.
    run(&mut a, "select.range", json!({"anchor": Pos::body(1, 0), "focus": Pos::body(2, 0)}));
    run(&mut a, "edit.cut", json!({}));
    run(&mut a, "caret.set", json!({"pos": Pos::body(2, "Third one. ".len())}));
    run(&mut a, "format.bold", json!({}));
    run(&mut b, "caret.set", json!({"pos": Pos::body(0, "First one.".len())}));
    run(&mut b, "text.insert", json!({"text": " Ben"}));
    b.send();
    a.send();
    assert_eq!((a.stats().clashes, a.stats().lost), (1, 0));
    run(&mut a, "edit.paste", json!({}));
    let moves = |r: &Replica| r.session().doc.revisions.iter().filter(|r| r.move_name.is_some()).count();
    assert_eq!(moves(&a), 0);
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

#[test]
fn a_tracked_paste_made_again_still_moves_what_was_cut() {
    let room = Room::new(Document::from_text("First one.\nSecond one.\nThird one."));
    let mut a = join(&room, "Ana");
    let mut b = join(&room, "Ben");
    run(&mut a, "review.trackChanges", json!({"value": true}));
    // Ana cuts the second paragraph, and the hub takes the cut; then she pastes it at the start
    // while Ben types at the end first, a tracked insertion of his own. Her paste, made again,
    // is still the other end of the move her cut began.
    run(&mut a, "select.range", json!({"anchor": Pos::body(1, 0), "focus": Pos::body(2, 0)}));
    run(&mut a, "edit.cut", json!({}));
    a.send();
    a.receive();
    b.receive();
    run(&mut a, "caret.set", json!({"pos": Pos::body(0, 0)}));
    run(&mut a, "edit.paste", json!({}));
    let moves = |r: &Replica| r.session().doc.revisions.iter().filter(|r| r.move_name.is_some()).count();
    assert_eq!(moves(&a), 2);
    run(&mut b, "caret.docEnd", json!({}));
    run(&mut b, "text.insert", json!({"text": " Ben"}));
    b.send();
    a.send();
    assert_eq!((a.stats().clashes, a.stats().lost), (1, 0));
    assert_eq!(moves(&a), 2);
    let mut copies = [a, b];
    settle(&mut copies).unwrap();
    smooth(&copies);
    same_everywhere(&copies).unwrap();
}

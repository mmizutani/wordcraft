//! Undo per participant (milestone A1.4): in a shared document, Undo and Redo take the
//! participant's own steps back, past the changes others made since, and leave those in. When
//! one of them changed what the step changed, the step stays and Undo names who made it.

use serde_json::{Value, json};
use wordcraft_doc::change::Side;
use wordcraft_doc::{ChangeSet, Document, Pos, StoryRef};

use super::room::{join, run, same_everywhere, settle, smooth, text};
use crate::room::{Replica, Room};
use crate::{CmdError, Session};

fn body(s: &Session) -> String {
    s.doc.plain_text(StoryRef::Body)
}

fn cmd(s: &mut Session, id: &str, v: Value) -> Value {
    match s.run(id, &v) {
        Ok(out) => out,
        Err(e) => panic!("{id} {v}: {e}"),
    }
}

/// What command `id` says when it fails.
fn refused(r: &mut Replica, id: &str) -> String {
    match r.run(id, &json!({})) {
        Ok(v) => panic!("{id} ran: {v}"),
        Err(e) => e.to_string(),
    }
}

/// A session holding `text`, with block ids, and the document it started with.
fn session(text: &str) -> (Session, Document) {
    let s = Session::new(Document::from_text(text));
    let base = s.doc.clone();
    (s, base)
}

/// The changes another session makes when it runs `commands` on `doc`, one per command.
fn theirs(doc: &Document, commands: &[(&str, Value)]) -> Vec<ChangeSet> {
    let mut other = Session::new(doc.clone());
    let mut out = Vec::new();
    for (id, v) in commands {
        let was = other.doc.clone();
        cmd(&mut other, id, v.clone());
        out.push(ChangeSet::between(&was, &other.doc));
    }
    out
}

/// Make `changes`, made one after the other on `base`, in `s`, past the edits `s` made since
/// `base` (as a room's copy does): participant `by`'s.
fn apply_past(s: &mut Session, base: &Document, changes: &[ChangeSet], by: &str) {
    let mut mine = ChangeSet::between(base, &s.doc);
    for cs in changes {
        let moved = cs.rebase(&mine, Side::Before).unwrap();
        mine = mine.rebase(cs, Side::After).unwrap();
        s.apply_theirs(&moved, by).unwrap();
    }
}

/// Make `changes`, made on the document `s` has, in it: participant `by`'s.
fn apply(s: &mut Session, changes: &[ChangeSet], by: &str) {
    let base = s.doc.clone();
    apply_past(s, &base, changes, by);
}

#[test]
fn undo_takes_the_session_s_own_step_back_and_leaves_others_changes() {
    let (mut s, base) = session("Hello world\nSecond");
    cmd(&mut s, "caret.docStart", json!({}));
    cmd(&mut s, "text.insert", json!({"text": "Hi "}));
    // Ben, on the version before it, types at the end and puts a paragraph first.
    let ben = theirs(
        &base,
        &[("caret.docEnd", json!({})), ("text.insert", json!({"text": "!"})), ("caret.docStart", json!({})), ("text.newParagraph", json!({}))],
    );
    apply_past(&mut s, &base, &ben, "Ben");
    assert_eq!(body(&s), "\nHi Hello world\nSecond!");
    assert!(s.can_undo());
    assert_eq!(s.undo_label(), Some("Typing"));
    // Undo takes "Hi " back, and the caret back to where it was, in the paragraph that moved.
    cmd(&mut s, "edit.undo", json!({}));
    assert_eq!(body(&s), "\nHello world\nSecond!");
    assert_eq!((s.sel.focus.path.0.clone(), s.sel.focus.off), (vec![1], 0));
    assert!(!s.can_undo());
    // Redo puts it back, Ben's changes still in.
    cmd(&mut s, "edit.redo", json!({}));
    assert_eq!(body(&s), "\nHi Hello world\nSecond!");
    assert_eq!((s.sel.focus.path.0.clone(), s.sel.focus.off), (vec![1], 3));
    cmd(&mut s, "edit.undo", json!({}));
    assert_eq!(body(&s), "\nHello world\nSecond!");
}

#[test]
fn undo_refuses_a_step_whose_text_another_participant_changed_and_names_them() {
    let (mut s, _) = session("Hello world");
    cmd(&mut s, "caret.docStart", json!({}));
    cmd(&mut s, "text.insert", json!({"text": "Hi "}));
    // Ben, who has "Hi " too, makes it bold.
    let ben = theirs(&s.doc, &[("select.text", json!({"text": "Hi"})), ("format.bold", json!({}))]);
    apply(&mut s, &ben, "Ben");
    let before = (s.doc.clone(), s.sel.clone());
    let err = s.run("edit.undo", &json!({})).unwrap_err();
    assert!(matches!(err, CmdError::Failed(_)));
    assert!(err.to_string().contains("Typing can't be undone: Ben has changed it since"), "{err}");
    // Nothing moved: the step is still there, and so is the text.
    assert_eq!((s.doc.clone(), s.sel.clone()), before);
    assert!(s.can_undo());
    assert_eq!(body(&s), "Hi Hello world");
    // A change that came without a name is another participant's.
    let mut s2 = Session::new(Document::from_text("one"));
    cmd(&mut s2, "caret.docEnd", json!({}));
    cmd(&mut s2, "text.insert", json!({"text": " two"}));
    let other = theirs(&s2.doc, &[("select.text", json!({"text": "two"})), ("text.insert", json!({"text": "2"}))]);
    for cs in &other {
        s2.apply_change(cs).unwrap();
    }
    let err = s2.run("edit.undo", &json!({})).unwrap_err();
    assert!(err.to_string().contains("another participant has changed it since"), "{err}");
    // The Undo list stops at it, and says why.
    let err = s2.run("edit.undoTo", &json!({"count": 3})).unwrap_err();
    assert!(err.to_string().contains("edit.undoTo: Typing can't be undone"), "{err}");
}

#[test]
fn the_undo_list_takes_steps_back_until_one_another_participant_changed() {
    let (mut s, _) = session("one\ntwo");
    cmd(&mut s, "caret.docEnd", json!({}));
    cmd(&mut s, "text.insert", json!({"text": " A"}));
    cmd(&mut s, "caret.docStart", json!({}));
    cmd(&mut s, "text.insert", json!({"text": "B "}));
    // Ben changes " A", the older step's text: the newer step is undone, then it stops.
    let ben = theirs(&s.doc, &[("select.text", json!({"text": "A"})), ("format.italic", json!({}))]);
    apply(&mut s, &ben, "Ben");
    let out = cmd(&mut s, "edit.undoTo", json!({"count": 2}));
    assert_eq!(out["undone"], 1);
    assert!(out["stopped"].as_str().is_some_and(|w| w.contains("Ben has changed it since")), "{out}");
    assert_eq!(body(&s), "one\ntwo A");
}

#[test]
fn others_changes_after_a_step_do_not_count_as_the_step_s_protection() {
    // Ben turns protection on after the session typed: Undo takes the typing back, and only
    // the typing (only Stop Protection lifts it).
    let (mut s, _) = session("Hello");
    cmd(&mut s, "caret.docEnd", json!({}));
    cmd(&mut s, "text.insert", json!({"text": " world"}));
    let ben = theirs(&s.doc, &[("review.restrict", json!({"mode": "trackedChanges"}))]);
    apply(&mut s, &ben, "Ben");
    assert!(!s.step_lifts_protection(false));
    cmd(&mut s, "edit.undo", json!({}));
    assert_eq!(body(&s), "Hello");
    assert_eq!(s.doc.settings.protection.as_deref(), Some("trackedChanges"));
    // A step that changed the protection itself still needs Stop Protection when nobody
    // changed the settings since.
    let (mut s, _) = session("Hello");
    cmd(&mut s, "review.restrict", json!({"mode": "trackedChanges"}));
    let ben = theirs(&s.doc, &[("caret.docEnd", json!({})), ("text.insert", json!({"text": "!"}))]);
    apply(&mut s, &ben, "Ben");
    assert!(s.step_lifts_protection(false));
}

#[test]
fn what_is_typed_after_others_change_is_a_step_of_its_own() {
    let (mut s, _) = session("one\ntwo");
    cmd(&mut s, "caret.docEnd", json!({}));
    cmd(&mut s, "text.insert", json!({"text": " a"}));
    cmd(&mut s, "text.insert", json!({"text": "b"}));
    let ben = theirs(&s.doc, &[("caret.docStart", json!({})), ("text.insert", json!({"text": "Ben "}))]);
    apply(&mut s, &ben, "Ben");
    // The typing group ends with Ben's change: "cd" is another step, and an input method's
    // rewrite after it doesn't open the group again.
    s.continue_typing();
    assert!(!s.typing_open());
    cmd(&mut s, "text.insert", json!({"text": "cd"}));
    assert_eq!(s.undo_depth(), 2);
    cmd(&mut s, "edit.undo", json!({}));
    assert_eq!(body(&s), "Ben one\ntwo ab");
    cmd(&mut s, "edit.undo", json!({}));
    assert_eq!(body(&s), "Ben one\ntwo");
    // A drag joined to the step before others' changes is a step of its own too.
    let (mut s, _) = session("one");
    let mark = s.undo_mark();
    cmd(&mut s, "caret.docEnd", json!({}));
    cmd(&mut s, "text.insert", json!({"text": "1"}));
    let ben = theirs(&s.doc, &[("caret.docStart", json!({})), ("text.insert", json!({"text": "B"}))]);
    apply(&mut s, &ben, "Ben");
    s.join_next_undo();
    cmd(&mut s, "text.insert", json!({"text": "2"}));
    assert_eq!(s.undo_depth(), 2);
    // Merging the steps since the mark keeps them apart: one step would take Ben's "B" back.
    s.merge_undo_since(mark);
    assert_eq!(s.undo_depth(), 2);
    cmd(&mut s, "edit.undo", json!({}));
    cmd(&mut s, "edit.undo", json!({}));
    assert_eq!(body(&s), "Bone");
}

#[test]
fn steps_merged_after_others_changes_keep_them() {
    // Steps after the last of others' changes merge, and keep the changes that came after them.
    let (mut s, _) = session("one");
    cmd(&mut s, "caret.docEnd", json!({}));
    cmd(&mut s, "text.insert", json!({"text": "1"}));
    let ben = theirs(&s.doc, &[("caret.docStart", json!({})), ("text.insert", json!({"text": "B"}))]);
    apply(&mut s, &ben, "Ben");
    let mark = s.undo_mark();
    cmd(&mut s, "text.insert", json!({"text": "2"}));
    cmd(&mut s, "caret.docEnd", json!({}));
    cmd(&mut s, "format.bold", json!({}));
    cmd(&mut s, "text.insert", json!({"text": "3"}));
    let cy = theirs(&s.doc, &[("caret.docStart", json!({})), ("text.insert", json!({"text": "C"}))]);
    apply(&mut s, &cy, "Cy");
    s.merge_undo_since(mark);
    assert_eq!(s.undo_depth(), 2);
    cmd(&mut s, "edit.undo", json!({}));
    assert_eq!(body(&s), "CBone1");
    cmd(&mut s, "edit.undo", json!({}));
    assert_eq!(body(&s), "CBone");
}

#[test]
fn a_long_run_of_others_changes_is_kept_as_one_change() {
    let (mut s, _) = session("Mine\nTheirs");
    cmd(&mut s, "caret.set", json!({"pos": Pos::body(0, 4)}));
    cmd(&mut s, "text.insert", json!({"text": " <"}));
    // Ben types 1,200 characters one by one at the start of the same paragraph, each moving
    // the session's step along, and 1,200 more at the end of the second.
    let mut ben = Session::new(s.doc.clone());
    for at in ["caret.docStart", "caret.docEnd"] {
        cmd(&mut ben, at, json!({}));
        for _ in 0..1200 {
            let was = ben.doc.clone();
            cmd(&mut ben, "text.insert", json!({"text": "x"}));
            s.apply_theirs(&ChangeSet::between(&was, &ben.doc), "Ben").unwrap();
        }
    }
    let xs = "x".repeat(1200);
    assert_eq!(body(&s), format!("{xs}Mine <\nTheirs{xs}"));
    cmd(&mut s, "edit.undo", json!({}));
    assert_eq!(body(&s), format!("{xs}Mine\nTheirs{xs}"));
    cmd(&mut s, "edit.redo", json!({}));
    assert_eq!(body(&s), format!("{xs}Mine <\nTheirs{xs}"));
}

#[test]
fn a_participant_s_undo_leaves_the_others_edits_in_every_copy() {
    let room = Room::new(Document::from_text("First\nSecond\nThird"));
    let mut c = [join(&room, "Ana"), join(&room, "Ben")];
    // Ana types in the first paragraph, Ben in the third and at the end of the first.
    run(&mut c[0], "caret.set", json!({"pos": Pos::body(0, 0)}));
    run(&mut c[0], "text.insert", json!({"text": "Ana: "}));
    run(&mut c[1], "caret.set", json!({"pos": Pos::body(2, "Third".len())}));
    run(&mut c[1], "text.insert", json!({"text": " (Ben)"}));
    run(&mut c[1], "caret.set", json!({"pos": Pos::body(0, "First".len())}));
    run(&mut c[1], "text.insert", json!({"text": "!"}));
    settle(&mut c).unwrap();
    assert_eq!(text(&c[0]), "Ana: First!\nSecond\nThird (Ben)");
    // Ana's Undo takes her typing back only; Ben's Undo then takes his last step back.
    run(&mut c[0], "edit.undo", json!({}));
    assert_eq!(text(&c[0]), "First!\nSecond\nThird (Ben)");
    settle(&mut c).unwrap();
    assert_eq!(text(&c[1]), "First!\nSecond\nThird (Ben)");
    run(&mut c[1], "edit.undo", json!({}));
    settle(&mut c).unwrap();
    assert_eq!(text(&c[0]), "First\nSecond\nThird (Ben)");
    // Ana's Redo puts her typing back, Ben's "!" still undone.
    run(&mut c[0], "edit.redo", json!({}));
    settle(&mut c).unwrap();
    assert_eq!(text(&c[1]), "Ana: First\nSecond\nThird (Ben)");
    smooth(&c);
    same_everywhere(&c).unwrap();
}

#[test]
fn a_participant_s_undo_is_refused_when_another_changed_their_text() {
    let room = Room::new(Document::from_text("one two three"));
    let mut c = [join(&room, "Ana"), join(&room, "Ben")];
    run(&mut c[0], "select.text", json!({"text": "two"}));
    run(&mut c[0], "text.insert", json!({"text": "TWO"}));
    settle(&mut c).unwrap();
    run(&mut c[1], "select.text", json!({"text": "TWO"}));
    run(&mut c[1], "format.bold", json!({}));
    settle(&mut c).unwrap();
    let why = refused(&mut c[0], "edit.undo");
    assert!(why.contains("Typing can't be undone: Ben has changed it since"), "{why}");
    assert!(c[0].session().can_undo());
    assert!(!c[0].has_unconfirmed());
    smooth(&c);
    same_everywhere(&c).unwrap();
}

#[test]
fn steps_from_before_an_edit_made_again_after_a_clash_stay() {
    let room = Room::new(Document::from_text("one two three\nfour"));
    let mut c = [join(&room, "Ana"), join(&room, "Ben")];
    // Ana types in the second paragraph; the hub has it.
    run(&mut c[0], "caret.set", json!({"pos": Pos::body(1, 0)}));
    run(&mut c[0], "text.insert", json!({"text": "X"}));
    settle(&mut c).unwrap();
    // Then Ana bolds "two" while Ben types "2" over it: her bold is made again on "2".
    run(&mut c[0], "select.text", json!({"text": "two"}));
    run(&mut c[0], "format.bold", json!({}));
    run(&mut c[1], "select.text", json!({"text": "two"}));
    run(&mut c[1], "text.insert", json!({"text": "2"}));
    c[1].send();
    c[0].send();
    let a = &mut c[0];
    assert_eq!((a.stats().clashes, a.stats().made_again, a.stats().lost), (1, 1, 0));
    // Her Undo takes the bold back, then her typing, past Ben's "2".
    assert_eq!(a.session().undo_depth(), 2);
    run(a, "edit.undo", json!({}));
    run(a, "edit.undo", json!({}));
    assert_eq!(text(a), "one 2 three\nfour");
    let first = a.session().doc.body.first().and_then(|b| b.as_para());
    assert!(first.is_some_and(|p| p.runs.iter().all(|r| r.props.bold != Some(true))));
    settle(&mut c).unwrap();
    smooth(&c);
    same_everywhere(&c).unwrap();
}

#[test]
fn an_undo_made_again_after_a_clash_takes_the_same_step_or_none() {
    let room = Room::new(Document::from_text("one\ntwo\nthree"));
    let mut c = [join(&room, "Ana"), join(&room, "Ben")];
    // Ana types in the second and third paragraphs and undoes the third's; the hub has it all.
    run(&mut c[0], "caret.set", json!({"pos": Pos::body(1, 0)}));
    run(&mut c[0], "text.insert", json!({"text": "X"}));
    run(&mut c[0], "caret.set", json!({"pos": Pos::body(2, 0)}));
    run(&mut c[0], "text.insert", json!({"text": "Y"}));
    run(&mut c[0], "edit.undo", json!({}));
    settle(&mut c).unwrap();
    assert_eq!(text(&c[0]), "one\nXtwo\nthree");
    // Ana undoes her "X" while Ben, not having seen it yet, types over it: the hub takes Ben's
    // first, so Ana's Undo comes back. Made again, it would take Ben's text away too: it is
    // refused, naming Ben, and her step stays.
    run(&mut c[0], "edit.undo", json!({}));
    run(&mut c[1], "select.text", json!({"text": "Xtwo"}));
    run(&mut c[1], "text.insert", json!({"text": "TWO"}));
    c[1].send();
    c[0].send();
    let a = &mut c[0];
    assert_eq!((a.stats().clashes, a.stats().lost), (1, 1));
    assert!(a.session().status.contains("Ben has changed it since"), "{}", a.session().status);
    assert_eq!(text(a), "one\nTWO\nthree");
    assert_eq!(a.session().undo_depth(), 1);
    // Her Redo still has her "Y", which it puts back past Ben's change.
    run(a, "edit.redo", json!({}));
    assert_eq!(text(a), "one\nTWO\nYthree");
    settle(&mut c).unwrap();
    same_everywhere(&c).unwrap();
}

#[test]
fn others_changes_while_an_edit_waits_for_the_hub_stay_in_its_steps() {
    let room = Room::new(Document::from_text("one two three\nfour"));
    let mut c = [join(&room, "Ana"), join(&room, "Ben")];
    // Ana types "X" in the second paragraph; the hub has it.
    run(&mut c[0], "caret.set", json!({"pos": Pos::body(1, 0)}));
    run(&mut c[0], "text.insert", json!({"text": "X"}));
    settle(&mut c).unwrap();
    // Ana bolds "two" and hasn't sent it when Ben's bold of her "X" comes in: her bold moves
    // past it. Then Ben types "2" over "two", and the hub takes it before Ana's bold, which is
    // made again on "2".
    run(&mut c[0], "select.text", json!({"text": "two"}));
    run(&mut c[0], "format.bold", json!({}));
    run(&mut c[1], "select.text", json!({"text": "X"}));
    run(&mut c[1], "format.bold", json!({}));
    c[1].send();
    c[0].receive();
    assert!(c[0].has_unconfirmed());
    run(&mut c[1], "select.text", json!({"text": "two"}));
    run(&mut c[1], "text.insert", json!({"text": "2"}));
    c[1].sync();
    c[0].send();
    let a = &mut c[0];
    assert_eq!((a.stats().clashes, a.stats().made_again, a.stats().lost), (1, 1, 0));
    // Undo takes the bold made again back; the "X" Ben bolded meanwhile stays, naming him.
    run(a, "edit.undo", json!({}));
    let why = refused(a, "edit.undo");
    assert!(why.contains("Typing can't be undone: Ben has changed it since"), "{why}");
    assert_eq!(text(a), "one 2 three\nXfour");
    settle(&mut c).unwrap();
    smooth(&c);
    same_everywhere(&c).unwrap();
}

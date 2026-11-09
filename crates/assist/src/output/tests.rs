//! A conversation's record of what it changed, against a real document.

use serde_json::json;
use wordcraft_doc::Document;
use wordcraft_engine::Session;

use super::{OutputKind, OutputRow, OutputState, rows};
use crate::transcript::{Entry, Transcript};

const AUTHOR: &str = "Test Assistant";

fn session() -> Session {
    let mut s = Session::new(Document::from_text("one two three\nfour five six"));
    s.author = AUTHOR.into();
    s.run("review.trackChanges", &json!({"value": true})).unwrap();
    s
}

fn rewrite(s: &mut Session, from: &str, to: &str) {
    s.run("select.text", &json!({"text": from})).unwrap();
    s.run("text.insert", &json!({"text": to})).unwrap();
}

fn note(t: &mut Transcript, s: &Session) {
    let (seen, more) = rows(s, AUTHOR);
    t.note_output(seen, more, |row| super::ended(&s.doc, row));
}

fn listed(t: &Transcript) -> Vec<(OutputKind, String, OutputState)> {
    t.details().turns.iter().flat_map(|turn| turn.output.iter()).map(|r: &OutputRow| (r.kind, r.text.clone(), r.state)).collect()
}

#[test]
fn a_change_shows_in_the_turn_it_first_showed_in() {
    let mut s = session();
    let mut t = Transcript::default();
    t.asked("First");
    rewrite(&mut s, "two", "2");
    note(&mut t, &s);
    t.asked("Second");
    note(&mut t, &s);
    rewrite(&mut s, "five", "5");
    note(&mut t, &s);
    let details = t.details();
    let words = |n: usize| details.turns[n].output.iter().map(|r| r.text.as_str()).collect::<Vec<_>>();
    assert_eq!(words(0), ["2", "two"], "{details:?}");
    assert_eq!(words(1), ["5", "five"], "{details:?}");
}

#[test]
fn a_change_that_grows_as_its_turn_types_stays_one_row() {
    let mut s = session();
    let mut t = Transcript::default();
    t.asked("Type");
    s.run("caret.docEnd", &json!({})).unwrap();
    s.run("text.insert", &json!({"text": " more"})).unwrap();
    note(&mut t, &s);
    s.run("text.insert", &json!({"text": " words"})).unwrap();
    note(&mut t, &s);
    assert_eq!(listed(&t), [(OutputKind::Insert, " more words".to_string(), OutputState::Open)]);
}

#[test]
fn how_a_change_ended_is_read_from_the_text() {
    let run = |id: &str, which: &str| {
        let mut s = session();
        let mut t = Transcript::default();
        t.asked("Rewrite");
        rewrite(&mut s, "two", "2");
        note(&mut t, &s);
        s.run("select.text", &json!({"text": which})).unwrap();
        s.run(id, &json!({})).unwrap();
        note(&mut t, &s);
        listed(&t)
    };
    let deleted = |state| (OutputKind::Delete, "two".to_string(), state);
    let inserted = |state| (OutputKind::Insert, "2".to_string(), state);
    assert_eq!(run("review.accept", "2"), [inserted(OutputState::Accepted), deleted(OutputState::Open)]);
    assert_eq!(run("review.reject", "2"), [inserted(OutputState::Rejected), deleted(OutputState::Open)]);
    assert_eq!(run("review.accept", "two"), [inserted(OutputState::Open), deleted(OutputState::Accepted)]);
    assert_eq!(run("review.reject", "two"), [inserted(OutputState::Open), deleted(OutputState::Rejected)]);
}

#[test]
fn a_change_undone_comes_back_as_the_same_row() {
    let mut s = session();
    let mut t = Transcript::default();
    t.asked("Rewrite");
    s.run("caret.docEnd", &json!({})).unwrap();
    s.run("text.insert", &json!({"text": " end"})).unwrap();
    note(&mut t, &s);
    s.run("select.text", &json!({"text": " end"})).unwrap();
    s.run("review.reject", &json!({})).unwrap();
    note(&mut t, &s);
    assert_eq!(listed(&t), [(OutputKind::Insert, " end".to_string(), OutputState::Rejected)]);
    s.run("edit.undo", &json!({})).unwrap();
    note(&mut t, &s);
    assert_eq!(listed(&t), [(OutputKind::Insert, " end".to_string(), OutputState::Open)]);
}

#[test]
fn someone_else_s_changes_aren_t_listed() {
    let mut s = session();
    let mut t = Transcript::default();
    t.asked("Rewrite");
    s.author = "Someone Else".into();
    rewrite(&mut s, "two", "2");
    note(&mut t, &s);
    assert_eq!(listed(&t), []);
}

#[test]
fn a_comment_shows_how_it_stands_and_when_it_went() {
    let mut s = session();
    let mut t = Transcript::default();
    t.asked("Comment");
    s.run("select.text", &json!({"text": "two"})).unwrap();
    let id = s.run("review.newComment", &json!({"text": "Why two?"})).unwrap()["id"].clone();
    note(&mut t, &s);
    assert_eq!(listed(&t), [(OutputKind::Comment, "Why two?".to_string(), OutputState::Open)]);
    s.run("review.resolveComment", &json!({"id": id})).unwrap();
    note(&mut t, &s);
    assert_eq!(listed(&t)[0].2, OutputState::Resolved);
    s.run("review.deleteComment", &json!({"id": id})).unwrap();
    note(&mut t, &s);
    assert_eq!(listed(&t)[0].2, OutputState::Deleted);
}

#[test]
fn past_the_limit_the_rest_are_counted_and_none_is_taken_for_ended() {
    let mut text = String::new();
    for n in 0..super::MAX_ROWS + 10 {
        text.push_str(&format!("word{n}\n"));
    }
    let mut s = Session::new(Document::from_text(&text));
    s.author = AUTHOR.into();
    s.run("review.trackChanges", &json!({"value": true})).unwrap();
    for n in 0..super::MAX_ROWS + 10 {
        s.run("select.text", &json!({"text": format!("word{n}\n").trim_end()})).unwrap();
        s.run("text.delete", &json!({})).unwrap();
    }
    let mut t = Transcript::default();
    t.asked("Rewrite");
    note(&mut t, &s);
    let details = t.details();
    assert_eq!(details.turns[0].output.len(), super::MAX_ROWS);
    assert_eq!(details.more_output, 10);
    // With rows past the limit, a row missing from the list may only have moved past it: nothing
    // is said to have ended, and the list stays at its limit as others come in.
    for n in 0..5 {
        s.run("select.text", &json!({"text": format!("word{n}")})).unwrap();
        s.run("review.reject", &json!({})).unwrap();
        note(&mut t, &s);
        assert_eq!(t.details().more_output, 9 - n);
        assert!(listed(&t).iter().all(|r| r.2 == OutputState::Open), "{:?}", listed(&t).first());
        assert_eq!(listed(&t).len(), super::MAX_ROWS);
    }
}

#[test]
fn only_details_shows_a_row() {
    let mut s = session();
    let mut t = Transcript::default();
    t.asked("Rewrite");
    t.record(&crate::Event::Text { text: "Rew".into() });
    rewrite(&mut s, "two", "2");
    note(&mut t, &s);
    t.record(&crate::Event::Text { text: "riting".into() });
    assert_eq!(t.last_answer().as_deref(), Some("Rewriting"));
    assert!(t.entries().filter(|e| matches!(e, Entry::Output(_))).all(Entry::details_only));
}

#[test]
fn a_change_of_words_that_repeat_keeps_its_row_and_its_verdict() {
    let mut s = Session::new(Document::from_text("a cat b"));
    s.author = AUTHOR.into();
    s.run("review.trackChanges", &json!({"value": true})).unwrap();
    let mut t = Transcript::default();
    t.asked("Add");
    s.run("caret.set", &json!({"pos": {"story": "body", "path": [0], "off": 0}})).unwrap();
    s.run("text.insert", &json!({"text": "cat "})).unwrap();
    s.run("caret.docEnd", &json!({})).unwrap();
    s.run("text.insert", &json!({"text": " cat "})).unwrap();
    note(&mut t, &s);
    // Reject the first: "cat" is still there, the original and the other insertion.
    s.run("select.text", &json!({"text": "cat ", "occurrence": 1})).unwrap();
    s.run("review.reject", &json!({})).unwrap();
    note(&mut t, &s);
    let states: Vec<_> = listed(&t).into_iter().map(|r| (r.1, r.2)).collect();
    assert_eq!(states, [("cat ".to_string(), OutputState::Resolved), (" cat ".to_string(), OutputState::Open)], "the count can't tell which went");
}

#[test]
fn a_replacement_holding_the_words_it_replaces_says_only_resolved() {
    let mut s = Session::new(Document::from_text("a cat b"));
    s.author = AUTHOR.into();
    s.run("review.trackChanges", &json!({"value": true})).unwrap();
    let mut t = Transcript::default();
    t.asked("Replace");
    rewrite(&mut s, "cat", "catfish");
    note(&mut t, &s);
    s.run("review.rejectAll", &json!({})).unwrap();
    note(&mut t, &s);
    assert!(listed(&t).iter().all(|r| r.2 != OutputState::Accepted), "{:?}", listed(&t));
    assert_eq!(s.doc.plain_text(wordcraft_doc::StoryRef::Body), "a cat b");
}

#[test]
fn accept_all_shifting_a_later_change_doesn_t_change_its_verdict() {
    let mut s = session();
    let mut t = Transcript::default();
    t.asked("Rewrite");
    rewrite(&mut s, "one", "1");
    rewrite(&mut s, "three", "3");
    note(&mut t, &s);
    s.run("review.acceptAll", &json!({})).unwrap();
    note(&mut t, &s);
    let states: Vec<_> = listed(&t).into_iter().map(|r| r.2).collect();
    assert!(states.iter().all(|s| *s == OutputState::Accepted), "{:?}", listed(&t));
}

#[test]
fn a_new_paragraph_mark_moves_a_change_without_a_new_row() {
    let mut s = session();
    let mut t = Transcript::default();
    t.asked("Add");
    s.run("caret.set", &json!({"pos": {"story": "body", "path": [0], "off": 13}})).unwrap();
    s.run("text.insert", &json!({"text": " end"})).unwrap();
    note(&mut t, &s);
    s.run("caret.set", &json!({"pos": {"story": "body", "path": [0], "off": 13}})).unwrap();
    s.author = "Someone Else".into();
    s.run("text.newParagraph", &json!({})).unwrap();
    note(&mut t, &s);
    assert_eq!(listed(&t), [(OutputKind::Insert, " end".to_string(), OutputState::Open)]);
}

#[test]
fn two_formatting_changes_side_by_side_stay_two() {
    let mut s = session();
    let mut t = Transcript::default();
    t.asked("Format");
    s.run("select.text", &json!({"text": "one"})).unwrap();
    s.run("format.bold", &json!({})).unwrap();
    s.run("select.text", &json!({"text": " two"})).unwrap();
    s.run("format.italic", &json!({})).unwrap();
    note(&mut t, &s);
    let words: Vec<String> = listed(&t).into_iter().map(|r| r.1).collect();
    assert_eq!(words, ["one", " two"]);
}

#[test]
fn a_full_transcript_keeps_its_entries_where_they_are() {
    let mut s = session();
    let mut t = Transcript::default();
    t.asked("Rewrite");
    for n in 0..crate::transcript::MAX_ENTRIES {
        t.record(&crate::Event::Error { message: format!("entry {n}") });
    }
    rewrite(&mut s, "two", "2");
    note(&mut t, &s);
    for n in 0..4 {
        t.record(&crate::Event::Error { message: format!("later {n}") });
    }
    let before: Vec<Entry> = t.entries().filter(|e| matches!(e, Entry::Error { .. })).cloned().collect();
    // One row ends and others come in at once, in a transcript that trims as they come in.
    s.run("select.text", &json!({"text": "2"})).unwrap();
    s.run("review.accept", &json!({})).unwrap();
    rewrite(&mut s, "five", "5");
    note(&mut t, &s);
    let after: Vec<Entry> = t.entries().filter(|e| matches!(e, Entry::Error { .. })).cloned().collect();
    assert!(before.ends_with(&after), "no answer or error was overwritten");
    assert!(listed(&t).iter().any(|r| r.1 == "2" && r.2 == OutputState::Accepted), "{:?}", listed(&t));
}

#[test]
fn rows_that_ended_go_before_they_push_the_conversation_out() {
    let mut s = session();
    s.run("caret.docEnd", &json!({})).unwrap();
    let mut t = Transcript::default();
    t.asked("Keep this prompt");
    for n in 0..super::MAX_ROWS + 40 {
        s.run("text.insert", &json!({"text": format!("<{n}>")})).unwrap();
        note(&mut t, &s);
        s.run("review.acceptAll", &json!({})).unwrap();
        note(&mut t, &s);
    }
    let rows = t.entries().filter(|e| matches!(e, Entry::Output(_))).count();
    assert!(rows <= super::MAX_ROWS, "{rows} rows");
    assert!(t.entries().any(|e| matches!(e, Entry::Asked { text } if text == "Keep this prompt")));
}

#[test]
fn a_comment_past_the_limit_isn_t_taken_for_deleted() {
    let mut s = session();
    let mut t = Transcript::default();
    t.asked("Comment");
    for n in 0..super::MAX_COMMENTS {
        s.run("caret.docEnd", &json!({})).unwrap();
        s.run("review.newComment", &json!({"text": format!("c{n}")})).unwrap();
    }
    note(&mut t, &s);
    s.run("caret.docStart", &json!({})).unwrap();
    s.run("review.newComment", &json!({"text": "earlier"})).unwrap();
    note(&mut t, &s);
    assert!(listed(&t).iter().all(|r| r.2 != OutputState::Deleted), "{:?}", listed(&t).last());
    assert_eq!(t.details().more_output, 1);
}

#[test]
fn a_long_comment_is_read_only_as_far_as_its_row_shows() {
    let mut s = session();
    s.run("select.text", &json!({"text": "two"})).unwrap();
    s.run("review.newComment", &json!({"text": "x".repeat(4096)})).unwrap();
    let (rows, _) = rows(&s, AUTHOR);
    assert_eq!(rows[0].text.chars().count(), wordcraft_engine::cmd::review::ROW_TEXT_CHARS);
}

#[test]
fn a_change_whose_paragraph_joined_another_says_only_that_it_was_resolved() {
    let mut s = session();
    let mut t = Transcript::default();
    t.asked("Join");
    s.run("caret.docEnd", &json!({})).unwrap();
    s.run("text.insert", &json!({"text": " extra"})).unwrap();
    let end = |path: usize, off: usize| json!({"story": "body", "path": [path], "off": off});
    s.run("select.range", &json!({"anchor": end(0, 13), "focus": end(1, 0)})).unwrap();
    s.run("text.delete", &json!({})).unwrap();
    note(&mut t, &s);
    s.run("review.acceptAll", &json!({"author": AUTHOR})).unwrap();
    note(&mut t, &s);
    assert_eq!(s.doc.plain_text(wordcraft_doc::StoryRef::Body), "one two threefour five six extra");
    let row = listed(&t).into_iter().find(|r| r.1 == " extra");
    assert_eq!(row.map(|r| r.2), Some(OutputState::Resolved), "{:?}", listed(&t));
}

#[test]
fn a_move_s_row_takes_the_move_and_leaves_the_formatting_of_where_it_went() {
    use wordcraft_doc::{Path, Revision, RevisionKind, StoryRef};
    let mut s = session();
    s.run("document.setText", &json!({"text": "First one.\nSecond one.\nThird one."})).unwrap();
    s.run("review.trackChanges", &json!({"value": true})).unwrap();
    s.run("select.range", &json!({"anchor": {"block": 1, "off": 0}, "focus": {"block": 2, "off": 0}})).unwrap();
    s.run("edit.cut", &json!({})).unwrap();
    s.run("caret.set", &json!({"pos": {"block": 0, "off": 0}})).unwrap();
    s.run("edit.paste", &json!({})).unwrap();
    let rev = u32::try_from(s.doc.revisions.len()).unwrap();
    s.doc.revisions.push(Revision::new(RevisionKind::Format, AUTHOR, ""));
    let change = wordcraft_doc::props::PropChange { rev, old: Default::default() };
    s.doc.para_mut(StoryRef::Body, &Path::top(0)).unwrap().props.fmt_change = Some(Box::new(change));

    let (seen, _) = rows(&s, AUTHOR);
    let row = seen.iter().find(|r| r.kind == OutputKind::Move).expect("the move's row");
    s.run("select.range", &row.range().unwrap()).unwrap();
    s.run("review.accept", &row.resolve_params(AUTHOR)).unwrap();
    let left = rows(&s, AUTHOR).0;
    assert!(left.iter().all(|r| r.kind != OutputKind::Move), "{left:?}");
    assert!(s.doc.para(StoryRef::Body, &Path::top(0)).unwrap().props.fmt_change.is_some(), "the formatting waits still");
}

#[test]
fn a_change_across_paragraphs_says_only_that_it_was_resolved() {
    let mut s = session();
    let mut t = Transcript::default();
    t.asked("Remove");
    s.run("select.range", &json!({"anchor": {"block": 0, "off": 4}, "focus": {"block": 1, "off": 4}})).unwrap();
    s.run("text.delete", &json!({})).unwrap();
    note(&mut t, &s);
    let before = listed(&t);
    assert!(before.iter().any(|r| r.1.contains("two three") && r.1.contains("four")), "{before:?}");
    s.run("review.acceptAll", &json!({})).unwrap();
    note(&mut t, &s);
    assert!(listed(&t).iter().all(|r| r.2 == OutputState::Resolved), "{:?}", listed(&t));
}

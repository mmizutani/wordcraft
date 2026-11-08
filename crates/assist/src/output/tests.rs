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
    // is said to have ended.
    s.run("select.text", &json!({"text": "word0"})).unwrap();
    s.run("review.reject", &json!({})).unwrap();
    note(&mut t, &s);
    assert_eq!(t.details().more_output, 9);
    assert!(listed(&t).iter().all(|r| r.2 == OutputState::Open), "{:?}", listed(&t).first());
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

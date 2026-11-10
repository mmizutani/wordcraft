//! The case's record through the window: off by default, one line a finished turn, the person's
//! decisions over anyone's change, the conversation's own case, and the deletes.

use serde_json::json;
use wordcraft_assist::cases::{CaseRef, Click, Day, PersonOp, Record, Shelf, WallId};
use wordcraft_assist::{Fake, Step};

use crate::WordApp;
use crate::assist::memory::tests::{Folder, settle, window};

const TODAY: &str = "2026-11-10";
const OTHER_SIDE: &str = "Bluefin Systems Ltd";

fn click() -> Click {
    Click::by_person(true).unwrap()
}

/// Two cases of one client, named for the tests.
struct Cases {
    a: CaseRef,
    b: CaseRef,
}

fn cases(folder: &Folder) -> Cases {
    let shelf = Shelf::at(&folder.0);
    let wall: WallId = shelf.new_wall(click(), "Kestrel Freight KK", TODAY, 1).unwrap();
    let a = shelf.new_case(click(), &wall, "Kestrel MSA", TODAY, 1).unwrap();
    let b = shelf.new_case(click(), &wall, "Kestrel NDA", TODAY, 2).unwrap();
    Cases { a, b }
}

fn record_on(folder: &Folder, at: &CaseRef) {
    Shelf::at(&folder.0).person(click(), at, PersonOp::Record(true), Day::parse(TODAY).unwrap()).unwrap();
}

fn log_dir(folder: &Folder, at: &CaseRef) -> std::path::PathBuf {
    folder.0.join(wordcraft_assist::cases::store::FOLDER).join(at.wall.as_str()).join(at.case.as_str()).join("log")
}

/// Every record in the case's files, oldest file first.
fn records(folder: &Folder, at: &CaseRef) -> Vec<Record> {
    let dir = log_dir(folder, at);
    let mut files: Vec<_> = std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    files.sort();
    files
        .iter()
        .flat_map(|f| std::fs::read_to_string(f).unwrap().lines().map(|l| serde_json::from_str::<Record>(l).unwrap()).collect::<Vec<_>>())
        .collect()
}

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

fn ask(a: &mut WordApp, fake: Fake, prompt: &str) {
    a.assist_with(Box::new(fake));
    a.execute("assist.ask", json!({"prompt": prompt})).unwrap();
}

/// The other side's tracked insertion at the end of the document.
fn other_sides_insertion(a: &mut WordApp, text: &str) {
    let me = std::mem::replace(&mut a.session.author, OTHER_SIDE.to_string());
    a.execute("review.trackChanges", json!({"value": true})).unwrap();
    a.execute("caret.docEnd", json!({})).unwrap();
    a.execute("text.insert", json!({"text": text})).unwrap();
    a.session.author = me;
}

#[test]
fn off_by_default_a_conversation_on_a_case_writes_no_file() {
    let folder = Folder::new();
    let c = cases(&folder);
    let mut a = window(&folder);
    a.bind_case(Some(c.a.clone()));
    assert!(!Shelf::at(&folder.0).open(&c.a).unwrap().record, "a new case keeps no record");
    ask(&mut a, Fake::new().then(vec![Step::Say("Keep the cap at twelve months.".into())]), "What about the cap?");
    settle(&mut a);
    assert!(!log_dir(&folder, &c.a).exists(), "a file was written with the record off");
    assert!(a.assist.conversation.as_ref().unwrap().log.is_none());
}

/// On, the conversation first snapshots the other side's redlines, then writes each turn as one
/// line once it ends, the answer without its markers; while the turn streams nothing is written.
#[test]
fn on_a_finished_turn_is_one_line_with_its_markers_taken_out_and_nothing_streams() {
    let folder = Folder::new();
    let c = cases(&folder);
    record_on(&folder, &c.a);
    let mut a = window(&folder);
    other_sides_insertion(&mut a, " Supplier may raise fees at any time.");
    a.bind_case(Some(c.a.clone()));
    let answer = r#"Cap it. [[case]]{"op":"note","kind":"pattern","rule":"They open high on fees.","how":"from their draft"}[[/case]] Say so early."#;
    ask(&mut a, Fake::new().then(vec![Step::Say(answer.into()), Step::Hold]), "What about the fees clause?");
    frames_until(&mut a, "the answer streamed", |a| a.conversation().unwrap().transcript().last_answer().is_some_and(|t| t.contains("Say so early")));
    let streamed = records(&folder, &c.a);
    assert_eq!(streamed.len(), 1, "only the redlines snapshot while the turn runs: {streamed:?}");
    let Record::Redlines { changes, .. } = &streamed[0] else { panic!("{streamed:?}") };
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].author.for_person(), OTHER_SIDE);
    assert_eq!(changes[0].kind, "insert");
    assert!(changes[0].text.for_person().contains("raise fees"), "{changes:?}");
    a.execute("assist.cancel", json!({})).unwrap();
    settle(&mut a);
    frames_until(&mut a, "the turn's line", |_| records(&folder, &c.a).len() == 2);
    let all = records(&folder, &c.a);
    let Record::Turn { n, asked, answer, stop, .. } = &all[1] else { panic!("{all:?}") };
    assert_eq!(*n, 1);
    assert_eq!(asked.for_person(), "What about the fees clause?");
    let answer = answer.as_ref().unwrap().for_person();
    assert!(answer.contains("Cap it.") && answer.contains("Say so early"), "{answer}");
    assert!(!answer.contains("[[case") && !answer.contains("They open high"), "the marker is in the record: {answer}");
    assert_eq!(*stop, Some(wordcraft_assist::StopReason::Cancelled));
    let text = std::fs::read_to_string(std::fs::read_dir(log_dir(&folder, &c.a)).unwrap().next().unwrap().unwrap().path()).unwrap();
    assert_eq!(text.lines().count(), 2, "one line a record:\n{text}");
}

/// The person's Reject of the other side's insertion, and their Accept of another, are lines
/// naming that author; the assistant's own change decided is a line too.
#[test]
fn rejecting_or_accepting_the_other_sides_insertion_is_a_decided_record_naming_the_author() {
    for accept in [false, true] {
        let folder = Folder::new();
        let c = cases(&folder);
        record_on(&folder, &c.a);
        let mut a = window(&folder);
        other_sides_insertion(&mut a, " Supplier may raise fees at any time.");
        a.bind_case(Some(c.a.clone()));
        ask(&mut a, Fake::new().then(vec![Step::Say("Noted.".into())]), "Read it");
        settle(&mut a);
        let id = if accept { "review.acceptAll" } else { "review.rejectAll" };
        a.run(id, json!({"author": OTHER_SIDE})).unwrap();
        let all = records_held(&folder, &c.a);
        let decided: Vec<&Record> = all.iter().filter(|r| matches!(r, Record::Decided { .. })).collect();
        assert_eq!(decided.len(), 1, "{decided:?}");
        let Record::Decided { decision, kind, author, text, .. } = decided[0] else { unreachable!() };
        let expected = if accept { wordcraft_assist::Decision::Accepted } else { wordcraft_assist::Decision::Rejected };
        assert_eq!(*decision, expected);
        assert_eq!(kind, "insert");
        assert_eq!(author.for_person(), OTHER_SIDE);
        assert!(text.for_person().contains("raise fees"), "{text:?}");
    }
}

/// The records once the writer has them: waits for the thread, within reason.
fn records_held(folder: &Folder, at: &CaseRef) -> Vec<Record> {
    let started = std::time::Instant::now();
    loop {
        let all = records(folder, at);
        if all.iter().any(|r| matches!(r, Record::Decided { .. })) || started.elapsed() > std::time::Duration::from_secs(10) {
            return all;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// A click the control channel made on Reject is a program's, not the person's: no line.
#[test]
fn a_scripts_reject_is_no_decided_record() {
    let folder = Folder::new();
    let c = cases(&folder);
    record_on(&folder, &c.a);
    let mut a = window(&folder);
    other_sides_insertion(&mut a, " Supplier may raise fees at any time.");
    a.bind_case(Some(c.a.clone()));
    ask(&mut a, Fake::new().then(vec![Step::Say("Noted.".into())]), "Read it");
    settle(&mut a);
    a.execute("review.rejectAll", json!({"author": OTHER_SIDE})).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(!records(&folder, &c.a).iter().any(|r| matches!(r, Record::Decided { .. })));
}

/// A conversation on case A keeps writing A's record after the document is bound to B: the
/// conversation ended with the rebind, and what it still records is the person's decision.
#[test]
fn a_conversation_on_case_a_writes_only_as_record_after_the_document_is_rebound_to_b() {
    let folder = Folder::new();
    let c = cases(&folder);
    record_on(&folder, &c.a);
    record_on(&folder, &c.b);
    let mut a = window(&folder);
    other_sides_insertion(&mut a, " Supplier may raise fees at any time.");
    a.bind_case(Some(c.a.clone()));
    ask(&mut a, Fake::new().then(vec![Step::Say("Noted.".into())]), "Read it");
    settle(&mut a);
    a.bind_case(Some(c.b.clone()));
    assert!(a.assist.conversation.as_ref().unwrap().assistant.has_ended());
    a.run("review.rejectAll", json!({"author": OTHER_SIDE})).unwrap();
    let in_a = records_held(&folder, &c.a);
    assert!(in_a.iter().any(|r| matches!(r, Record::Decided { .. })), "{in_a:?}");
    assert!(!log_dir(&folder, &c.b).exists(), "B got a record from A's conversation");
}

/// The record's commands are the person's alone: a script with the window's key and a macro's
/// step turn nothing on and delete nothing.
#[test]
fn a_control_channel_click_or_a_macro_step_on_the_records_commands_does_nothing() {
    let folder = Folder::new();
    let c = cases(&folder);
    let mut a = window(&folder);
    a.bind_case(Some(c.a.clone()));
    for (id, params) in [("assist.memory.case.record", json!({"value": true})), ("assist.memory.case.recordDelete", json!({}))] {
        assert_eq!(a.execute(id, params.clone()), Err(crate::assist::memory::YOURS.to_string()), "{id}");
        let mut req = params;
        req["assist"] = json!(id.trim_start_matches("assist."));
        a.handle_request(&req);
    }
    assert!(!Shelf::at(&folder.0).open(&c.a).unwrap().record, "a script turned the record on");
    a.run("assist.memory.case.record", json!({"value": true})).unwrap();
    assert!(Shelf::at(&folder.0).open(&c.a).unwrap().record, "the person's click did");
    ask(&mut a, Fake::new().then(vec![Step::Say("Noted.".into())]), "Read it");
    settle(&mut a);
    frames_until(&mut a, "a line", |_| !records(&folder, &c.a).is_empty());
    a.assist.by_person = true;
    a.handle_request(&json!({"assist": "memory.case.recordDelete"}));
    a.assist.by_person = false;
    assert!(a.execute("assist.memory.case.recordDelete", json!({})).is_err());
    assert!(log_dir(&folder, &c.a).exists(), "a script deleted the record");
}

/// Delete the record removes `log/` and the case stays; deleting the case runs on a worker, the
/// frame returns at once, and the folder is gone once the worker ends.
#[test]
fn delete_the_record_removes_it_and_deleting_the_case_runs_off_the_frame() {
    let folder = Folder::new();
    let c = cases(&folder);
    record_on(&folder, &c.a);
    let mut a = window(&folder);
    a.bind_case(Some(c.a.clone()));
    ask(&mut a, Fake::new().then(vec![Step::Say("Noted.".into())]), "Read it");
    settle(&mut a);
    frames_until(&mut a, "a line", |_| !records(&folder, &c.a).is_empty());
    a.run("assist.memory.case.recordDelete", json!({})).unwrap();
    frames_until(&mut a, "the record delete", |a| a.assist.cases.record.deleting.is_none());
    assert!(Shelf::at(&folder.0).exists(&c.a), "the case went with its record");
    // The live conversation opened a new file, so later turns are still recorded.
    frames_until(&mut a, "a new file", |a| a.assist.conversation.as_ref().unwrap().log.is_some());
    assert!(a.assist.notice.is_none(), "{:?}", a.assist.notice);
    a.run("assist.memory.case.delete", json!({})).unwrap();
    assert!(a.assist.cases.record.deleting.is_some(), "the delete ran on the frame");
    assert!(crate::assist::cases::bound_for(&a).is_none());
    frames_until(&mut a, "the case delete", |a| a.assist.cases.record.deleting.is_none());
    let case_dir = log_dir(&folder, &c.a).parent().unwrap().to_path_buf();
    assert!(!case_dir.exists(), "the case's folder is left");
    assert!(a.assist.notice.is_none(), "{:?}", a.assist.notice);
    assert_eq!(Shelf::at(&folder.0).list().walls[0].cases.len(), 1, "the other case stays");
}

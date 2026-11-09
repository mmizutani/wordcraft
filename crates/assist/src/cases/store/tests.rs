//! The shelf: two writers, a delete racing a write, a crash mid-delete, hostile ids, the file at
//! every limit, a damaged file, and what a model can't do.

use std::path::PathBuf;

use super::{CaseId, CaseRef, DAMAGED_SUFFIX, MAX_FILE, Shelf, WallId};
use crate::cases::record::{
    Applied, Body, CaseError, Click, Day, Factor, FactorKind, Facts, MAX_BALANCES, MAX_DISMISSED, MAX_FACTORS, MAX_LIST, MAX_NOTES, MAX_SHORT,
    MAX_TEXT, MAX_UNCONFIRMED, ModelWrite, NoteDraft, NoteId, PersonOp, Plan, Quoted, Side, Stance, Standing, Topics,
};
use crate::cases::record::{BalanceDraft, Case};

const TODAY: &str = "2026-11-10";

fn today() -> Day {
    Day::parse(TODAY).unwrap()
}

fn click() -> Click {
    Click::by_person(true).unwrap()
}

/// A settings folder of its own.
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wordcraft-cases-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn shelf_with_case(name: &str) -> (Shelf, CaseRef) {
    let shelf = Shelf::at(&folder(name));
    let wall = shelf.new_wall(click(), "Acme", TODAY, 1).unwrap();
    let at = shelf.new_case(click(), &wall, "Acme MSA", TODAY, 2).unwrap();
    (shelf, at)
}

fn pitfall(text: &str) -> NoteDraft {
    NoteDraft { body: Body::Pitfall { trap: Quoted::parse(text), guard: None }, how: None, topics: Topics::default() }
}

fn plan(goal: &str) -> Plan {
    Plan { goal: Quoted::parse(goal), next: Vec::new(), standing: Standing::Unconfirmed }
}

#[test]
fn two_writers_keep_both_notes() {
    let (shelf, at) = shelf_with_case("writers");
    std::thread::scope(|s| {
        for w in 0..2 {
            let (shelf, at) = (shelf.clone(), at.clone());
            s.spawn(move || {
                for n in 0..20 {
                    let mut tries = 0;
                    while let Err(e) = shelf.person(click(), &at, PersonOp::AddNote(pitfall(&format!("writer {w} trap {n}"))), today()) {
                        assert!(e == CaseError::Busy && tries < 200, "{e}");
                        tries += 1;
                    }
                }
            });
        }
    });
    let case = shelf.open(&at).unwrap();
    assert_eq!(case.notes.len(), 40);
    let mut ids: Vec<NoteId> = case.notes.iter().map(|n| n.id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 40, "each with an id of its own");
}

#[test]
fn a_deleted_case_does_not_come_back() {
    let (shelf, at) = shelf_with_case("deleted");
    let other = shelf.clone();
    shelf.delete_case(click(), &at).unwrap();
    assert_eq!(other.person(click(), &at, PersonOp::AddNote(pitfall("late")), today()), Err(CaseError::Gone));
    assert_eq!(other.model(&at, ModelWrite::Note(pitfall("later")), today()), Err(CaseError::Gone));
    assert!(!shelf.exists(&at));
    assert_eq!(shelf.open(&at), Err(CaseError::Gone));
    let wall_dir = shelf.root.join(at.wall.as_str());
    let left: Vec<String> = std::fs::read_dir(&wall_dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(left, vec!["wall.json"], "no folder and no lock reappear");
    assert_eq!(shelf.list().walls[0].cases, vec![]);
}

#[test]
fn a_crash_after_the_rename_is_swept() {
    let (shelf, at) = shelf_with_case("crash");
    let trash = shelf.root.join(format!(".trash-{}", at.case.as_str()));
    std::fs::rename(shelf.case_dir(&at), &trash).unwrap();
    assert!(trash.is_dir());
    let listing = shelf.list();
    assert!(!trash.exists(), "the sweep finished the delete");
    assert_eq!(listing.walls.len(), 1);
    assert!(listing.walls[0].cases.is_empty());
}

#[test]
fn a_case_id_cannot_leave_the_folder() {
    for bad in [
        "../../etc/passwd",
        "0123456789abcde/",
        "/0123456789abcdef",
        "0123456789ABCDEF",
        "0123456789abcdeg",
        "",
        "0123456789abcdef0",
        ".trash-0123456789",
    ] {
        assert!(WallId::parse(bad).is_none(), "{bad:?}");
        assert!(CaseId::parse(bad).is_none(), "{bad:?}");
        assert!(serde_json::from_str::<CaseId>(&serde_json::to_string(bad).unwrap()).is_err(), "{bad:?}");
    }
    assert!(CaseId::parse("0123456789abcdef").is_some());
}

/// A case holding the most of everything, in three-byte characters.
fn full_case() -> Case {
    let long = |n: usize| Quoted::parse(&"責".repeat(n));
    let mut case = Case::new(&"題".repeat(MAX_TEXT));
    case.facts = Facts { parties: (0..MAX_LIST).map(|_| long(MAX_SHORT)).collect(), ..Facts::default() };
    for i in 0..MAX_BALANCES {
        let factors = (0..MAX_FACTORS).map(|_| Factor { kind: FactorKind::Deadline, favors: Side::Them, why: Some(long(MAX_SHORT)) }).collect();
        case.apply_person(PersonOp::Balance(BalanceDraft { stance: Stance::Weaker, factors }), today()).unwrap();
        assert_eq!(case.balance.len(), i + 1);
    }
    case.apply_person(
        PersonOp::Plan(Some(Plan { goal: long(MAX_TEXT), next: (0..MAX_LIST).map(|_| long(MAX_TEXT)).collect(), standing: Standing::Unconfirmed })),
        today(),
    )
    .unwrap();
    for i in 0..MAX_DISMISSED {
        case.apply_person(PersonOp::AddNote(pitfall(&format!("gone {i}"))), today()).unwrap();
        let id = case.notes.last().unwrap().id;
        case.apply_person(PersonOp::RemoveNote(id), today()).unwrap();
    }
    for i in 0..MAX_NOTES {
        let draft = NoteDraft {
            body: Body::Position {
                topic: crate::cases::Topic::Liability,
                moved: Side::Them,
                terms: Quoted::parse(&format!("{i:04}{}", "責".repeat(MAX_TEXT - 4))),
                traded_for: Some(long(MAX_SHORT)),
                landed: true,
            },
            how: Some(long(MAX_SHORT)),
            topics: crate::cases::Topic::ALL.into_iter().collect(),
        };
        case.apply_person(PersonOp::AddNote(draft), today()).unwrap();
    }
    case
}

#[test]
fn a_full_case_fits_the_file() {
    let case = full_case();
    assert_eq!(case.notes.len(), MAX_NOTES);
    assert_eq!(case.dismissed().len(), MAX_DISMISSED);
    let bytes = case.to_bytes().unwrap();
    assert!(bytes.len() as u64 <= MAX_FILE, "{} bytes", bytes.len());
    let back = Case::read(&bytes);
    assert!(!back.damaged);
    assert_eq!(back, case);
}

#[test]
fn a_damaged_case_moves_aside() {
    let (shelf, at) = shelf_with_case("damaged");
    let path = shelf.case_dir(&at).join("case.json");
    let aside = shelf.case_dir(&at).join(format!("case.json{DAMAGED_SUFFIX}"));
    let mut full = full_case();
    full.notes.push(full.notes[0].clone());
    full.notes.last_mut().unwrap().id = NoteId::parse("n99999").unwrap();
    let over_limit = full.to_bytes().unwrap();
    let unknown_version = br#"{"version": 2, "title": "From a newer build", "notes": []}"#.to_vec();
    for (what, bytes) in [("junk", b"{not json".to_vec()), ("an unknown version", unknown_version), ("a file over the limits", over_limit)] {
        std::fs::write(&path, &bytes).unwrap();
        let _ = std::fs::remove_file(&aside);
        let read = shelf.open(&at).unwrap();
        assert!(read.damaged, "{what} reads as damaged");
        assert_eq!(
            shelf.model(&at, ModelWrite::Note(pitfall("from a model")), today()),
            Err(CaseError::Damaged),
            "{what}: a model's write leaves it"
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "{what}: untouched by the model");
        if what == "an unknown version" {
            assert_eq!(read.title.for_person(), "From a newer build", "what could be read was read");
        }
        let written = shelf.person(click(), &at, PersonOp::Close(crate::cases::Stage::Signed), today()).unwrap();
        assert!(!written.damaged);
        assert_eq!(std::fs::read(&aside).unwrap(), bytes, "{what} is moved aside, not replaced in silence");
        assert_eq!(shelf.open(&at).unwrap().facts.stage, crate::cases::Stage::Signed);
    }
}

#[test]
fn a_model_write_never_confirms_or_replaces_the_person_s_plan() {
    let (shelf, at) = shelf_with_case("model");
    shelf.person(click(), &at, PersonOp::Plan(Some(plan("Sign by March"))), today()).unwrap();
    assert_eq!(shelf.model(&at, ModelWrite::Plan(plan("Walk away")), today()), Ok(Applied::Card(ModelWrite::Plan(plan("Walk away")))));
    let case = shelf.open(&at).unwrap();
    assert_eq!(case.plan.as_ref().unwrap().goal.for_person(), "Sign by March");
    assert!(matches!(case.plan.as_ref().unwrap().standing, Standing::Confirmed { .. }));

    let added = shelf.model(&at, ModelWrite::Note(pitfall("A footer clause overrides the body.")), today()).unwrap();
    let Applied::Added(id) = added else { panic!("{added:?}") };
    shelf.model(&at, ModelWrite::Balance(BalanceDraft { stance: Stance::Weaker, factors: Vec::new() }), today()).unwrap();
    shelf.model(&at, ModelWrite::Used(vec![id]), today()).unwrap();
    let case = shelf.open(&at).unwrap();
    let note = case.notes.iter().find(|n| n.id == id).unwrap();
    assert_eq!(note.standing, Standing::Unconfirmed);
    assert_eq!((note.used, note.last_used), (1, Some(today())));
    assert_eq!(case.balance.last().unwrap().standing, Standing::Unconfirmed);
    assert_eq!(
        shelf.model(&at, ModelWrite::Note(pitfall("a footer clause overrides the body.")), today()),
        Ok(Applied::Known),
        "same text, other case"
    );

    shelf.person(click(), &at, PersonOp::Confirm(id), today()).unwrap();
    shelf.person(click(), &at, PersonOp::RemoveNote(id), today()).unwrap();
    assert_eq!(shelf.model(&at, ModelWrite::Note(pitfall("A footer clause overrides the body.")), today()), Ok(Applied::Known), "dismissed");
    assert_eq!(shelf.open(&at).unwrap().dismissed().len(), 1);

    // Over a model's own plan it does replace.
    shelf.person(click(), &at, PersonOp::Plan(None), today()).unwrap();
    assert_eq!(shelf.model(&at, ModelWrite::Plan(plan("First")), today()), Ok(Applied::Changed));
    assert_eq!(shelf.model(&at, ModelWrite::Plan(plan("Second")), today()), Ok(Applied::Changed));
    assert_eq!(shelf.open(&at).unwrap().plan.unwrap().goal.for_person(), "Second");
}

#[test]
fn the_sixty_fifth_unconfirmed_note_replaces_the_oldest_unused() {
    let (shelf, at) = shelf_with_case("cap");
    shelf.person(click(), &at, PersonOp::AddNote(pitfall("the person's")), today()).unwrap();
    let mut ids = Vec::new();
    for n in 0..MAX_UNCONFIRMED {
        let day = Day::parse(&format!("2026-01-{:02}", n % 28 + 1)).unwrap();
        match shelf.model(&at, ModelWrite::Note(pitfall(&format!("trap {n}"))), day).unwrap() {
            Applied::Added(id) => ids.push(id),
            other => panic!("{other:?}"),
        }
    }
    // The oldest is n0 (January 1); it was used, so the oldest never used is n28 (also January 1).
    shelf.model(&at, ModelWrite::Used(vec![ids[0]]), today()).unwrap();
    let applied = shelf.model(&at, ModelWrite::Note(pitfall("trap 64")), today()).unwrap();
    let Applied::Replaced { added, evicted } = applied else { panic!("{applied:?}") };
    assert_eq!(evicted, ids[28]);
    let case = shelf.open(&at).unwrap();
    assert_eq!(case.notes.len(), MAX_UNCONFIRMED + 1);
    assert!(case.notes.iter().any(|n| n.id == added));
    assert!(case.notes.iter().any(|n| n.id == ids[0]), "the used one stays");
    assert!(case.notes.iter().any(|n| matches!(n.standing, Standing::Confirmed { .. })), "the person's note is never evicted");
    assert!(added > *ids.last().unwrap(), "an id is never reused");
}

#[test]
fn a_model_balance_never_evicts_a_confirmed_one() {
    let (shelf, at) = shelf_with_case("balances");
    let draft = |stance| BalanceDraft { stance, factors: Vec::new() };
    for _ in 0..MAX_BALANCES {
        shelf.person(click(), &at, PersonOp::Balance(draft(Stance::Stronger)), today()).unwrap();
    }
    // Every slot is the person's: a model's write changes nothing and comes back as a card.
    assert_eq!(shelf.model(&at, ModelWrite::Balance(draft(Stance::Weaker)), today()), Ok(Applied::Card(ModelWrite::Balance(draft(Stance::Weaker)))));
    let case = shelf.open(&at).unwrap();
    assert_eq!(case.balance.len(), MAX_BALANCES);
    assert!(case.balance.iter().all(|b| matches!(b.standing, Standing::Confirmed { .. }) && b.stance == Stance::Stronger));

    // With one unconfirmed slot, a model's writes only ever replace that one.
    let mut case = shelf.open(&at).unwrap();
    case.balance.remove(0);
    case.apply_model(ModelWrite::Balance(draft(Stance::Even)), today()).unwrap();
    for _ in 0..3 {
        assert_eq!(case.apply_model(ModelWrite::Balance(draft(Stance::Weaker)), today()), Ok(Applied::Changed));
    }
    assert_eq!(case.balance.len(), MAX_BALANCES);
    assert_eq!(case.balance.iter().filter(|b| matches!(b.standing, Standing::Confirmed { .. })).count(), MAX_BALANCES - 1);
    assert_eq!(case.balance.iter().filter(|b| b.standing == Standing::Unconfirmed).count(), 1);
    // The person's own write past the cap still drops the oldest.
    case.apply_person(PersonOp::Balance(draft(Stance::Even)), today()).unwrap();
    assert_eq!(case.balance.len(), MAX_BALANCES);
}

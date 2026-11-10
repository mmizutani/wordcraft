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
    shelf.delete_case(click(), &at, false).unwrap();
    assert_eq!(other.person(click(), &at, PersonOp::AddNote(pitfall("late")), today()), Err(CaseError::Gone));
    assert_eq!(other.model(&at, ModelWrite::Note(pitfall("later")), today()), Err(CaseError::Gone));
    assert!(!shelf.exists(&at));
    assert_eq!(shelf.open(&at), Err(CaseError::Gone));
    let wall_dir = shelf.root.join(at.wall.as_str());
    let left: Vec<String> = std::fs::read_dir(&wall_dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(left, vec!["wall.json"], "no folder and no lock reappear");
    assert_eq!(shelf.list().walls[0].cases, vec![]);
}

/// Delete client in one window while another window adds a case: the client's folder goes only
/// when it holds no case.
#[test]
fn a_client_with_a_case_is_never_deleted_and_an_empty_one_is() {
    let (shelf, at) = shelf_with_case("delete-wall");
    assert_eq!(shelf.delete_empty_wall(click(), &at.wall), Err(CaseError::NotEmpty));
    assert!(shelf.exists(&at), "the case is still there");
    assert_eq!(shelf.list().walls.len(), 1);
    shelf.delete_case(click(), &at, false).unwrap();
    shelf.delete_empty_wall(click(), &at.wall).unwrap();
    assert!(!shelf.wall_dir(&at.wall).exists());
    assert!(shelf.list().walls.is_empty());
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
    let paragraphs: Vec<String> = (0..300).map(|i| format!("Clause {i}. The supplier delivers lot {i} to the named warehouse by noon.")).collect();
    let doc = wordcraft_doc::Document::from_text(&paragraphs.join("\n"));
    for _ in 0..crate::cases::MAX_DRAFTS {
        let print = crate::cases::DraftPrint::of(&doc, &"題".repeat(MAX_TEXT), today());
        assert_eq!(print.len(), crate::cases::MAX_HASHES);
        case.apply_person(PersonOp::AddDraft(print), today()).unwrap();
    }
    assert_eq!(case.drafts.len(), crate::cases::MAX_DRAFTS);
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

/// A note in four-byte characters, numbered so no two say the same thing.
fn wide_note(n: usize) -> NoteDraft {
    let wide = |n: usize| Quoted::parse(&"😀".repeat(n));
    NoteDraft {
        body: Body::Position {
            topic: crate::cases::Topic::Liability,
            moved: Side::Them,
            terms: Quoted::parse(&format!("{n:04}{}", "😀".repeat(MAX_TEXT - 4))),
            traded_for: Some(wide(MAX_SHORT)),
            landed: true,
        },
        how: Some(wide(MAX_SHORT)),
        topics: crate::cases::Topic::ALL.into_iter().collect(),
    }
}

#[test]
fn a_write_past_the_file_limit_is_refused_and_the_file_kept() {
    let (shelf, at) = shelf_with_case("file-limit");
    let path = shelf.case_dir(&at).join("case.json");
    let mut case = Case::new(&"😀".repeat(MAX_TEXT));
    let wide = |n: usize| Quoted::parse(&"😀".repeat(n));
    case.facts = Facts { parties: (0..MAX_LIST).map(|_| wide(MAX_SHORT)).collect(), ..Facts::default() };
    for _ in 0..MAX_BALANCES {
        let factors = (0..MAX_FACTORS).map(|_| Factor { kind: FactorKind::Deadline, favors: Side::Them, why: Some(wide(MAX_SHORT)) }).collect();
        case.apply_person(PersonOp::Balance(BalanceDraft { stance: Stance::Weaker, factors }), today()).unwrap();
    }
    case.apply_person(
        PersonOp::Plan(Some(Plan { goal: wide(MAX_TEXT), next: (0..MAX_LIST).map(|_| wide(MAX_TEXT)).collect(), standing: Standing::Unconfirmed })),
        today(),
    )
    .unwrap();
    let mut n = 0;
    let next = loop {
        let mut more = case.clone();
        more.apply_person(PersonOp::AddNote(wide_note(n)), today()).unwrap();
        if more.to_bytes().unwrap().len() as u64 > MAX_FILE {
            break wide_note(n);
        }
        case = more;
        n += 1;
    };
    let bytes = case.to_bytes().unwrap();
    assert!(bytes.len() as u64 <= MAX_FILE && case.notes.len() < MAX_NOTES, "{} bytes, {} notes", bytes.len(), case.notes.len());
    std::fs::write(&path, &bytes).unwrap();
    assert!(!shelf.open(&at).unwrap().damaged);

    assert_eq!(shelf.person(click(), &at, PersonOp::AddNote(next.clone()), today()), Err(CaseError::Full("file")));
    assert_eq!(shelf.model(&at, ModelWrite::Note(next), today()), Err(CaseError::Full("file")));
    assert_eq!(std::fs::read(&path).unwrap(), bytes, "the file is as it was");
    assert_eq!(shelf.open(&at).unwrap(), case, "and still reads whole");
}

#[test]
fn a_case_is_never_made_on_a_deleted_wall() {
    let shelf = Shelf::at(&folder("gone-wall"));
    let wall = shelf.new_wall(click(), "Acme", TODAY, 1).unwrap();
    let dir = shelf.wall_dir(&wall);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(shelf.new_case(click(), &wall, "Late", TODAY, 2), Err(CaseError::Gone));
    assert!(!dir.exists(), "the wall is not recreated");
}

#[cfg(unix)]
#[test]
fn a_symlinked_wall_is_skipped_and_its_target_left_alone() {
    let settings = folder("symlink-wall");
    let shelf = Shelf::at(&settings);
    shelf.new_wall(click(), "Real", TODAY, 1).unwrap();
    let target = folder("symlink-wall-target");
    let other = Shelf::at(&target);
    let wall = other.new_wall(click(), "Elsewhere", TODAY, 1).unwrap();
    let trash = other.wall_dir(&wall).join(".trash-x");
    std::fs::create_dir(&trash).unwrap();
    std::os::unix::fs::symlink(other.wall_dir(&wall), shelf.root.join("00000000000000aa")).unwrap();
    let listing = shelf.list();
    assert_eq!(listing.walls.len(), 1);
    assert_eq!(listing.walls[0].name.for_person(), "Real");
    assert_eq!(listing.skipped, 1, "the link is skipped, not followed");
    assert!(trash.is_dir(), "nothing behind the link is swept");
}

#[cfg(unix)]
#[test]
fn a_case_behind_a_symlinked_wall_is_gone() {
    let shelf = Shelf::at(&folder("symlink-case"));
    shelf.new_wall(click(), "Real", TODAY, 9).unwrap();
    let (other, at) = shelf_with_case("symlink-case-target");
    std::os::unix::fs::symlink(other.wall_dir(&at.wall), shelf.wall_dir(&at.wall)).unwrap();
    let path = other.case_dir(&at).join("case.json");
    let before = std::fs::read(&path).unwrap();
    assert_eq!(shelf.model(&at, ModelWrite::Note(pitfall("through a link")), today()), Err(CaseError::Gone));
    assert_eq!(shelf.person(click(), &at, PersonOp::AddNote(pitfall("through a link")), today()), Err(CaseError::Gone));
    assert_eq!(shelf.open(&at), Err(CaseError::Gone));
    assert_eq!(shelf.delete_case(click(), &at, false), Err(CaseError::Gone));
    assert_eq!(shelf.delete_empty_wall(click(), &at.wall), Err(CaseError::Gone));
    assert_eq!(std::fs::read(&path).unwrap(), before, "the target is untouched");
    assert!(other.exists(&at));
}

#[test]
fn setting_a_damaged_file_aside_never_leaves_the_case_without_one() {
    let (shelf, at) = shelf_with_case("aside");
    let dir = shelf.case_dir(&at);
    let path = dir.join("case.json");
    std::fs::write(&path, b"{not json").unwrap();
    std::fs::create_dir(dir.join("case.json.tmp")).unwrap();
    let r = shelf.person(click(), &at, PersonOp::Close(crate::cases::Stage::Signed), today());
    assert!(matches!(r, Err(CaseError::Io(_))), "{r:?}");
    assert!(path.is_file(), "case.json is still there");
    assert_ne!(shelf.open(&at), Err(CaseError::Gone));
    assert!(shelf.open(&at).unwrap().damaged);
}

#[test]
fn a_field_this_build_does_not_know_is_damage() {
    let (shelf, at) = shelf_with_case("unknown-field");
    let path = shelf.case_dir(&at).join("case.json");
    let aside = shelf.case_dir(&at).join(format!("case.json{DAMAGED_SUFFIX}"));
    let mut case = Case::new("Acme MSA");
    case.apply_person(PersonOp::AddNote(pitfall("a trap")), today()).unwrap();
    let mut v: serde_json::Value = serde_json::from_slice(&case.to_bytes().unwrap()).unwrap();
    let mut top = v.clone();
    top["mood"] = serde_json::json!("tense");
    v["notes"][0]["colour"] = serde_json::json!("red");
    for (what, file) in [("at the top", top), ("in a note", v)] {
        let bytes = serde_json::to_vec(&file).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let _ = std::fs::remove_file(&aside);
        assert!(shelf.open(&at).unwrap().damaged, "a field {what} reads as damaged");
        assert_eq!(shelf.model(&at, ModelWrite::Note(pitfall("from a model")), today()), Err(CaseError::Damaged), "{what}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "{what}: untouched by the model");
        shelf.person(click(), &at, PersonOp::Close(crate::cases::Stage::Signed), today()).unwrap();
        assert_eq!(std::fs::read(&aside).unwrap(), bytes, "{what}: the original is kept aside");
    }
}

#[test]
fn note_ids_that_run_out_stop_the_write() {
    let (shelf, at) = shelf_with_case("ids");
    let path = shelf.case_dir(&at).join("case.json");
    let max = 1u64 << 53;
    let mut v: serde_json::Value = serde_json::from_slice(&Case::new("Acme MSA").to_bytes().unwrap()).unwrap();
    v["lastNote"] = serde_json::json!(max);
    let bytes = serde_json::to_vec(&v).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    assert!(!shelf.open(&at).unwrap().damaged, "the maximum itself is a valid last id");
    assert_eq!(shelf.model(&at, ModelWrite::Note(pitfall("one more")), today()), Err(CaseError::Full("note ids")));
    assert_eq!(shelf.person(click(), &at, PersonOp::AddNote(pitfall("one more")), today()), Err(CaseError::Full("note ids")));
    assert_eq!(std::fs::read(&path).unwrap(), bytes, "nothing was written");

    v["lastNote"] = serde_json::json!(max + 1);
    std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    assert!(shelf.open(&at).unwrap().damaged, "past the maximum is damage");
}

fn lesson_draft(text: &str, from: Option<&CaseRef>) -> crate::cases::LessonDraft {
    crate::cases::LessonDraft {
        text: Quoted::parse(text),
        applies: Default::default(),
        topics: Topics::default(),
        from: from.map(|at| at.case.clone()),
    }
}

/// Deleting a case with "Delete these too" removes the lessons kept from it; without, they stay
/// and lose the link, so a later delete of another case never reaches them.
#[test]
fn a_deleted_case_s_lessons_go_with_it_or_stay_unlinked() {
    use crate::cases::LessonOp;
    let (shelf, at) = shelf_with_case("lessons-delete");
    let other = shelf.new_case(click(), &at.wall, "Acme SaaS", TODAY, 3).unwrap();
    shelf.lesson(click(), LessonOp::Keep(lesson_draft("From the MSA.", Some(&at)))).unwrap();
    shelf.lesson(click(), LessonOp::Keep(lesson_draft("From the SaaS deal.", Some(&other)))).unwrap();
    shelf.lesson(click(), LessonOp::Keep(lesson_draft("From nowhere.", None))).unwrap();
    assert_eq!(shelf.lessons().from_case(&at.case).count(), 1);
    shelf.delete_case(click(), &other, true).unwrap();
    let kept = shelf.lessons();
    assert_eq!(kept.lessons.len(), 3, "kept, unlinked");
    assert_eq!(kept.from_case(&other.case).count(), 0, "the link is gone");
    assert_eq!(kept.from_case(&at.case).count(), 1);
    shelf.delete_case(click(), &at, false).unwrap();
    let left: Vec<String> = shelf.lessons().lessons.iter().map(|l| l.text.for_person().to_string()).collect();
    assert_eq!(left, ["From the SaaS deal.", "From nowhere."]);
    assert!(!shelf.exists(&at));
}

/// A delete refused because another window holds the case's lock leaves its lessons as they were:
/// the person may not try again, so nothing of the delete happens until the case can go.
#[test]
fn a_delete_refused_as_busy_leaves_the_case_s_lessons_alone() {
    use crate::cases::LessonOp;
    let (shelf, at) = shelf_with_case("lessons-busy");
    shelf.lesson(click(), LessonOp::Keep(lesson_draft("From the MSA.", Some(&at)))).unwrap();
    {
        let _held = crate::atomic::Lock::take(&shelf.lock_path(&at)).unwrap();
        assert_eq!(shelf.delete_case(click(), &at, false), Err(CaseError::Busy));
        assert_eq!(shelf.lessons().from_case(&at.case).count(), 1, "still there and still linked");
        assert!(shelf.exists(&at));
    }
    shelf.delete_case(click(), &at, false).unwrap();
    assert!(shelf.lessons().lessons.is_empty());
}

/// A damaged lessons file is moved aside on the person's next write, never replaced in silence,
/// and until then is read as damaged with no lesson in it.
#[test]
fn a_damaged_lessons_file_moves_aside() {
    use crate::cases::LessonOp;
    use crate::cases::lessons::FILE;
    let (shelf, at) = shelf_with_case("lessons-damaged");
    let path = shelf.lessons.clone();
    assert!(path.ends_with(FILE));
    let aside = path.with_extension(format!("json{DAMAGED_SUFFIX}"));
    shelf.lesson(click(), LessonOp::Keep(lesson_draft("Kept before the damage.", Some(&at)))).unwrap();
    let newer = br#"{"version": 2, "lessons": [{"id": "l1", "text": "From a newer build", "applies": {"size": "huge"}}]}"#.to_vec();
    for (what, bytes) in [("junk", b"{not json".to_vec()), ("an unknown version or field", newer)] {
        std::fs::write(&path, &bytes).unwrap();
        let _ = std::fs::remove_file(&aside);
        let read = shelf.lessons();
        assert!(read.damaged && read.lessons.is_empty(), "{what} reads as damaged, with nothing in it");
        shelf.lesson(click(), LessonOp::Keep(lesson_draft("Kept after.", None))).unwrap();
        assert_eq!(std::fs::read(&aside).unwrap(), bytes, "{what} is moved aside, not replaced in silence");
        let now = shelf.lessons();
        assert!(!now.damaged);
        assert_eq!(now.lessons.len(), 1);
        assert_eq!(now.lessons[0].text.for_person(), "Kept after.");
    }
}

#[test]
fn a_lesson_s_text_is_held_to_the_limit_and_ids_are_not_reused() {
    use crate::cases::LessonOp;
    let (shelf, _) = shelf_with_case("lessons-limit");
    let long: String = "x".repeat(MAX_TEXT + 1);
    assert_eq!(shelf.lesson(click(), LessonOp::Keep(lesson_draft(&long, None))), Err(CaseError::TooLong));
    assert_eq!(shelf.lesson(click(), LessonOp::Keep(lesson_draft("  ", None))), Err(CaseError::TooLong));
    let first = shelf.lesson(click(), LessonOp::Keep(lesson_draft("One.", None))).unwrap().unwrap();
    shelf.lesson(click(), LessonOp::Remove(first)).unwrap();
    let second = shelf.lesson(click(), LessonOp::Keep(lesson_draft("Two.", None))).unwrap().unwrap();
    assert_ne!(first, second);
    assert_eq!(shelf.lesson(click(), LessonOp::Edit(first, "x".into())), Err(CaseError::NoSuchLesson(first)));
    shelf.lesson(click(), LessonOp::Off(second, true)).unwrap();
    assert!(shelf.lessons().lessons[0].off);
}

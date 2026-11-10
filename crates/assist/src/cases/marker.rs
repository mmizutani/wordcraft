//! What a model writes into a case from its answer (#107): `[[case]]{json}[[/case]]`, parsed with
//! serde into [`CaseMarker`], so a new kind of note is a new variant and no marker code changes. A
//! document can make a model write one, so a marker carries no authority beyond what
//! [`CaseMarker::route`] gives it: a note or a balance is stored `Unconfirmed` in the conversation's
//! own case, a plan replaces only a model's plan, and facts are always a card. Nothing here
//! confirms, binds, deletes, or crosses a case.

use serde::{Deserialize, Serialize};

use super::lessons::LessonProposal;
use super::record::{
    BalanceDraft, ContractKind, CounterpartyKind, Lang, Law, ModelWrite, NoteDraft, NoteId, OurSide, Paper, Plan, Quoted, SizeBand, Standing,
};

/// Most case markers one turn keeps; more are dropped and counted.
pub const MAX_PER_TURN: u8 = 5;

/// Most case markers a review's turn keeps ([`REVIEW_ASK`]): it answers about every note.
pub const MAX_PER_REVIEW: u8 = 32;

/// One marker's body, by `op`. An `op` this build doesn't know, or a field a record doesn't, fails
/// to parse and is dropped and counted. The last four mean something only in a review's answer
/// ([`CaseMarker::review_only`]); in any other turn the window drops and counts them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum CaseMarker {
    Note(NoteDraft),
    Plan(PlanDraft),
    Balance(BalanceDraft),
    Facts(FactsDraft),
    Lesson(LessonProposal),
    Correct { note: NoteId, text: Quoted, why: Quoted },
    Retire { note: NoteId, why: Quoted },
    Supported { note: NoteId, evidence: Quoted },
    Gap { question: Quoted },
}

/// A plan as a marker gives it: the shelf sets its standing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanDraft {
    pub goal: Quoted,
    #[serde(default)]
    pub next: Vec<Quoted>,
}

/// Facts a model proposes: a card, never a write. No names: the person types those, so `parties`
/// here is an unknown field and the marker is dropped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct FactsDraft {
    pub kind: Option<ContractKind>,
    pub side: Option<OurSide>,
    pub paper: Option<Paper>,
    pub theirs: Option<CounterpartyKind>,
    pub law: Option<Law>,
    pub size: Option<SizeBand>,
    pub language: Option<Lang>,
}

/// Where a marker goes: into the case, in front of the person as a card, or in front of the
/// person as a row that changes nothing (a review's evidence for a note, or its question).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    Write(ModelWrite),
    Card(Card),
    Supported { note: NoteId, evidence: Quoted },
    Gap { question: Quoted },
}

/// What the person answers with a click: facts, which a model never sets, a lesson, which crosses
/// clients and so is always theirs to keep, or a write the case didn't take
/// ([`super::record::Applied::Card`]) or that [`ModelWrites::CardsOnly`] turned into one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "card")]
pub enum Card {
    Facts(FactsDraft),
    Lesson(LessonProposal),
    Write(ModelWrite),
}

/// Whether a model's note is stored without a click (the owner's decision in #107), or every write
/// is a card the person answers, which keeps the promise #86 made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelWrites {
    InCase,
    CardsOnly,
}

/// Assumption 1, in one place.
pub const MODEL_WRITES: ModelWrites = ModelWrites::InCase;

impl Default for ModelWrites {
    fn default() -> ModelWrites {
        MODEL_WRITES
    }
}

impl CaseMarker {
    /// The body between `[[case]]` and `[[/case]]`.
    pub fn parse(body: &str) -> Option<CaseMarker> {
        serde_json::from_str(body).ok()
    }

    /// Whether the marker means something only in a review's answer ([`REVIEW_ASK`]). The window
    /// drops and counts one of these in any other turn, as it does an unknown marker.
    pub fn review_only(&self) -> bool {
        matches!(self, CaseMarker::Correct { .. } | CaseMarker::Retire { .. } | CaseMarker::Supported { .. } | CaseMarker::Gap { .. })
    }

    /// What the marker does under `writes`. Facts and a lesson are always a card; a review's
    /// evidence and question are rows and never a write.
    pub fn route(self, writes: ModelWrites) -> Route {
        let write = match self {
            CaseMarker::Facts(facts) => return Route::Card(Card::Facts(facts)),
            CaseMarker::Lesson(lesson) => return Route::Card(Card::Lesson(lesson)),
            CaseMarker::Supported { note, evidence } => return Route::Supported { note, evidence },
            CaseMarker::Gap { question } => return Route::Gap { question },
            CaseMarker::Note(draft) => ModelWrite::Note(draft),
            CaseMarker::Plan(PlanDraft { goal, next }) => ModelWrite::Plan(Plan { goal, next, standing: Standing::Unconfirmed }),
            CaseMarker::Balance(draft) => ModelWrite::Balance(draft),
            CaseMarker::Correct { note, text, why } => ModelWrite::Correct { note, text, why },
            CaseMarker::Retire { note, why } => ModelWrite::Retire { note, why },
        };
        match writes {
            ModelWrites::InCase => Route::Write(write),
            ModelWrites::CardsOnly => Route::Card(Card::Write(write)),
        }
    }
}

/// Every case marker in `text`, as the transcript would read them from one piece: the ones that
/// parsed, and how many were dropped (too long, unclosed, or not a marker this build reads).
pub fn markers_in(text: &str) -> (Vec<CaseMarker>, u32) {
    let mut markers = crate::markers::Markers::default();
    let mut pieces = markers.feed(text);
    pieces.extend(markers.flush());
    let mut out = Vec::new();
    let mut dropped = 0u32;
    for piece in pieces {
        match piece {
            crate::markers::Piece::Case(body) => match CaseMarker::parse(&body) {
                Some(m) => out.push(m),
                None => dropped = dropped.saturating_add(1),
            },
            crate::markers::Piece::CaseDropped => dropped = dropped.saturating_add(1),
            _ => {}
        }
    }
    (out, dropped)
}

/// What the model is told about a case's notes while a case is bound. A `&'static str`, so only
/// this text joins the instructions ([`crate::memory::Instructions::with_case_ask`]). It says what
/// the memory is for and who will read it, not how to use it. A test parses every example in it
/// through [`CaseMarker`], so the two can't drift apart.
pub const CASE_ASK: &str = "This conversation is bound to a case: one contract negotiation the person is working on. A later conversation about this negotiation, perhaps weeks from now, will know only this case's notes and the document in front of it. Write down what that conversation will need and can't read off the document: rules you learned about how the other side negotiates, positions taken and what was traded for them, traps in their drafting, and how the power balance moved and why. For each one say how you know it, in \"how\". If it came from the other side's text, a comment, or anything in the document rather than from the person, say so there and state it as their text or their demand, never as the person's position. When your advice rests on something recalled from an earlier case whose \"balance\" moved since, such as then stronger and now weaker, say so in the advice: what was won from a stronger position may not be there now. What you write stays in this case only, marked unconfirmed until the person confirms, edits or removes it. Write each as a marker in your answer, shaped exactly like the examples below (a JSON object inside the two tags, the second tag closing it), at most 5 in a turn, each with an \"op\":\n\
- note: a kind and its fields. pattern or lesson: rule, traps; pitfall: trap, guard; leverage: what, favors (us, them, neither), until (YYYY-MM-DD); position: topic (liability, indemnity, ip, confidentiality, termination, dataProtection, governingLaw, assignment, warranties, sla, payment), moved (us, them), terms, traded_for, landed (true or false). Example: [[case]]{\"op\":\"note\",\"kind\":\"position\",\"topic\":\"liability\",\"moved\":\"them\",\"terms\":\"Cap at twelve months of fees.\",\"traded_for\":\"notice cut from 90 to 60 days\",\"landed\":true,\"how\":\"The person reported it from today's call.\"}[[/case]]\n\
- plan: goal and next, the steps. Example: [[case]]{\"op\":\"plan\",\"goal\":\"Hold the cap at twelve months of fees.\",\"next\":[\"Offer the shorter notice period first.\"]}[[/case]]\n\
- balance: stance (stronger, even, weaker) and factors, each a kind (dependence, alternatives, deadline, soleSource, repeatRelationship, pen, other), favors (us, them, neither) and why. Example: [[case]]{\"op\":\"balance\",\"stance\":\"weaker\",\"factors\":[{\"kind\":\"deadline\",\"favors\":\"them\",\"why\":\"Our quarter closes first.\"}]}[[/case]]\n\
- facts: kind (nda, msa, saas, licence, services, supply, distribution, employment, lease, loan, acquisition, other), side (vendor, customer, licensor, licensee, employer, employee, lender, borrower, buyer, seller, landlord, tenant, other), paper (ours, theirs, thirdParty), theirs (startup, sme, enterprise, public, individual, other), law (JP, US-NY), size (small, medium, large, huge), language (en, ja, other). The person decides these: a facts marker is only a suggestion to them. Example: [[case]]{\"op\":\"facts\",\"kind\":\"saas\",\"side\":\"customer\",\"paper\":\"theirs\",\"law\":\"JP\"}[[/case]]\n\
- lesson: text, one sentence that would help in a similar deal for any client, naming no party, and notes, the ids of this case's notes it rests on. The person decides whether to keep it: a lesson marker is only a proposal to them. Example: [[case]]{\"op\":\"lesson\",\"text\":\"Ask for the SLA credit before the cap: it is cheaper for the vendor to give.\",\"notes\":[\"n3\"]}[[/case]]";

/// What **Review this case** asks: the person's prompt for the review turn, a constant, so a
/// document can't shape it. The case's notes and its record travel beside it as one data-grade
/// source, never in this text. It names the four verbs that mean something only in this answer;
/// a test parses every example here, as [`CASE_ASK`]'s does.
pub const REVIEW_ASK: &str = "Review this case for the next conversation about this negotiation, which will know only this case's notes and the document. Beside this message is one source, marked as data: the case's notes with their ids, kinds, standing (confirmed by me, or unconfirmed), how each was known and its date; its balance and plan; and the most recent lines of its record, if it keeps one (each turn as it ended, my Accept and Reject of changes, my edits of your text, the other side's redlines). Check each unconfirmed note against the record: is it still true, is it stated as the other side's text where it came from the other side, is anything in it no longer so? Then say what is likely to have changed: the plan, the balance, open positions. Answer in prose, and for each note you judged write one marker, shaped exactly like the examples below (a JSON object inside the two tags, the second tag closing it), at most 32 in all, each with an \"op\" and the note's id:\n\
- correct: the note's main text as it should read now, and why. Example: [[case]]{\"op\":\"correct\",\"note\":\"n2\",\"text\":\"Cap at eighteen months of fees.\",\"why\":\"The record's turn 4 reports eighteen, not twelve.\"}[[/case]]\n\
- retire: the note no longer holds, and why. Example: [[case]]{\"op\":\"retire\",\"note\":\"n5\",\"why\":\"They dropped the audit demand in the May redline.\"}[[/case]]\n\
- supported: the record bears the note out; say where. Example: [[case]]{\"op\":\"supported\",\"note\":\"n1\",\"evidence\":\"Turn 2: the person reported the cap from the call.\"}[[/case]]\n\
- gap: a question the record can't answer, for me. Example: [[case]]{\"op\":\"gap\",\"question\":\"Did the call on the notice period land at 60 days?\"}[[/case]]\n\
A note I confirmed changes only if I agree: your correction or retirement of one shows me a card. The note, plan, balance and lesson markers work here as in any turn. Write nothing of this into the document.";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cases::record::{Body, Kind, Stance};

    #[test]
    fn every_example_in_case_ask_parses_and_covers_every_op() {
        let (markers, dropped) = markers_in(CASE_ASK);
        assert_eq!(dropped, 0, "{markers:?}");
        let ops: Vec<&str> = markers
            .iter()
            .map(|m| match m {
                CaseMarker::Note(_) => "note",
                CaseMarker::Plan(_) => "plan",
                CaseMarker::Balance(_) => "balance",
                CaseMarker::Facts(_) => "facts",
                CaseMarker::Lesson(_) => "lesson",
                other => panic!("a review verb in CASE_ASK: {other:?}"),
            })
            .collect();
        assert_eq!(ops, ["note", "plan", "balance", "facts", "lesson"]);
        assert!(matches!(&markers[4], CaseMarker::Lesson(l) if l.notes.len() == 1));
        assert!(matches!(&markers[0], CaseMarker::Note(n) if n.body.kind() == Kind::Position && n.how.is_some()));
        assert!(CASE_ASK.len() < 4 << 10, "{}", CASE_ASK.len());
    }

    #[test]
    fn an_unknown_op_a_name_in_facts_and_a_confirm_are_dropped() {
        for body in [
            r#"{"op":"confirm","note":"n1"}"#,
            r#"{"op":"facts","parties":["Acme"]}"#,
            r#"{"op":"delete"}"#,
            r#"{"op":"bind","case":"0123456789abcdef"}"#,
            r#"{"op":"note","kind":"pattern","rule":"x","standing":{"confirmed":{"on":"2026-01-01"}}}"#,
            "not json",
        ] {
            assert!(CaseMarker::parse(body).is_none(), "{body}");
        }
    }

    #[test]
    fn facts_are_a_card_under_either_switch_and_the_switch_decides_the_rest() {
        let note = || CaseMarker::parse(r#"{"op":"note","kind":"pitfall","trap":"They widen Affiliate.","how":"Clause 1."}"#).unwrap();
        let plan = || CaseMarker::parse(r#"{"op":"plan","goal":"Hold the cap."}"#).unwrap();
        let balance = || CaseMarker::parse(r#"{"op":"balance","stance":"weaker"}"#).unwrap();
        let facts = || CaseMarker::parse(r#"{"op":"facts","kind":"msa"}"#).unwrap();
        for m in [note(), plan(), balance()] {
            assert!(matches!(m.clone().route(ModelWrites::InCase), Route::Write(_)), "{m:?}");
            assert!(matches!(m.route(ModelWrites::CardsOnly), Route::Card(Card::Write(_))));
        }
        let lesson = || CaseMarker::parse(r#"{"op":"lesson","text":"Ask for the credit first.","notes":["n1"]}"#).unwrap();
        for writes in [ModelWrites::InCase, ModelWrites::CardsOnly] {
            assert!(matches!(facts().route(writes), Route::Card(Card::Facts(f)) if f.kind == Some(ContractKind::Msa)));
            assert!(matches!(lesson().route(writes), Route::Card(Card::Lesson(l)) if l.notes.len() == 1));
        }
        assert!(CaseMarker::parse(r#"{"op":"lesson","text":"x","from":"0123456789abcdef"}"#).is_none(), "a marker names no case");
        match note().route(ModelWrites::InCase) {
            Route::Write(ModelWrite::Note(n)) => assert!(matches!(n.body, Body::Pitfall { .. }) && n.how.is_some()),
            other => panic!("{other:?}"),
        }
        match plan().route(ModelWrites::InCase) {
            Route::Write(ModelWrite::Plan(p)) => assert_eq!(p.standing, Standing::Unconfirmed),
            other => panic!("{other:?}"),
        }
        assert!(matches!(balance().route(ModelWrites::InCase), Route::Write(ModelWrite::Balance(b)) if b.stance == Stance::Weaker));
    }

    #[test]
    fn every_example_in_review_ask_parses_and_covers_every_review_verb() {
        let (markers, dropped) = markers_in(REVIEW_ASK);
        assert_eq!(dropped, 0, "{markers:?}");
        let ops: Vec<&str> = markers
            .iter()
            .map(|m| match m {
                CaseMarker::Correct { .. } => "correct",
                CaseMarker::Retire { .. } => "retire",
                CaseMarker::Supported { .. } => "supported",
                CaseMarker::Gap { .. } => "gap",
                _ => "other",
            })
            .collect();
        assert_eq!(ops, ["correct", "retire", "supported", "gap"]);
        assert!(markers.iter().all(CaseMarker::review_only));
        assert!(!CaseMarker::parse(r#"{"op":"note","kind":"pattern","rule":"x"}"#).unwrap().review_only());
        assert!(REVIEW_ASK.len() < 4 << 10, "{}", REVIEW_ASK.len());
    }

    fn case_with(confirmed: &str, unconfirmed: &str) -> super::super::record::Case {
        let today = super::super::record::Day::parse("2026-11-10").unwrap();
        let draft = |text: &str| NoteDraft { body: Body::Pattern { rule: Quoted::parse(text), traps: None }, how: None, topics: Default::default() };
        let mut case = super::super::record::Case::new("Kestrel Freight KK supply");
        case.apply_person(super::super::record::PersonOp::AddNote(draft(confirmed)), today).unwrap();
        case.apply_model(ModelWrite::Note(draft(unconfirmed)), today).unwrap();
        case
    }

    #[test]
    fn a_correction_applies_to_an_unconfirmed_note_and_is_a_card_for_a_confirmed_one() {
        use super::super::record::{Applied, Case, Day};
        let today = Day::parse("2026-11-10").unwrap();
        let mut case: Case = case_with("They never move on the cap.", "The cap is twelve months.");
        let correct = |note: &str, text: &str| {
            CaseMarker::parse(&format!(r#"{{"op":"correct","note":"{note}","text":"{text}","why":"Turn 4 of the record says eighteen."}}"#)).unwrap()
        };
        let Route::Write(write) = correct("n2", "The cap is eighteen months.").route(ModelWrites::InCase) else { panic!() };
        assert_eq!(case.apply_model(write, today), Ok(Applied::Edited(super::super::record::NoteId::parse("n2").unwrap())));
        let n2 = case.notes.iter().find(|n| n.id == super::super::record::NoteId::parse("n2").unwrap()).unwrap();
        assert_eq!(n2.body.texts().next().unwrap().for_person(), "The cap is eighteen months.");
        assert_eq!(n2.how.as_ref().unwrap().for_person(), "Turn 4 of the record says eighteen.");
        assert_eq!(n2.standing, Standing::Unconfirmed);
        let Route::Write(write) = correct("n1", "They move on the cap.").route(ModelWrites::InCase) else { panic!() };
        assert!(matches!(case.apply_model(write, today), Ok(Applied::Card(ModelWrite::Correct { .. }))));
        assert_eq!(case.notes[0].body.texts().next().unwrap().for_person(), "They never move on the cap.", "the confirmed note stands");
        let Route::Write(write) = correct("n9", "x").route(ModelWrites::InCase) else { panic!() };
        assert!(matches!(case.apply_model(write, today), Err(super::super::record::CaseError::NoSuchNote(_))));
    }

    #[test]
    fn a_retired_unconfirmed_note_goes_and_joins_dismissed_and_a_confirmed_one_is_a_card() {
        use super::super::record::{Applied, Case, Day, NoteId};
        let today = Day::parse("2026-11-10").unwrap();
        let mut case: Case = case_with("They never move on the cap.", "Bluefin Systems Ltd signs by March.");
        let retire =
            |note: &str| CaseMarker::parse(&format!(r#"{{"op":"retire","note":"{note}","why":"The record shows they signed in May."}}"#)).unwrap();
        let Route::Write(write) = retire("n2").route(ModelWrites::InCase) else { panic!() };
        assert_eq!(case.apply_model(write, today), Ok(Applied::Removed(NoteId::parse("n2").unwrap())));
        assert_eq!(case.notes.len(), 1);
        assert_eq!(case.dismissed().len(), 1, "its text joins dismissed");
        let again = NoteDraft {
            body: Body::Pattern { rule: Quoted::parse("Bluefin Systems Ltd signs by March."), traps: None },
            how: None,
            topics: Default::default(),
        };
        assert_eq!(case.apply_model(ModelWrite::Note(again), today), Ok(Applied::Known), "so the model doesn't note it again");
        let Route::Write(write) = retire("n1").route(ModelWrites::InCase) else { panic!() };
        assert!(matches!(case.apply_model(write, today), Ok(Applied::Card(ModelWrite::Retire { .. }))));
        assert_eq!(case.notes.len(), 1, "the confirmed note stands");
        assert!(matches!(retire("n1").route(ModelWrites::CardsOnly), Route::Card(Card::Write(ModelWrite::Retire { .. }))));
    }

    #[test]
    fn supported_and_gap_are_rows_not_writes() {
        let supported = CaseMarker::parse(r#"{"op":"supported","note":"n1","evidence":"Turn 2: the person reported the call."}"#).unwrap();
        let gap = CaseMarker::parse(r#"{"op":"gap","question":"Did the May call settle the notice period?"}"#).unwrap();
        for writes in [ModelWrites::InCase, ModelWrites::CardsOnly] {
            assert!(matches!(supported.clone().route(writes), Route::Supported { evidence, .. } if evidence.for_person().starts_with("Turn 2")));
            assert!(matches!(gap.clone().route(writes), Route::Gap { question } if question.for_person().starts_with("Did")));
        }
        assert!(CaseMarker::parse(r#"{"op":"correct","note":"n1","text":"x"}"#).is_none(), "a correction says why");
        assert!(CaseMarker::parse(r#"{"op":"retire","note":"bogus","why":"x"}"#).is_none(), "an id is a note's");
    }
}

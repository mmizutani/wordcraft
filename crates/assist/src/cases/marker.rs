//! What a model writes into a case from its answer (#107): `[[case]]{json}[[/case]]`, parsed with
//! serde into [`CaseMarker`], so a new kind of note is a new variant and no marker code changes. A
//! document can make a model write one, so a marker carries no authority beyond what
//! [`CaseMarker::route`] gives it: a note or a balance is stored `Unconfirmed` in the conversation's
//! own case, a plan replaces only a model's plan, and facts are always a card. Nothing here
//! confirms, binds, deletes, or crosses a case.

use serde::{Deserialize, Serialize};

use super::lessons::LessonProposal;
use super::record::{
    BalanceDraft, ContractKind, CounterpartyKind, Lang, Law, ModelWrite, NoteDraft, OurSide, Paper, Plan, Quoted, SizeBand, Standing,
};

/// Most case markers one turn keeps; more are dropped and counted.
pub const MAX_PER_TURN: u8 = 5;

/// One marker's body, by `op`. An `op` this build doesn't know, or a field a record doesn't, fails
/// to parse and is dropped and counted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum CaseMarker {
    Note(NoteDraft),
    Plan(PlanDraft),
    Balance(BalanceDraft),
    Facts(FactsDraft),
    Lesson(LessonProposal),
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

/// Where a marker goes: into the case, or in front of the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    Write(ModelWrite),
    Card(Card),
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

    /// What the marker does under `writes`. Facts and a lesson are always a card.
    pub fn route(self, writes: ModelWrites) -> Route {
        let write = match self {
            CaseMarker::Facts(facts) => return Route::Card(Card::Facts(facts)),
            CaseMarker::Lesson(lesson) => return Route::Card(Card::Lesson(lesson)),
            CaseMarker::Note(draft) => ModelWrite::Note(draft),
            CaseMarker::Plan(PlanDraft { goal, next }) => ModelWrite::Plan(Plan { goal, next, standing: Standing::Unconfirmed }),
            CaseMarker::Balance(draft) => ModelWrite::Balance(draft),
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
}

//! What similarity means, and how it uses the power balance. A past item is worth recalling when
//! it is about the thing at hand, comes from a similar deal, and still transfers given who holds
//! the power now. Those are three questions, so [`score`] multiplies three answers: a mismatch
//! discounts an item and can never make an irrelevant one relevant.
//!
//! The second half is how a turn gets what ranks highest ([`RecallAsk`]). It reads the bound case
//! and the other cases of its wall ([`Pool`]), one client's folder and nothing else, ranks, and
//! composes what fits as one JSON string the turn carries as data ([`Recalled::into_sources`]).
//! It runs only with proof that no one but the person and this conversation's assistant reads the
//! document ([`Alone`]), and never on the instruction channel: nothing here can make a
//! [`crate::memory::Instructions`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wordcraft_engine::room::{ParticipantId, Room};

use super::lessons::{KeptLesson, LessonId};
use super::record::{Applies, Body, Case, CaseError, Day, Kind, Note, Quoted, Side, Stance, Standing, Topic, Topics};
use super::store::{CaseId, CaseRef, Shelf};
use super::text::{Bm25, Token, tokens, topics_in};
use crate::backend::{cut, label, shown};
use crate::knowledge::{SourceOrigin, TurnSource};

/// Where a candidate comes from, against the query's case. A lesson is from any client, and is
/// worth more than a neighbor's note because the person chose it, less than this case's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Place {
    ThisCase,
    SameClient,
    Lesson,
}

impl Place {
    fn weight(self) -> f32 {
        match self {
            Place::ThisCase => 1.5,
            Place::Lesson => 1.2,
            Place::SameClient => 1.0,
        }
    }
}

/// One turn's question, with the facts of the case it is asked from as of today.
pub struct Query {
    topics: Topics,
    tokens: Vec<Token>,
    applies: Applies,
    today: Day,
}

impl Query {
    pub fn new(about: &str, applies: Applies, today: Day) -> Query {
        Query { topics: topics_in(about), tokens: tokens(about), applies, today }
    }
}

/// A note seen from the query's case, or a lesson the person kept.
#[derive(Clone, Copy)]
pub enum Candidate<'a> {
    Note { note: &'a Note, case: &'a Case, place: Place },
    Lesson(&'a KeptLesson),
}

impl<'a> Candidate<'a> {
    /// The facts and stance as of when the item was learned.
    fn applies(&self) -> Applies {
        match self {
            Candidate::Note { note, case, .. } => case.applies(note.at),
            Candidate::Lesson(l) => l.applies,
        }
    }

    fn tokens(&self) -> Vec<Token> {
        match self {
            Candidate::Note { note, .. } => note.texts().flat_map(tokens).take(super::text::MAX_TOKENS).collect(),
            Candidate::Lesson(l) => tokens(l.text.raw()),
        }
    }

    fn topics(&self) -> Topics {
        match self {
            Candidate::Note { note, .. } => note.topics(),
            Candidate::Lesson(l) => l.topics.union(topics_in(l.text.raw())),
        }
    }

    fn kind(&self) -> Kind {
        match self {
            Candidate::Note { note, .. } => note.kind(),
            Candidate::Lesson(_) => Kind::Lesson,
        }
    }

    fn place(&self) -> Place {
        match self {
            Candidate::Note { place, .. } => *place,
            Candidate::Lesson(_) => Place::Lesson,
        }
    }

    /// A lesson keeps only its year, so it ages from that year's first day; with none it doesn't age.
    fn age(&self, today: Day) -> f32 {
        match self {
            Candidate::Note { note, .. } => age(note.kind(), note.at, today),
            Candidate::Lesson(l) => l.applies.year.and_then(Day::first_of).map_or(1.0, |on| age(Kind::Lesson, on, today)),
        }
    }

    fn standing(&self) -> f32 {
        match self {
            Candidate::Note { note, .. } => standing(note.standing),
            Candidate::Lesson(_) => 1.0,
        }
    }

    /// The note, when it is one: for the corpus tests.
    pub fn note(&self) -> Option<&'a Note> {
        match self {
            Candidate::Note { note, .. } => Some(note),
            Candidate::Lesson(_) => None,
        }
    }
}

pub struct Ranked<'a> {
    pub score: f32,
    pub candidate: Candidate<'a>,
}

/// Weight of one shared topic against BM25 word overlap.
const TOPIC: f32 = 2.0;

/// What a pitfall scores on a query with words but no topic, so "review this draft" still finds
/// traps. A query with a topic the pitfall lacks gets nothing from it, and a query with no
/// searchable word at all (hiragana alone) is nothing to search for.
const PITFALL_FLOOR: f32 = 0.5;

/// A field both sides know and disagree on multiplies by its entry; a field either side lacks
/// multiplies by [`UNKNOWN`]. Our side matters most: a vendor's lesson reads backwards to a customer.
const UNKNOWN: f32 = 0.85;
const OTHER_KIND: f32 = 0.5;
const OTHER_SIDE: f32 = 0.4;
const OTHER_LAW: f32 = 0.7;
const OTHER_REGION: f32 = 0.9;
const OTHER_THEIRS: f32 = 0.8;
const OTHER_PAPER: f32 = 0.9;

fn field<T: PartialEq>(now: Option<T>, then: Option<T>, other: f32) -> f32 {
    match (now, then) {
        (Some(a), Some(b)) => {
            if a == b {
                1.0
            } else {
                other
            }
        }
        _ => UNKNOWN,
    }
}

/// How well a past deal's facts fit the current one. 1.0 for an item of this very case.
pub fn fit(now: &Applies, then: &Applies) -> f32 {
    let law = match (now.law, then.law) {
        (Some(a), Some(b)) if a == b => 1.0,
        (Some(a), Some(b)) if a.same_country(b) => OTHER_REGION,
        (Some(_), Some(_)) => OTHER_LAW,
        _ => UNKNOWN,
    };
    field(now.kind, then.kind, OTHER_KIND)
        * field(now.side, then.side, OTHER_SIDE)
        * field(now.paper, then.paper, OTHER_PAPER)
        * field(now.theirs, then.theirs, OTHER_THEIRS)
        * law
}

/// How much an item learned under one balance says under another, by kind and by the gap, now
/// minus then, with Stronger as +1, Even as 0 and Weaker as -1. Columns: gap -2, -1, 0, +1, +2,
/// unknown. The numbers are guesses and the calibration knob; the corpus test pins their order.
///
/// Pitfall: a silent edit is a trap whoever is stronger. Leverage is the most circumstantial.
/// Position is asymmetric: a concession won while stronger says little to someone weaker now
/// (gap -2), and one won while weaker is a floor for someone stronger now (gap +2).
fn transfer_row(kind: Kind) -> [f32; 6] {
    match kind {
        Kind::Pitfall => [1.00, 1.00, 1.00, 1.00, 1.00, 1.00],
        Kind::Pattern => [0.70, 0.85, 1.00, 0.85, 0.70, 0.85],
        Kind::Lesson => [0.50, 0.75, 1.00, 0.90, 0.80, 0.80],
        Kind::Leverage => [0.20, 0.50, 1.00, 0.60, 0.40, 0.60],
        Kind::Position => [0.25, 0.60, 1.00, 0.95, 0.90, 0.60],
    }
}

fn rung(stance: Stance) -> i8 {
    match stance {
        Stance::Stronger => 1,
        Stance::Even => 0,
        Stance::Weaker => -1,
    }
}

pub fn transfer(kind: Kind, then: Option<Stance>, now: Option<Stance>) -> f32 {
    let column = match (then, now) {
        (Some(then), Some(now)) => usize::from((rung(now) - rung(then) + 2).unsigned_abs()).min(4),
        _ => 5,
    };
    transfer_row(kind).get(column).copied().unwrap_or(1.0)
}

/// Half-life in days, by kind. Leverage is about one deal's moment; a pitfall or a lesson lasts.
fn half_life(kind: Kind) -> f32 {
    match kind {
        Kind::Pitfall | Kind::Lesson => 1095.0,
        Kind::Pattern => 730.0,
        Kind::Position => 365.0,
        Kind::Leverage => 180.0,
    }
}

/// Exponential decay by the kind's half-life, with a floor so an old item is discounted, never gone.
pub fn age(kind: Kind, on: Day, today: Day) -> f32 {
    0.5f32.powf(today.days_since(on) as f32 / half_life(kind)).max(0.2)
}

fn standing(s: Standing) -> f32 {
    match s {
        Standing::Confirmed { .. } => 1.0,
        Standing::Unconfirmed => 0.7,
    }
}

/// About the thing at hand: stated or found topics in common, plus word overlap. Zero recalls
/// nothing, so similar facts alone never do.
fn relevance(q: &Query, c: &Candidate<'_>, words: &Bm25) -> f32 {
    let topics = TOPIC * q.topics.shared(c.topics()) as f32;
    let floor = if q.topics.is_empty() && !q.tokens.is_empty() && c.kind() == Kind::Pitfall { PITFALL_FLOOR } else { 0.0 };
    (topics + words.score(&q.tokens, &c.tokens())).max(floor)
}

pub fn score(q: &Query, c: &Candidate<'_>, words: &Bm25) -> f32 {
    let relevance = relevance(q, c, words);
    if relevance == 0.0 {
        return 0.0;
    }
    let then = c.applies();
    relevance * fit(&q.applies, &then) * transfer(c.kind(), then.stance, q.applies.stance) * c.age(q.today) * c.place().weight() * c.standing()
}

fn ranked_by<'a>(pool: &[Candidate<'a>], mut f: impl FnMut(&Candidate<'a>, &Bm25) -> f32) -> Vec<Ranked<'a>> {
    let docs: Vec<Vec<Token>> = pool.iter().map(Candidate::tokens).collect();
    let words = Bm25::over(docs.iter().map(Vec::as_slice));
    let mut out: Vec<Ranked<'a>> = pool.iter().map(|c| Ranked { score: f(c, &words), candidate: *c }).filter(|r| r.score > 0.0).collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

/// The pool ranked for the query, best first, with what scores nothing left out.
pub fn rank<'a>(q: &Query, pool: &[Candidate<'a>]) -> Vec<Ranked<'a>> {
    ranked_by(pool, |c, words| score(q, c, words))
}

/// The same pool ranked on words alone: the baseline the typed ranking is measured against.
pub fn rank_by_text<'a>(q: &Query, pool: &[Candidate<'a>]) -> Vec<Ranked<'a>> {
    ranked_by(pool, |c, words| words.score(&q.tokens, &c.tokens()))
}

/// Most bytes of recalled items one turn carries, as the JSON the model reads.
pub const BUDGET: usize = 6 << 10;
/// Most items one turn carries.
pub const MAX_ITEMS: usize = 12;
/// Most notes one other case contributes to a turn.
pub const MAX_PER_NEIGHBOR: usize = 3;
/// From this many `Position` notes on one of the query's topics, they go as one ladder item.
pub const LADDER_FROM: usize = 3;
/// Below this an item isn't about the thing at hand enough to send. A guess, and the knob.
pub const MIN_SCORE: f32 = 0.1;
/// Most other cases of the wall read for one turn, most recently changed first, and most bytes of
/// their files.
pub const MAX_NEIGHBORS: usize = 64;
pub const MAX_NEIGHBOR_BYTES: u64 = 16 << 20;
/// Most bytes of the text a turn is about (the prompt and the selection).
pub const MAX_ABOUT: usize = 4 << 10;
/// Most item keys one agent session is remembered to have been told; past it, everything is sent
/// again, which only costs tokens.
const MAX_TOLD: usize = 4096;

/// Proof that no one but the person and this conversation's assistant can read the document. Its
/// fields are private: the only way to get one is [`Alone::check`], and [`RecallAsk::new`] takes
/// one, so recalled text can't reach a room the other side is in. Not `Clone`: a proof is used once.
/// It keeps what the check needs, because the window checks on the frame and the conversation's
/// thread runs the recall later: someone who joins in between is found by [`Alone::recheck`].
pub struct Alone {
    room: Arc<Room>,
    me: ParticipantId,
    seat: String,
}

impl std::fmt::Debug for Alone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Alone").field("me", &self.me).field("seat", &self.seat).finish_non_exhaustive()
    }
}

/// Who else can read the document: their names, for Details.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Others {
    pub names: Vec<String>,
}

impl Alone {
    /// `room` holds no participant but `me` (the window's own copy) and `seat` (this conversation's
    /// assistant), and no invite but `seat`'s, joined or not. A room refuses a second invite or
    /// participant under a name it has, so the seat's name is sound to compare.
    pub fn check(room: &Arc<Room>, me: ParticipantId, seat: &str) -> Result<Alone, Others> {
        let names = others(room, me, seat);
        if names.is_empty() { Ok(Alone { room: room.clone(), me, seat: seat.to_string() }) } else { Err(Others { names }) }
    }

    /// The same check, where the recall runs.
    fn recheck(&self) -> Result<(), Others> {
        let names = others(&self.room, self.me, &self.seat);
        if names.is_empty() { Ok(()) } else { Err(Others { names }) }
    }
}

/// Who is in `room`, or invited to it, besides `me` and `seat`.
fn others(room: &Room, me: ParticipantId, seat: &str) -> Vec<String> {
    let mut names: Vec<String> = room.participants().into_iter().filter(|p| p.id != me && p.name != seat).map(|p| label(&p.name)).collect();
    names.extend(room.invites().into_iter().filter(|i| i.name != seat).map(|i| label(&i.name)));
    names.sort();
    names.dedup();
    names
}

/// What one turn asks to recall: from which shelf and case, about what, as of which day, and the
/// proof it may. Built on the frame, run once on the conversation's thread ([`RecallAsk::run`]).
pub struct RecallAsk {
    shelf: Shelf,
    at: CaseRef,
    about: String,
    today: Day,
    alone: Alone,
    /// Run once the files are read, before the proof is checked again ([`RecallAsk::after_reading`]).
    after_reading: Option<Box<dyn FnOnce() + Send>>,
    /// Windows of recalled text the conversation's transcript can still take
    /// ([`crate::Transcript::recall_room`]): a recall with more is withheld.
    room: usize,
}

impl std::fmt::Debug for RecallAsk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecallAsk").field("at", &self.at).field("today", &self.today).field("alone", &self.alone).finish_non_exhaustive()
    }
}

impl RecallAsk {
    pub fn new(shelf: Shelf, at: CaseRef, about: &str, today: Day, alone: Alone) -> RecallAsk {
        RecallAsk {
            shelf,
            at,
            about: cut(shown(about.to_string()), MAX_ABOUT),
            today,
            alone,
            after_reading: None,
            room: crate::transcript::MAX_WINDOWS,
        }
    }

    /// With `room` windows left in the conversation's transcript, set as the turn is sent.
    pub(crate) fn with_room(mut self, room: usize) -> RecallAsk {
        self.room = room;
        self
    }

    /// For tests: `f` runs between reading the files and the second check of the proof, where
    /// someone can join.
    #[doc(hidden)]
    pub fn after_reading(mut self, f: impl FnOnce() + Send + 'static) -> RecallAsk {
        self.after_reading = Some(Box::new(f));
        self
    }

    /// Check again that no one else reads the document, then read the case and its wall's other
    /// cases, rank, compose, and check once more before anything read leaves: reading takes
    /// long enough for someone to join. A case that can't be read makes it return nothing and
    /// say so: a memory failure never stops a turn.
    pub(crate) fn run(self, told: &Told, retains: Option<u64>) -> Recalled {
        let others = |o: Others| Recalled::nothing(format!("{} can read this document", o.names.join(", ")));
        if let Err(o) = self.alone.recheck() {
            return others(o);
        }
        let pool = match Pool::load(&self.shelf, &self.at) {
            Ok(pool) => pool,
            Err(e) => return Recalled::nothing(format!("the case couldn't be read: {e}")),
        };
        let query = Query::new(&self.about, pool.here.applies(self.today), self.today);
        let mut recalled = compose(&pool, &query);
        recalled.sent.problem = pool.problem();
        if let Some(f) = self.after_reading {
            f();
        }
        if let Err(o) = self.alone.recheck() {
            return others(o);
        }
        told.filter(&mut recalled, retains);
        // Fail closed: text that can't be checked for quotes afterwards isn't sent. Nothing in
        // the set leaves to make room, since a retaining session still holds what it was sent.
        if crate::transcript::recall_windows(recalled.sent().sent_text.iter().map(String::as_str)) > self.room {
            return Recalled::nothing(crate::transcript::RECALL_WITHHELD.into());
        }
        recalled
    }
}

/// What recall can see: the bound case, the other cases of its wall, and the lessons the person
/// kept from any client. Built from one wall's folder and the lessons file, and nothing else.
pub(crate) struct Pool {
    here: Case,
    neighbors: Vec<Neighbor>,
    /// The kept lessons that aren't off, from every client.
    lessons: Vec<KeptLesson>,
    /// The bound case's file is damaged: it reads as empty here.
    damaged: bool,
    /// Other cases of the wall skipped as damaged or unreadable.
    skipped: u32,
    /// The lessons file is damaged: no lesson is sent.
    lessons_damaged: bool,
}

impl Pool {
    fn load(shelf: &Shelf, at: &CaseRef) -> Result<Pool, CaseError> {
        let mut here = shelf.open(at)?;
        let damaged = std::mem::take(&mut here.damaged);
        if damaged {
            here = Case::new("");
        }
        let (cases, skipped) = shelf.recent_cases(at, MAX_NEIGHBORS, MAX_NEIGHBOR_BYTES);
        let kept = shelf.lessons();
        let lessons = if kept.damaged { Vec::new() } else { kept.on().cloned().collect() };
        Ok(Pool {
            here,
            neighbors: cases.into_iter().map(|(id, case)| Neighbor::of(id, case)).collect(),
            lessons,
            damaged,
            skipped,
            lessons_damaged: kept.damaged,
        })
    }

    /// Why less than expected was recalled, when the person should know.
    fn problem(&self) -> Option<String> {
        let mut problems = Vec::new();
        if self.damaged {
            problems.push("this case's file is damaged, so nothing of it was recalled".to_string());
        } else if self.skipped > 0 {
            problems.push(format!("{} of this client's cases couldn't be read", self.skipped));
        }
        if self.lessons_damaged {
            problems.push("the lessons file is damaged, so no lesson was recalled".to_string());
        }
        (!problems.is_empty()).then(|| problems.join("; "))
    }
}

/// What a case looks like from another case in its wall: its facts, its confirmed notes and its
/// confirmed balances. [`Neighbor::of`] drops everything unconfirmed, and the plan, so a model's
/// note can't leave the case it was written in.
struct Neighbor {
    id: CaseId,
    case: Case,
}

impl Neighbor {
    fn of(id: CaseId, mut case: Case) -> Neighbor {
        let confirmed = |s: Standing| matches!(s, Standing::Confirmed { .. });
        case.notes.retain(|n| confirmed(n.standing));
        case.balance.retain(|b| confirmed(b.standing));
        case.plan = None;
        Neighbor { id, case }
    }
}

/// What a recalled item is called in `[[used: …]]` and in Details: `n12` for this case's note,
/// `<case>:n12` for another case's, `<case>:balance` for its balance, `l3` for a lesson,
/// `ladder:liability` for a topic's ladder. Disjoint from a preference's `m1`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "where")]
pub enum ItemId {
    Here { note: super::record::NoteId },
    Neighbor { case: CaseId, note: super::record::NoteId },
    NeighborBalance { case: CaseId },
    Lesson { lesson: LessonId },
    Ladder { topic: Topic },
}

impl std::fmt::Display for ItemId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ItemId::Here { note } => write!(f, "{note}"),
            ItemId::Neighbor { case, note } => write!(f, "{}:{note}", case.as_str()),
            ItemId::NeighborBalance { case } => write!(f, "{}:balance", case.as_str()),
            ItemId::Lesson { lesson } => write!(f, "{lesson}"),
            ItemId::Ladder { topic } => write!(f, "ladder:{}", topic_name(*topic)),
        }
    }
}

/// A topic as its JSON name: `liability`, `dataProtection`.
fn topic_name(topic: Topic) -> String {
    serde_json::to_value(topic).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
}

/// One position on a topic, as the ladder shows it: where it is from, when, who moved, the terms,
/// what was traded, whether it landed, the balance then against now, and whether the person
/// confirmed it. The same record goes to the model, the pane and the quote index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rung {
    pub place: Place,
    /// The other case's title, for [`Place::SameClient`].
    pub title: Option<String>,
    pub on: Day,
    pub moved: Side,
    pub terms: String,
    pub traded_for: Option<String>,
    pub landed: bool,
    pub balance: BalanceGap,
    pub confirmed: bool,
}

/// Most rungs one ladder carries. Past it the rungs that order last are counted in `left_out`.
pub const MAX_RUNGS: usize = 8;

/// The clause ladder: every `Position` note on `topic` in the pool, this case's and the same
/// client's confirmed ones, as rungs. Derived per query and stored nowhere, so it can't go stale
/// and deleting a case takes its rungs with it. Lessons and other kinds never enter.
///
/// The order answers "what did we concede, and when were we weaker": the rung whose balance is
/// closest to now first, since that is the precedent that transfers, with an unknown balance
/// last; then landed before open, since a landed position is a fact and an open one an ask; then
/// newest first.
pub(crate) fn ladder(topic: Topic, pool: &Pool, now: Option<Stance>) -> Vec<Rung> {
    let rung_of = |note: &Note, case: &Case, neighbor: Option<&Neighbor>| {
        let Body::Position { topic: t, moved, terms, traded_for, landed } = &note.body else { return None };
        if *t != topic {
            return None;
        }
        let (place, title, _) = place_words(neighbor);
        Some(Rung {
            place,
            title,
            on: note.at,
            moved: *moved,
            terms: terms.for_person().to_string(),
            traded_for: traded_for.as_ref().map(|q| q.for_person().to_string()),
            landed: *landed,
            balance: BalanceGap { then: case.stance_on(note.at), now },
            confirmed: matches!(note.standing, Standing::Confirmed { .. }),
        })
    };
    let here = pool.here.notes.iter().filter_map(|n| rung_of(n, &pool.here, None));
    let theirs = pool.neighbors.iter().flat_map(|nb| nb.case.notes.iter().filter_map(move |n| rung_of(n, &nb.case, Some(nb))));
    let mut rungs: Vec<Rung> = here.chain(theirs).collect();
    let gap = |r: &Rung| match (r.balance.then, now) {
        (Some(then), Some(now)) => (rung(now) - rung(then)).abs(),
        _ => i8::MAX,
    };
    rungs.sort_by(|a, b| gap(a).cmp(&gap(b)).then(b.landed.cmp(&a.landed)).then(b.on.cmp(&a.on)));
    rungs
}

/// The balance an item was learned under, against the balance now: the model sees these words on
/// every item, and the pane shows the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceGap {
    pub then: Option<Stance>,
    pub now: Option<Stance>,
}

impl BalanceGap {
    pub fn words(&self) -> String {
        let word = |s: Option<Stance>| match s {
            Some(Stance::Stronger) => "stronger",
            Some(Stance::Even) => "even",
            Some(Stance::Weaker) => "weaker",
            None => "unknown",
        };
        format!("then {}, now {}", word(self.then), word(self.now))
    }
}

/// One item a turn recalled, as Details shows it and `[[used: …]]` names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecalledItem {
    pub id: ItemId,
    pub place: Place,
    /// The other case's title, for [`Place::SameClient`].
    pub title: Option<String>,
    /// `None` for a balance.
    pub kind: Option<Kind>,
    /// Its main text, as the pane shows it.
    pub text: String,
    pub balance: Option<BalanceGap>,
    pub confirmed: bool,
    /// Sent to this agent session in an earlier turn, so not sent again this turn.
    pub again: bool,
    /// A ladder's positions, in the order sent; empty for every other item.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rungs: Vec<Rung>,
}

impl RecalledItem {
    /// What [`Told`] remembers an item by. A ladder is its rungs: one whose rungs changed is
    /// new text, so it is keyed by their content and not by its id alone.
    fn told_key(&self) -> String {
        if self.rungs.is_empty() {
            return self.id.to_string();
        }
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for r in &self.rungs {
            (&r.title, r.on, r.terms.as_str(), r.traded_for.as_deref(), r.landed, r.moved as u8).hash(&mut h);
        }
        format!("{}#{:x}", self.id, h.finish())
    }
}

/// What a turn recalled: for Details, and what `[[used: …]]` is checked against.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecallSent {
    pub items: Vec<RecalledItem>,
    /// Items that scored but didn't fit the budget or the caps.
    pub left_out: u32,
    /// Bytes of the JSON sent.
    pub bytes: u32,
    /// Why less than expected was recalled, when the person should know.
    pub problem: Option<String>,
    /// Every text the items sent carried to the model, field by field (terms, what was traded for,
    /// traps, guards, how, a balance's factors): what [`crate::Transcript`] checks an output row
    /// against. An item the session had already (`again`) isn't sent, so it isn't here.
    /// It keeps them apart from the entry, so this isn't shown or sent on.
    #[serde(skip)]
    pub sent_text: Vec<String>,
}

impl RecallSent {
    pub(crate) fn bytes(&self) -> usize {
        let rung = |r: &Rung| r.terms.len() + r.traded_for.as_ref().map_or(0, String::len) + r.title.as_ref().map_or(0, String::len) + 32;
        self.items
            .iter()
            .map(|i| i.text.len() + i.title.as_ref().map_or(0, String::len) + 32 + i.rungs.iter().map(rung).sum::<usize>())
            .sum::<usize>()
            + self.problem.as_ref().map_or(0, String::len)
    }

    /// Held to the pane's limits: at most [`MAX_ITEMS`] items, labels on one line.
    pub(crate) fn bounded(mut self) -> RecallSent {
        self.items.truncate(MAX_ITEMS.saturating_mul(2));
        for item in &mut self.items {
            item.text = cut(shown(std::mem::take(&mut item.text)), super::record::MAX_TEXT.saturating_mul(4));
            item.title = item.title.take().map(|t| label(&t));
            item.rungs.truncate(MAX_RUNGS);
        }
        self.problem = self.problem.map(|p| label(&p));
        self
    }
}

/// What one turn recalled. No public way to read its text: [`Recalled::into_sources`] is its one
/// exit, onto [`crate::Turn::knowledge`].
pub struct Recalled {
    sent: RecallSent,
    /// Each item's JSON, in the order sent.
    json: Vec<Value>,
}

impl Recalled {
    fn nothing(problem: String) -> Recalled {
        Recalled { sent: RecallSent { problem: Some(problem), ..RecallSent::default() }, json: Vec::new() }
    }

    pub(crate) fn sent(&self) -> RecallSent {
        let mut sent_text = Vec::new();
        for json in self.fresh() {
            strings(json, 0, &mut sent_text);
        }
        RecallSent { sent_text, ..self.sent.clone() }
    }

    /// The JSON of the items this turn carries: those not sent to this session before.
    fn fresh(&self) -> impl Iterator<Item = &Value> {
        self.json.iter().zip(&self.sent.items).filter(|(_, item)| !item.again).map(|(j, _)| j)
    }

    /// The ids of the items this turn carries, for [`Told::record`] once the prompt was taken.
    pub(crate) fn fresh_ids(&self) -> Vec<String> {
        self.sent.items.iter().filter(|i| !i.again).map(RecalledItem::told_key).collect()
    }

    /// The items not sent to this session before, as one data-grade source; none when there are
    /// none.
    pub(crate) fn into_sources(self) -> Vec<TurnSource> {
        let fresh: Vec<&Value> = self.fresh().collect();
        if fresh.is_empty() {
            return Vec::new();
        }
        vec![TurnSource {
            name: "recalled".into(),
            text: Value::Array(fresh.into_iter().cloned().collect()).to_string(),
            origin: SourceOrigin::Recalled,
        }]
    }
}

/// For the evaluation harness (`examples/case_eval.rs`): what a turn bound to `at` would carry
/// of `shelf`, by the same composer a turn uses. The harness has no room, so the [`Alone`] proof
/// can't be made there and isn't checked; nothing in the app calls this.
#[doc(hidden)]
pub fn compose_for_eval_without_room_check(shelf: &Shelf, at: &CaseRef, about: &str, today: Day) -> Result<(RecallSent, Vec<TurnSource>), CaseError> {
    let pool = Pool::load(shelf, at)?;
    let query = Query::new(about, pool.here.applies(today), today);
    let recalled = compose(&pool, &query);
    Ok((recalled.sent(), recalled.into_sources()))
}

/// Every string in `json`, as [`note_item`], [`balance_item`] and [`ladder_item`] build it: a
/// ladder's rungs are the deepest, three levels down.
fn strings(json: &Value, depth: u8, out: &mut Vec<String>) {
    match json {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) if depth < 4 => items.iter().for_each(|v| strings(v, depth.saturating_add(1), out)),
        Value::Object(fields) if depth < 4 => fields.values().for_each(|v| strings(v, depth.saturating_add(1), out)),
        _ => {}
    }
}

/// What a backend's session has been sent of the person's cases, by item id, while the session
/// stays the same ([`crate::Backend::retains`]). An ACP agent keeps what it read, so re-sending
/// 6 KiB a turn would spend the person's plan for nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Told {
    generation: Option<u64>,
    keys: HashSet<String>,
}

impl Told {
    /// Mark the items `retains`' session has had already. Nothing is remembered here: a prompt
    /// can fail or be stopped before the agent reads it, so what was sent is recorded once the
    /// turn ended well ([`Told::record`]). With `None` (ChatGPT, the fake, an agent that
    /// exited) nothing is marked: the turn carries what ranks.
    fn filter(&self, recalled: &mut Recalled, retains: Option<u64>) {
        let Some(generation) = retains else { return };
        if self.generation != Some(generation) {
            return;
        }
        for item in &mut recalled.sent.items {
            item.again = self.keys.contains(&item.told_key());
        }
    }

    /// Remember `ids` as sent to `retains`' session, read once the prompt was taken: the session
    /// that received it, not the one that ran when the turn was composed.
    pub(crate) fn record(&mut self, ids: impl IntoIterator<Item = String>, retains: Option<u64>) {
        let Some(generation) = retains else { return };
        if self.generation != Some(generation) || self.keys.len() >= MAX_TOLD {
            self.generation = Some(generation);
            self.keys.clear();
        }
        self.keys.extend(ids);
    }
}

/// Greedy by score inside [`BUDGET`], at most [`MAX_PER_NEIGHBOR`] notes from one other case and
/// [`MAX_ITEMS`] in all, nothing below [`MIN_SCORE`]. An other case's newest confirmed balance
/// rides with its first note that made the cut, so the model sees why that case stood where it did.
///
/// A topic of the query with at least [`LADDER_FROM`] `Position` notes in the pool goes as one
/// [`ladder`] item in their place: it takes the rank of its best rung, counts as one item and its
/// JSON against the budget, and its rungs count toward no neighbor's cap and bring no balance
/// item, since each rung carries its own balance. The notes it replaces are neither sent nor
/// counted as left out; rungs past [`MAX_RUNGS`] are.
fn compose(pool: &Pool, query: &Query) -> Recalled {
    let positions = |topic: Topic| {
        let on = |n: &Note| n.body.topic() == Some(topic);
        pool.here.notes.iter().filter(|n| on(n)).count()
            + pool.neighbors.iter().map(|nb| nb.case.notes.iter().filter(|n| on(n)).count()).sum::<usize>()
    };
    let laddered: Vec<Topic> = Topic::ALL.into_iter().filter(|t| query.topics.contains(*t) && positions(*t) >= LADDER_FROM).collect();
    let mut ladders_sent: Vec<Topic> = Vec::new();
    let here: Vec<Candidate<'_>> = pool.here.notes.iter().map(|note| Candidate::Note { note, case: &pool.here, place: Place::ThisCase }).collect();
    let theirs =
        pool.neighbors.iter().flat_map(|nb| nb.case.notes.iter().map(move |note| Candidate::Note { note, case: &nb.case, place: Place::SameClient }));
    let lessons = pool.lessons.iter().map(Candidate::Lesson);
    let candidates: Vec<Candidate<'_>> = here.into_iter().chain(theirs).chain(lessons).collect();
    let now = pool.here.stance_now().map(|(stance, _)| stance);
    let mut out = Recalled { sent: RecallSent::default(), json: Vec::new() };
    let mut bytes = 0usize;
    let mut per_neighbor: HashMap<&CaseId, usize> = HashMap::new();
    let mut push = |out: &mut Recalled, item: RecalledItem, json: Value| -> bool {
        let size = json.to_string().len().saturating_add(1);
        if out.sent.items.len() >= MAX_ITEMS || bytes.saturating_add(size) > BUDGET {
            return false;
        }
        bytes = bytes.saturating_add(size);
        out.sent.items.push(item);
        out.json.push(json);
        true
    };
    for ranked in rank(query, &candidates) {
        if let Some(topic) = ranked.candidate.note().and_then(|n| n.body.topic()).filter(|t| laddered.contains(t)) {
            if ladders_sent.contains(&topic) {
                continue;
            }
            ladders_sent.push(topic);
            let mut rungs = ladder(topic, pool, now);
            let all = u32::try_from(rungs.len()).unwrap_or(u32::MAX);
            rungs.truncate(MAX_RUNGS);
            let kept = u32::try_from(rungs.len()).unwrap_or(u32::MAX);
            let (item, json) = ladder_item(topic, rungs, now);
            let sent = ranked.score >= MIN_SCORE && push(&mut out, item, json);
            let left = if sent { all.saturating_sub(kept) } else { all };
            out.sent.left_out = out.sent.left_out.saturating_add(left);
            continue;
        }
        if ranked.score < MIN_SCORE {
            out.sent.left_out = out.sent.left_out.saturating_add(1);
            continue;
        }
        let (note, case) = match ranked.candidate {
            Candidate::Note { note, case, .. } => (note, case),
            Candidate::Lesson(lesson) => {
                let (item, json) = lesson_item(lesson, now);
                if !push(&mut out, item, json) {
                    out.sent.left_out = out.sent.left_out.saturating_add(1);
                }
                continue;
            }
        };
        let neighbor = pool.neighbors.iter().find(|nb| std::ptr::eq(&nb.case, case));
        if let Some(nb) = neighbor
            && per_neighbor.get(&nb.id).copied().unwrap_or(0) >= MAX_PER_NEIGHBOR
        {
            out.sent.left_out = out.sent.left_out.saturating_add(1);
            continue;
        }
        let (item, json) = note_item(note, case, neighbor, now);
        if !push(&mut out, item, json) {
            out.sent.left_out = out.sent.left_out.saturating_add(1);
            continue;
        }
        if let Some(nb) = neighbor {
            let first = !per_neighbor.contains_key(&nb.id);
            *per_neighbor.entry(&nb.id).or_default() += 1;
            if first && let Some(balance) = nb.case.balance.last() {
                let (item, json) = balance_item(nb, balance, now);
                push(&mut out, item, json);
            }
        }
    }
    out.sent.bytes = u32::try_from(bytes).unwrap_or(u32::MAX);
    out
}

fn place_words(neighbor: Option<&Neighbor>) -> (Place, Option<String>, String) {
    match neighbor {
        None => (Place::ThisCase, None, "this case".to_string()),
        Some(nb) => {
            let title = label(nb.case.title.for_person());
            let words = place_line(Some(&title));
            (Place::SameClient, Some(title), words)
        }
    }
}

/// The place as the model reads it: this case, or the same client's case by its title.
fn place_line(title: Option<&String>) -> String {
    match title {
        None => "this case".to_string(),
        Some(title) => format!("the same client's case {}", serde_json::to_string(title).unwrap_or_default()),
    }
}

/// The facts a model may see with an item: the closed fields of the case's facts, never a name.
fn facts_json(json: &mut serde_json::Map<String, Value>, applies: &Applies) {
    let mut put = |key: &str, value: Option<Value>| {
        if let Some(v) = value {
            json.insert(key.to_string(), v);
        }
    };
    put("contract", applies.kind.and_then(|k| serde_json::to_value(k).ok()));
    put("side", applies.side.and_then(|k| serde_json::to_value(k).ok()));
    put("paper", applies.paper.and_then(|k| serde_json::to_value(k).ok()));
    put("counterparty", applies.theirs.and_then(|k| serde_json::to_value(k).ok()));
    put("law", applies.law.map(|l| Value::String(String::from(l))));
}

/// A lesson as the model sees it: its text, its coarse facts, the balance it was kept under
/// against now. No case, no client, no name: it is the one item that crosses clients.
fn lesson_item(lesson: &KeptLesson, now: Option<Stance>) -> (RecalledItem, Value) {
    let id = ItemId::Lesson { lesson: lesson.id };
    let gap = BalanceGap { then: lesson.applies.stance, now };
    let text = lesson.text.for_person().to_string();
    let mut json = serde_json::Map::new();
    json.insert("id".into(), Value::String(id.to_string()));
    json.insert("place".into(), Value::String("a lesson the person kept".into()));
    json.insert("kind".into(), Value::String("lesson".into()));
    json.insert("text".into(), Value::String(text.clone()));
    if let Some(year) = lesson.applies.year {
        json.insert("year".into(), Value::from(year));
    }
    facts_json(&mut json, &lesson.applies);
    json.insert("balance".into(), Value::String(gap.words()));
    json.insert("confirmed".into(), Value::Bool(true));
    let item = RecalledItem {
        id,
        place: Place::Lesson,
        title: None,
        kind: Some(Kind::Lesson),
        text,
        balance: Some(gap),
        confirmed: true,
        again: false,
        rungs: Vec::new(),
    };
    (item, Value::Object(json))
}

/// A topic's ladder as the model sees it: its rungs in order, each with its place, date, mover,
/// terms, trade, whether it landed, the balance then against now, and its standing.
fn ladder_item(topic: Topic, rungs: Vec<Rung>, now: Option<Stance>) -> (RecalledItem, Value) {
    let id = ItemId::Ladder { topic };
    let landed = rungs.iter().filter(|r| r.landed).count();
    let confirmed = rungs.iter().all(|r| r.confirmed);
    let text = format!("{}: {} positions, {landed} landed", topic_name(topic), rungs.len());
    let rows: Vec<Value> = rungs
        .iter()
        .map(|r| {
            let mut j = json!({
                "place": place_line(r.title.as_ref()),
                "on": String::from(r.on),
                "moved": r.moved,
                "terms": r.terms,
                "landed": r.landed,
                "balance": r.balance.words(),
                "confirmed": r.confirmed,
            });
            if let Some(t) = &r.traded_for {
                j["tradedFor"] = Value::String(t.clone());
            }
            j
        })
        .collect();
    let json = json!({
        "id": id.to_string(),
        "place": "this case and the same client's cases",
        "kind": "ladder",
        "topic": topic_name(topic),
        "text": text,
        "rungs": rows,
        "balance": BalanceGap { then: None, now }.words(),
        "confirmed": confirmed,
    });
    let item = RecalledItem { id, place: Place::ThisCase, title: None, kind: None, text, balance: None, confirmed, again: false, rungs };
    (item, json)
}

fn note_item(note: &Note, case: &Case, neighbor: Option<&Neighbor>, now: Option<Stance>) -> (RecalledItem, Value) {
    let id = match neighbor {
        None => ItemId::Here { note: note.id },
        Some(nb) => ItemId::Neighbor { case: nb.id.clone(), note: note.id },
    };
    let (place, title, place_words) = place_words(neighbor);
    let then = case.stance_on(note.at);
    let gap = BalanceGap { then, now };
    let confirmed = matches!(note.standing, Standing::Confirmed { .. });
    let text = note.body.texts().next().map_or(String::new(), |q| q.for_person().to_string());
    let mut json = serde_json::Map::new();
    json.insert("id".into(), Value::String(id.to_string()));
    json.insert("place".into(), Value::String(place_words));
    json.insert("kind".into(), serde_json::to_value(note.kind()).unwrap_or(Value::Null));
    json.insert("text".into(), Value::String(text.clone()));
    let more = |q: &Option<Quoted>| q.as_ref().map(|q| Value::String(q.for_person().to_string()));
    match &note.body {
        Body::Pattern { traps, .. } | Body::Lesson { traps, .. } => {
            if let Some(t) = more(traps) {
                json.insert("traps".into(), t);
            }
        }
        Body::Pitfall { guard, .. } => {
            if let Some(g) = more(guard) {
                json.insert("guard".into(), g);
            }
        }
        Body::Leverage { favors, until, .. } => {
            json.insert("favors".into(), serde_json::to_value(favors).unwrap_or(Value::Null));
            if let Some(u) = until {
                json.insert("until".into(), Value::String(String::from(*u)));
            }
        }
        Body::Position { topic, moved, traded_for, landed, .. } => {
            json.insert("topic".into(), serde_json::to_value(topic).unwrap_or(Value::Null));
            json.insert("moved".into(), serde_json::to_value(moved).unwrap_or(Value::Null));
            json.insert("landed".into(), Value::Bool(*landed));
            if let Some(t) = more(traded_for) {
                json.insert("tradedFor".into(), t);
            }
        }
    }
    if let Some(how) = more(&note.how) {
        json.insert("how".into(), how);
    }
    json.insert("on".into(), Value::String(String::from(note.at)));
    facts_json(&mut json, &case.applies(note.at));
    json.insert("balance".into(), Value::String(gap.words()));
    json.insert("confirmed".into(), Value::Bool(confirmed));
    let item = RecalledItem { id, place, title, kind: Some(note.kind()), text, balance: Some(gap), confirmed, again: false, rungs: Vec::new() };
    (item, Value::Object(json))
}

fn balance_item(nb: &Neighbor, balance: &super::record::Balance, now: Option<Stance>) -> (RecalledItem, Value) {
    let id = ItemId::NeighborBalance { case: nb.id.clone() };
    let (place, title, place_words) = place_words(Some(nb));
    let gap = BalanceGap { then: Some(balance.stance), now };
    let factors: Vec<Value> = balance
        .factors
        .iter()
        .map(|f| {
            let mut j = json!({"kind": f.kind, "favors": f.favors});
            if let Some(why) = &f.why {
                j["why"] = Value::String(why.for_person().to_string());
            }
            j
        })
        .collect();
    let text = format!("that case's balance on {}: {}", String::from(balance.on), gap.words());
    let mut json = serde_json::Map::new();
    json.insert("id".into(), Value::String(id.to_string()));
    json.insert("place".into(), Value::String(place_words));
    json.insert("kind".into(), Value::String("balance".into()));
    json.insert("stance".into(), serde_json::to_value(balance.stance).unwrap_or(Value::Null));
    json.insert("factors".into(), Value::Array(factors));
    json.insert("on".into(), Value::String(String::from(balance.on)));
    facts_json(&mut json, &nb.case.applies(balance.on));
    json.insert("balance".into(), Value::String(gap.words()));
    json.insert("confirmed".into(), Value::Bool(true));
    let item = RecalledItem { id, place, title, kind: None, text, balance: Some(gap), confirmed: true, again: false, rungs: Vec::new() };
    (item, Value::Object(json))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cases::record::{Balance, NoteId, Side, Topic};

    fn day(s: &str) -> Day {
        Day::parse(s).unwrap()
    }

    fn case(title: &str, notes: usize, standing: Standing) -> Case {
        let mut case = Case::new(title);
        for n in 0..notes {
            case.notes.push(Note {
                id: NoteId::parse(&format!("n{}", n + 1)).unwrap(),
                at: day("2026-01-01"),
                body: Body::Pattern { rule: Quoted::parse(&format!("Liability cap pattern {n} for the renewal.")), traps: None },
                how: None,
                standing,
                stated_topics: Topics::default(),
                used: 0,
                last_used: None,
            });
        }
        case.balance.push(Balance { on: day("2026-01-01"), stance: Stance::Weaker, factors: Vec::new(), standing });
        case
    }

    fn id(n: u8) -> CaseId {
        CaseId::parse(&format!("{n:016x}")).unwrap()
    }

    /// What the loader reads from the lessons file on disk: the lessons that are on, and none at
    /// all from a file that parsed but had to be cut to the limits, since a damaged file is
    /// withheld whole rather than sent in part.
    #[test]
    fn the_pool_reads_only_lessons_that_are_on_and_none_from_a_damaged_file() {
        use crate::cases::lessons::{FILE, LessonDraft, LessonOp};
        use crate::cases::record::{Click, MAX_TEXT, VERSION};
        let dir = std::env::temp_dir().join(format!("wordcraft-recall-{}-pool-lessons", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let shelf = Shelf::at(&dir);
        let click = || Click::by_person(true).unwrap();
        let wall = shelf.new_wall(click(), "Acme", "2026-11-10", 1).unwrap();
        let at = shelf.new_case(click(), &wall, "Acme MSA", "2026-11-10", 2).unwrap();
        let keep = |text: &str| {
            let draft = LessonDraft { text: Quoted::parse(text), applies: Applies::default(), topics: Topics::default(), from: None };
            shelf.lesson(click(), LessonOp::Keep(draft)).unwrap().unwrap()
        };
        keep("On: ask for the SLA credit before the cap.");
        let off = keep("Off: the cap moves last.");
        shelf.lesson(click(), LessonOp::Off(off, true)).unwrap();
        let pool = Pool::load(&shelf, &at).unwrap();
        let texts: Vec<&str> = pool.lessons.iter().map(|l| l.text.raw()).collect();
        assert_eq!(texts, ["On: ask for the SLA credit before the cap."]);
        assert!(!pool.lessons_damaged);
        let long = "x".repeat(MAX_TEXT + 1);
        let cut =
            format!(r#"{{"version":{VERSION},"lessons":[{{"id":"l1","text":"Parsed, kept whole."}},{{"id":"l2","text":"{long}"}}],"lastId":2}}"#);
        std::fs::write(dir.join(FILE), cut).unwrap();
        assert!(!shelf.lessons().lessons.is_empty(), "the file parsed: its lessons are there, flagged damaged");
        let pool = Pool::load(&shelf, &at).unwrap();
        assert!(pool.lessons.is_empty() && pool.lessons_damaged, "{:?}", pool.lessons);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A kept lesson is an item of its own kind, with its text, its coarse facts and the balance
    /// gap, and nothing that names the case it came from; one that is off isn't read, and a
    /// damaged lessons file sends none and says so.
    #[test]
    fn a_kept_lesson_is_an_item_with_no_case_and_a_damaged_file_sends_none() {
        use crate::cases::lessons::{LessonDraft, LessonOp, Lessons};
        use crate::cases::record::{ContractKind, Law};
        let mut kept = Lessons::default();
        let from = Some(id(7));
        let applies =
            Applies { kind: Some(ContractKind::Saas), law: Law::parse("JP"), stance: Some(Stance::Stronger), year: Some(2025), ..Applies::default() };
        let draft = |text: &str| LessonDraft { text: Quoted::parse(text), applies, topics: Topics::default(), from: from.clone() };
        kept.apply(LessonOp::Keep(draft("Ask for the SLA credit before the liability cap."))).unwrap();
        let off = kept.apply(LessonOp::Keep(draft("Off: the liability cap moves last."))).unwrap().unwrap();
        kept.apply(LessonOp::Off(off, true)).unwrap();
        let mut here = Case::new("here");
        here.balance.push(Balance {
            on: day("2026-01-01"),
            stance: Stance::Weaker,
            factors: Vec::new(),
            standing: Standing::Confirmed { on: day("2026-01-01") },
        });
        let pool = Pool { here, neighbors: Vec::new(), damaged: false, skipped: 0, lessons: kept.on().cloned().collect(), lessons_damaged: false };
        let query = Query::new("the liability cap", pool.here.applies(day("2026-11-10")), day("2026-11-10"));
        let out = compose(&pool, &query);
        assert_eq!(out.sent.items.len(), 1, "{:?}", out.sent.items);
        let item = &out.sent.items[0];
        assert_eq!((item.id.to_string(), item.place), ("l1".to_string(), Place::Lesson));
        assert_eq!(item.balance, Some(BalanceGap { then: Some(Stance::Stronger), now: Some(Stance::Weaker) }));
        let json = out.json[0].to_string();
        assert!(
            json.contains(r#""place":"a lesson the person kept""#) && json.contains(r#""contract":"saas""#) && json.contains(r#""year":2025"#),
            "{json}"
        );
        assert!(!json.contains("from") && !json.contains(&id(7).as_str().to_string()) && !json.contains("size"), "{json}");
        assert!(pool.problem().is_none());
        let damaged = Pool { lessons: Vec::new(), lessons_damaged: true, ..pool };
        assert!(compose(&damaged, &query).sent.items.is_empty());
        assert_eq!(damaged.problem().as_deref(), Some("the lessons file is damaged, so no lesson was recalled"));
    }

    #[test]
    fn a_neighbor_gives_at_most_three_notes_and_its_balance_and_the_rest_is_counted() {
        let confirmed = Standing::Confirmed { on: day("2026-01-01") };
        let pool = Pool {
            here: Case::new("here"),
            neighbors: (1..=3).map(|n| Neighbor::of(id(n), case(&format!("case {n}"), 10, confirmed))).collect(),
            damaged: false,
            skipped: 0,
            lessons: Vec::new(),
            lessons_damaged: false,
        };
        let query = Query::new("the liability cap", pool.here.applies(day("2026-11-10")), day("2026-11-10"));
        let out = compose(&pool, &query);
        assert_eq!(out.sent.items.len(), 12);
        for n in 1..=3 {
            let notes = out.sent.items.iter().filter(|i| matches!(&i.id, ItemId::Neighbor { case, .. } if *case == id(n))).count();
            let balances = out.sent.items.iter().filter(|i| matches!(&i.id, ItemId::NeighborBalance { case } if *case == id(n))).count();
            assert_eq!((notes, balances), (3, 1), "case {n}");
        }
        assert_eq!(out.sent.left_out, 30 - 9);
        assert!(out.sent.bytes as usize <= BUDGET);
        assert_eq!(out.into_sources().len(), 1);
    }

    /// What the transcript checks a change against is every field the model got, not the one the
    /// pane shows.
    #[test]
    fn what_was_sent_carries_every_field_of_an_item() {
        let confirmed = Standing::Confirmed { on: day("2026-01-01") };
        let mut here = Case::new("here");
        here.notes.push(Note {
            id: NoteId::parse("n1").unwrap(),
            at: day("2026-01-01"),
            body: Body::Position {
                topic: Topic::Liability,
                moved: Side::Us,
                terms: Quoted::parse("Liability capped at twelve months of fees paid."),
                traded_for: Some(Quoted::parse("A two-year initial term with no termination for convenience.")),
                landed: true,
            },
            how: Some(Quoted::parse("Offered late on the last call.")),
            standing: confirmed,
            stated_topics: Topics::default(),
            used: 0,
            last_used: None,
        });
        let pool = Pool { here, neighbors: Vec::new(), damaged: false, skipped: 0, lessons: Vec::new(), lessons_damaged: false };
        let query = Query::new("the liability cap", pool.here.applies(day("2026-11-10")), day("2026-11-10"));
        let sent = compose(&pool, &query).sent();
        assert_eq!(sent.items.len(), 1);
        for field in [
            "Liability capped at twelve months of fees paid.",
            "A two-year initial term with no termination for convenience.",
            "Offered late on the last call.",
        ] {
            assert!(sent.sent_text.iter().any(|t| t == field), "{field}: {:?}", sent.sent_text);
        }
    }

    #[test]
    fn a_neighbors_unconfirmed_notes_and_balances_are_not_in_the_pool() {
        let nb = Neighbor::of(id(1), case("case", 4, Standing::Unconfirmed));
        assert!(nb.case.notes.is_empty() && nb.case.balance.is_empty());
    }

    /// `Alone` was true on the frame, and someone joined before the conversation's thread ran the
    /// recall: it checks again there, sends nothing, and says who.
    #[test]
    fn someone_joining_between_the_frame_and_the_turn_gets_no_recalled_text() {
        use crate::cases::record::{Click, NoteDraft, PersonOp};
        use crate::{Fake, Options};
        use wordcraft_engine::room::{Role, Room};
        let dir = std::env::temp_dir().join(format!("wordcraft-recall-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let shelf = Shelf::at(&dir);
        let click = Click::by_person(true).unwrap();
        let wall = shelf.new_wall(click, "Acme", "2026-11-10", 1).unwrap();
        let at = shelf.new_case(click, &wall, "Acme MSA", "2026-11-10", 2).unwrap();
        let draft = NoteDraft {
            body: Body::Pattern { rule: Quoted::parse("Cap liability at twelve months."), traps: None },
            how: None,
            topics: Topics::default(),
        };
        shelf.person(click, &at, PersonOp::AddNote(draft), day("2026-11-10")).unwrap();
        let mut session = wordcraft_engine::Session::new(wordcraft_doc::Document::from_text("one"));
        let (room, own) = Room::host(&mut session, "Minoru").unwrap();
        let fake = Fake::new();
        let seen = fake.seen();
        let mut a = crate::Assistant::start(&room, Box::new(fake), Options::default()).unwrap();
        let alone = Alone::check(&room, own.id(), a.name()).unwrap();
        a.set_recall(Some(RecallAsk::new(shelf, at, "the liability cap", day("2026-11-10"), alone)));
        let _key = room.invite("Bob", Role::Viewer).unwrap();
        a.ask("What about the liability cap?", None).unwrap();
        assert!(a.wait_idle(std::time::Duration::from_secs(30)));
        let turns = seen.lock().unwrap().turns.clone();
        assert!(turns[0].knowledge.is_empty(), "{:?}", turns[0].knowledge);
        let problem = a.transcript().entries().find_map(|e| if let crate::Entry::Recalled(sent) = e { sent.problem.clone() } else { None });
        assert_eq!(problem.as_deref(), Some("Bob can read this document"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Someone joins while the recall reads its files: the proof is checked again after reading,
    /// so nothing of what was read reaches the turn.
    #[test]
    fn someone_joining_while_the_recall_reads_its_files_gets_no_recalled_text() {
        use crate::cases::record::{Click, NoteDraft, PersonOp};
        use crate::{Fake, Options};
        use wordcraft_engine::room::{Role, Room};
        let dir = std::env::temp_dir().join(format!("wordcraft-recall-reading-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let shelf = Shelf::at(&dir);
        let click = Click::by_person(true).unwrap();
        let wall = shelf.new_wall(click, "Acme", "2026-11-10", 1).unwrap();
        let at = shelf.new_case(click, &wall, "Acme MSA", "2026-11-10", 2).unwrap();
        let draft = NoteDraft {
            body: Body::Pattern { rule: Quoted::parse("Cap liability at twelve months."), traps: None },
            how: None,
            topics: Topics::default(),
        };
        shelf.person(click, &at, PersonOp::AddNote(draft), day("2026-11-10")).unwrap();
        let mut session = wordcraft_engine::Session::new(wordcraft_doc::Document::from_text("one"));
        let (room, own) = Room::host(&mut session, "Minoru").unwrap();
        let fake = Fake::new();
        let seen = fake.seen();
        let mut a = crate::Assistant::start(&room, Box::new(fake), Options::default()).unwrap();
        let alone = Alone::check(&room, own.id(), a.name()).unwrap();
        let joining = room.clone();
        let ask = RecallAsk::new(shelf, at, "the liability cap", day("2026-11-10"), alone).after_reading(move || {
            joining.invite("Bob", Role::Viewer).unwrap();
        });
        a.set_recall(Some(ask));
        a.ask("What about the liability cap?", None).unwrap();
        assert!(a.wait_idle(std::time::Duration::from_secs(30)));
        let turns = seen.lock().unwrap().turns.clone();
        assert!(turns[0].knowledge.is_empty(), "{:?}", turns[0].knowledge);
        let problem = a.transcript().entries().find_map(|e| if let crate::Entry::Recalled(sent) = e { sent.problem.clone() } else { None });
        assert_eq!(problem.as_deref(), Some("Bob can read this document"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An item a retaining session has already, marked `again`, isn't sent, so a change pasting it
    /// isn't a quote of this turn; what is sent is indexed.
    #[test]
    fn only_the_items_sent_are_indexed_for_the_quote_check() {
        let confirmed = Standing::Confirmed { on: day("2026-01-01") };
        let pool =
            Pool { here: case("here", 1, confirmed), neighbors: Vec::new(), damaged: false, skipped: 0, lessons: Vec::new(), lessons_damaged: false };
        let query = Query::new("the liability cap", pool.here.applies(day("2026-11-10")), day("2026-11-10"));
        let rule = "Liability cap pattern 0 for the renewal.";
        let mut told = Told::default();
        let first = compose(&pool, &query);
        told.record(first.fresh_ids(), Some(0));
        let mut again = compose(&pool, &query);
        told.filter(&mut again, Some(0));
        assert!(again.sent.items.iter().all(|i| i.again));
        let mut t = crate::Transcript::default();
        t.record(&crate::Event::Recalled(again.sent()));
        assert!(!t.quotes_recalled_for_test(rule), "nothing was sent, so nothing is indexed");
        t.record(&crate::Event::Recalled(first.sent()));
        assert!(t.quotes_recalled_for_test(rule));
    }

    /// The set of recalled windows has a cap and nothing leaves it: a recall that would pass the
    /// cap is withheld, the turn says so, and what was recalled before is checked still.
    #[test]
    fn a_recall_past_what_can_be_checked_is_withheld_and_earlier_ones_stay_checked() {
        use crate::cases::record::{Click, NoteDraft, PersonOp};
        use crate::{Fake, Options};
        use wordcraft_engine::room::Room;
        let dir = std::env::temp_dir().join(format!("wordcraft-recall-cap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let shelf = Shelf::at(&dir);
        let click = Click::by_person(true).unwrap();
        let wall = shelf.new_wall(click, "Acme", "2026-11-10", 1).unwrap();
        let at = shelf.new_case(click, &wall, "Acme MSA", "2026-11-10", 2).unwrap();
        let rule = "Cap liability at twelve months of the fees paid in the year before the claim was first made.";
        let draft = NoteDraft { body: Body::Pattern { rule: Quoted::parse(rule), traps: None }, how: None, topics: Topics::default() };
        shelf.person(click, &at, PersonOp::AddNote(draft), day("2026-11-10")).unwrap();
        let mut session = wordcraft_engine::Session::new(wordcraft_doc::Document::from_text("one"));
        let (room, own) = Room::host(&mut session, "Minoru").unwrap();
        let fake = Fake::new();
        let seen = fake.seen();
        let mut a = crate::Assistant::start(&room, Box::new(fake), Options::default()).unwrap();
        // 384 KB of six-digit counters, each window distinct: the set is full but for a few windows.
        let filler: String = (0..65_536u32).map(|n| format!("{n:06x}")).collect();
        let item = RecalledItem {
            id: ItemId::Here { note: NoteId::parse("n9").unwrap() },
            place: Place::ThisCase,
            title: None,
            kind: None,
            text: "filler".into(),
            balance: None,
            confirmed: true,
            again: false,
            rungs: Vec::new(),
        };
        a.transcript_mut().record(&crate::Event::Recalled(RecallSent {
            items: vec![item],
            sent_text: vec![filler.clone()],
            ..RecallSent::default()
        }));
        assert!(a.transcript().recall_room() < 40, "{}", a.transcript().recall_room());
        let alone = Alone::check(&room, own.id(), a.name()).unwrap();
        a.set_recall(Some(RecallAsk::new(shelf, at, "the liability cap", day("2026-11-10"), alone)));
        a.ask("What about the liability cap?", None).unwrap();
        assert!(a.wait_idle(std::time::Duration::from_secs(30)));
        a.poll();
        let turns = seen.lock().unwrap().turns.clone();
        assert!(turns[0].knowledge.is_empty(), "{:?}", turns[0].knowledge);
        let problem = a.transcript().entries().rev().find_map(|e| if let crate::Entry::Recalled(sent) = e { sent.problem.clone() } else { None });
        assert_eq!(problem.as_deref(), Some(crate::transcript::RECALL_WITHHELD));
        assert!(!a.transcript().quotes_recalled_for_test(rule), "the withheld text is not indexed either");
        assert!(a.transcript().quotes_recalled_for_test(&filler[..40]), "the first recall is checked still");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn position(n: u8, on: &str, terms: &str, landed: bool, standing: Standing) -> Note {
        Note {
            id: NoteId::parse(&format!("n{n}")).unwrap(),
            at: day(on),
            body: Body::Position {
                topic: Topic::Liability,
                moved: Side::Us,
                terms: Quoted::parse(terms),
                traded_for: Some(Quoted::parse(&format!("Trade {n}: a longer initial term with no termination for convenience."))),
                landed,
            },
            how: None,
            standing,
            stated_topics: Topics::default(),
            used: 0,
            last_used: None,
        }
    }

    fn confirmed() -> Standing {
        Standing::Confirmed { on: day("2026-01-01") }
    }

    fn balance(on: &str, stance: Stance) -> Balance {
        Balance { on: day(on), stance, factors: Vec::new(), standing: confirmed() }
    }

    /// Three positions on the liability cap, two of them a neighbor's, and a pattern on the same
    /// topic that is no rung: one ladder item carries the positions, the pattern goes as before.
    fn ladder_pool() -> Pool {
        let mut here = Case::new("here");
        here.balance.push(balance("2026-01-01", Stance::Weaker));
        here.notes.push(position(1, "2026-03-01", "Cap at twelve months of fees paid, here.", false, Standing::Unconfirmed));
        here.notes.push(Note {
            id: NoteId::parse("n2").unwrap(),
            at: day("2026-03-01"),
            body: Body::Pattern { rule: Quoted::parse("The liability cap moves last in every renewal."), traps: None },
            how: None,
            standing: confirmed(),
            stated_topics: Topics::default(),
            used: 0,
            last_used: None,
        });
        let mut nb = Case::new("Kestrel Freight renewal");
        nb.balance.push(balance("2025-01-01", Stance::Stronger));
        nb.balance.push(balance("2025-06-01", Stance::Weaker));
        nb.notes.push(position(1, "2025-02-01", "Cap at fees paid in the prior year, stronger then.", true, confirmed()));
        nb.notes.push(position(2, "2025-07-01", "Cap at six months of fees, weaker then.", true, confirmed()));
        Pool { here, neighbors: vec![Neighbor::of(id(1), nb)], damaged: false, skipped: 0, lessons: Vec::new(), lessons_damaged: false }
    }

    fn liability_query(pool: &Pool) -> Query {
        Query::new("the liability cap", pool.here.applies(day("2026-11-10")), day("2026-11-10"))
    }

    /// The ladder is a read of `Position` notes alone, in the order the question wants: the rung
    /// under the balance closest to now first, then landed, then newest. Each rung says where it
    /// is from, the balance then against now, and whether the person confirmed it.
    #[test]
    fn ladder_derives_from_position_notes_alone_closest_balance_first() {
        let pool = ladder_pool();
        let rungs = ladder(Topic::Liability, &pool, Some(Stance::Weaker));
        let terms: Vec<&str> = rungs.iter().map(|r| r.terms.as_str()).collect();
        assert_eq!(
            terms,
            [
                "Cap at six months of fees, weaker then.",
                "Cap at twelve months of fees paid, here.",
                "Cap at fees paid in the prior year, stronger then."
            ],
            "{rungs:?}"
        );
        assert_eq!((rungs[0].place, rungs[0].title.as_deref(), rungs[0].confirmed), (Place::SameClient, Some("Kestrel Freight renewal"), true));
        assert_eq!((rungs[1].place, rungs[1].title.as_deref(), rungs[1].confirmed), (Place::ThisCase, None, false));
        assert_eq!(rungs[2].balance, BalanceGap { then: Some(Stance::Stronger), now: Some(Stance::Weaker) });
        assert!(rungs.iter().all(|r| !r.terms.contains("moves last")), "a pattern on the topic is no rung");
        assert!(ladder(Topic::Indemnity, &pool, None).is_empty());
    }

    /// Under three positions on the topic the notes go as they did; from three, one ladder item
    /// replaces them, counted once, with every rung's balance gap, and the pattern still goes.
    #[test]
    fn ladder_item_replaces_three_or_more_positions_on_the_topic() {
        let pool = ladder_pool();
        let out = compose(&pool, &liability_query(&pool));
        let mut ids: Vec<String> = out.sent.items.iter().map(|i| i.id.to_string()).collect();
        ids.sort();
        assert_eq!(ids, ["ladder:liability", "n2"], "{:?}", out.sent.items);
        let at = out.sent.items.iter().position(|i| i.id == ItemId::Ladder { topic: Topic::Liability }).unwrap();
        let item = &out.sent.items[at];
        assert_eq!(item.rungs.len(), 3);
        assert_eq!(out.sent.left_out, 0);
        let json = &out.json[at];
        assert_eq!(json["kind"], "ladder");
        assert_eq!(json["topic"], "liability");
        assert_eq!(json["rungs"].as_array().map(Vec::len), Some(3));
        assert_eq!(json["rungs"][0]["balance"], "then weaker, now weaker");
        assert_eq!(json["rungs"][2]["balance"], "then stronger, now weaker");
        assert_eq!(json["rungs"][1]["confirmed"], false);
        assert_eq!(json["rungs"][0]["place"], "the same client's case \"Kestrel Freight renewal\"");
        assert_eq!(json["rungs"][0]["tradedFor"], "Trade 2: a longer initial term with no termination for convenience.");
        let mut fewer = ladder_pool();
        fewer.neighbors[0].case.notes.pop();
        let out = compose(&fewer, &liability_query(&fewer));
        let mut ids: Vec<String> = out.sent.items.iter().map(|i| i.id.to_string()).collect();
        ids.sort();
        assert_eq!(ids, ["n1", "n2"], "{:?}", out.sent.items);
        assert_eq!(out.sent.left_out, 1, "the rung won while stronger scores under MIN_SCORE on its own; a ladder would have carried it");
    }

    /// The ladder is computed from the files each turn and stored nowhere: deleting the neighbor
    /// takes its rungs with it, with no other write.
    #[test]
    fn ladder_loses_a_deleted_neighbors_rungs_with_no_other_write() {
        use crate::cases::record::{Click, NoteDraft, PersonOp};
        let dir = std::env::temp_dir().join(format!("wordcraft-ladder-delete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let shelf = Shelf::at(&dir);
        let click = || Click::by_person(true).unwrap();
        let wall = shelf.new_wall(click(), "Kestrel Freight KK", "2026-11-10", 1).unwrap();
        let here = shelf.new_case(click(), &wall, "Kestrel MSA", "2026-11-10", 2).unwrap();
        let nb = shelf.new_case(click(), &wall, "Kestrel renewal", "2026-11-10", 3).unwrap();
        let add = |at: &CaseRef, terms: &str| {
            let body = Body::Position { topic: Topic::Liability, moved: Side::Us, terms: Quoted::parse(terms), traded_for: None, landed: true };
            shelf.person(click(), at, PersonOp::AddNote(NoteDraft { body, how: None, topics: Topics::default() }), day("2026-11-10")).unwrap();
        };
        add(&here, "Cap at twelve months of fees, here.");
        add(&nb, "Cap at six months of fees, there.");
        add(&nb, "Cap at nine months of fees, there.");
        let query = |pool: &Pool| Query::new("the liability cap", pool.here.applies(day("2026-11-10")), day("2026-11-10"));
        let pool = Pool::load(&shelf, &here).unwrap();
        let out = compose(&pool, &query(&pool));
        assert_eq!(out.sent.items.iter().map(|i| i.rungs.len()).collect::<Vec<_>>(), [3]);
        shelf.delete_case(click(), &nb, true).unwrap();
        let pool = Pool::load(&shelf, &here).unwrap();
        let out = compose(&pool, &query(&pool));
        let ids: Vec<String> = out.sent.items.iter().map(|i| i.id.to_string()).collect();
        assert_eq!(ids, ["n1"]);
        assert!(out.sent.items[0].rungs.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A model's note in another case never leaves it: read through the shelf, an unconfirmed
    /// position in a neighbor is no rung, and its text is nowhere in what is sent.
    #[test]
    fn ladder_never_carries_a_neighbors_unconfirmed_position() {
        use crate::cases::record::{Click, ModelWrite, NoteDraft, PersonOp};
        let dir = std::env::temp_dir().join(format!("wordcraft-ladder-unconfirmed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let shelf = Shelf::at(&dir);
        let click = || Click::by_person(true).unwrap();
        let wall = shelf.new_wall(click(), "Bluefin Systems Ltd", "2026-11-10", 1).unwrap();
        let here = shelf.new_case(click(), &wall, "Bluefin MSA", "2026-11-10", 2).unwrap();
        let nb = shelf.new_case(click(), &wall, "Bluefin renewal", "2026-11-10", 3).unwrap();
        let draft = |terms: &str| {
            let body = Body::Position { topic: Topic::Liability, moved: Side::Us, terms: Quoted::parse(terms), traded_for: None, landed: true };
            NoteDraft { body, how: None, topics: Topics::default() }
        };
        for terms in ["Cap at twelve months of fees, here.", "Cap at nine months of fees, here too.", "Cap at six months of fees, here again."] {
            shelf.person(click(), &here, PersonOp::AddNote(draft(terms)), day("2026-11-10")).unwrap();
        }
        const CANARY: &str = "CANARY unlimited liability accepted in the renewal.";
        shelf.model(&nb, ModelWrite::Note(draft(CANARY)), day("2026-11-10")).unwrap();
        let pool = Pool::load(&shelf, &here).unwrap();
        let query = Query::new("the liability cap", pool.here.applies(day("2026-11-10")), day("2026-11-10"));
        let recalled = compose(&pool, &query);
        assert_eq!(recalled.sent.items[0].rungs.len(), 3);
        let sources = recalled.into_sources();
        assert!(!sources[0].as_text().contains("CANARY"), "{}", sources[0].as_text());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every rung's terms and trade are indexed, so a change pasting one is flagged as a quote
    /// like a pasted note.
    #[test]
    fn ladder_rungs_are_indexed_for_the_quote_check() {
        let pool = ladder_pool();
        let sent = compose(&pool, &liability_query(&pool)).sent();
        let mut t = crate::Transcript::default();
        t.record(&crate::Event::Recalled(sent));
        assert!(t.quotes_recalled_for_test("Cap at fees paid in the prior year, stronger then."));
        assert!(t.quotes_recalled_for_test("Trade 2: a longer initial term with no termination for convenience."));
    }

    /// A retaining session told a ladder isn't told the same ladder again, but is told one whose
    /// rungs changed: a new position is new text.
    #[test]
    fn ladder_with_changed_rungs_is_new_to_a_session_that_was_told_it() {
        let pool = ladder_pool();
        let mut told = Told::default();
        let first = compose(&pool, &liability_query(&pool));
        told.record(first.fresh_ids(), Some(0));
        let is_ladder = |i: &&RecalledItem| matches!(i.id, ItemId::Ladder { .. });
        let mut same = compose(&pool, &liability_query(&pool));
        told.filter(&mut same, Some(0));
        assert!(same.sent.items.iter().find(is_ladder).unwrap().again);
        let mut grown = ladder_pool();
        grown.here.notes.push(position(3, "2026-10-01", "Cap at eighteen months of fees, new.", false, confirmed()));
        let mut changed = compose(&grown, &liability_query(&grown));
        told.filter(&mut changed, Some(0));
        let ladder = changed.sent.items.iter().find(is_ladder).unwrap();
        assert!(!ladder.again, "{ladder:?}");
    }

    #[test]
    fn a_session_that_retains_is_told_each_item_once_and_a_new_session_again() {
        let confirmed = Standing::Confirmed { on: day("2026-01-01") };
        let pool =
            Pool { here: case("here", 2, confirmed), neighbors: Vec::new(), damaged: false, skipped: 0, lessons: Vec::new(), lessons_damaged: false };
        let query = Query::new("the liability cap", pool.here.applies(day("2026-11-10")), day("2026-11-10"));
        let mut told = Told::default();
        let mut first = compose(&pool, &query);
        told.filter(&mut first, Some(0));
        assert!(first.sent.items.iter().all(|i| !i.again));
        let mut lost = compose(&pool, &query);
        told.filter(&mut lost, Some(0));
        assert!(lost.sent.items.iter().all(|i| !i.again), "nothing is remembered until the prompt was taken");
        told.record(first.fresh_ids(), Some(0));
        assert_eq!(first.into_sources().len(), 1);
        let mut second = compose(&pool, &query);
        told.filter(&mut second, Some(0));
        assert!(second.sent.items.iter().all(|i| i.again), "{:?}", second.sent.items);
        assert!(second.into_sources().is_empty(), "nothing to send again");
        let mut third = compose(&pool, &query);
        told.filter(&mut third, Some(1));
        assert!(third.sent.items.iter().all(|i| !i.again), "a new session has read nothing");
        let mut none = compose(&pool, &query);
        told.filter(&mut none, None);
        assert!(none.sent.items.iter().all(|i| !i.again), "a backend that keeps nothing hears it every turn");
    }
}

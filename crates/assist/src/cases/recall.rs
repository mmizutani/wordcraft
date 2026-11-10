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

use super::record::{Applies, Body, Case, CaseError, Day, Kind, Note, Quoted, Stance, Standing, Topics};
use super::store::{CaseId, CaseRef, Shelf};
use super::text::{Bm25, Token, tokens, topics_in};
use crate::backend::{cut, label, shown};
use crate::knowledge::{SourceOrigin, TurnSource};

/// Where a candidate comes from, against the query's case.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Place {
    ThisCase,
    SameClient,
}

impl Place {
    fn weight(self) -> f32 {
        match self {
            Place::ThisCase => 1.5,
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

/// A note seen from the query's case.
#[derive(Clone, Copy)]
pub struct Candidate<'a> {
    pub note: &'a Note,
    pub case: &'a Case,
    pub place: Place,
}

impl Candidate<'_> {
    fn applies(&self) -> Applies {
        self.case.applies(self.note.at)
    }

    fn tokens(&self) -> Vec<Token> {
        self.note.texts().flat_map(tokens).take(super::text::MAX_TOKENS).collect()
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
    let topics = TOPIC * q.topics.shared(c.note.topics()) as f32;
    let floor = if q.topics.is_empty() && !q.tokens.is_empty() && c.note.kind() == Kind::Pitfall { PITFALL_FLOOR } else { 0.0 };
    (topics + words.score(&q.tokens, &c.tokens())).max(floor)
}

pub fn score(q: &Query, c: &Candidate<'_>, words: &Bm25) -> f32 {
    let relevance = relevance(q, c, words);
    if relevance == 0.0 {
        return 0.0;
    }
    let then = c.applies();
    relevance
        * fit(&q.applies, &then)
        * transfer(c.note.kind(), then.stance, q.applies.stance)
        * age(c.note.kind(), c.note.at, q.today)
        * c.place.weight()
        * standing(c.note.standing)
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
}

impl std::fmt::Debug for RecallAsk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecallAsk").field("at", &self.at).field("today", &self.today).field("alone", &self.alone).finish_non_exhaustive()
    }
}

impl RecallAsk {
    pub fn new(shelf: Shelf, at: CaseRef, about: &str, today: Day, alone: Alone) -> RecallAsk {
        RecallAsk { shelf, at, about: cut(shown(about.to_string()), MAX_ABOUT), today, alone, after_reading: None }
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
        if pool.damaged {
            recalled.sent.problem = Some("this case's file is damaged, so nothing of it was recalled".into());
        } else if pool.skipped > 0 {
            recalled.sent.problem = Some(format!("{} of this client's cases couldn't be read", pool.skipped));
        }
        if let Some(f) = self.after_reading {
            f();
        }
        if let Err(o) = self.alone.recheck() {
            return others(o);
        }
        told.filter(&mut recalled, retains);
        recalled
    }
}

/// What recall can see: the bound case and the other cases of its wall. Built from one wall's
/// folder and nothing else.
struct Pool {
    here: Case,
    neighbors: Vec<Neighbor>,
    /// The bound case's file is damaged: it reads as empty here.
    damaged: bool,
    /// Other cases of the wall skipped as damaged or unreadable.
    skipped: u32,
}

impl Pool {
    fn load(shelf: &Shelf, at: &CaseRef) -> Result<Pool, CaseError> {
        let mut here = shelf.open(at)?;
        let damaged = std::mem::take(&mut here.damaged);
        if damaged {
            here = Case::new("");
        }
        let (cases, skipped) = shelf.recent_cases(at, MAX_NEIGHBORS, MAX_NEIGHBOR_BYTES);
        Ok(Pool { here, neighbors: cases.into_iter().map(|(id, case)| Neighbor::of(id, case)).collect(), damaged, skipped })
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
/// `<case>:n12` for another case's, `<case>:balance` for its balance. Disjoint from a preference's
/// `m1`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "where")]
pub enum ItemId {
    Here { note: super::record::NoteId },
    Neighbor { case: CaseId, note: super::record::NoteId },
    NeighborBalance { case: CaseId },
}

impl std::fmt::Display for ItemId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ItemId::Here { note } => write!(f, "{note}"),
            ItemId::Neighbor { case, note } => write!(f, "{}:{note}", case.as_str()),
            ItemId::NeighborBalance { case } => write!(f, "{}:balance", case.as_str()),
        }
    }
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
    /// Every text the items carried to the model, field by field (terms, what was traded for, traps,
    /// guards, how, a balance's factors): what [`crate::Transcript`] checks an output row against.
    /// It keeps them apart from the entry, so this isn't shown or sent on.
    #[serde(skip)]
    pub sent_text: Vec<String>,
}

impl RecallSent {
    pub(crate) fn bytes(&self) -> usize {
        self.items.iter().map(|i| i.text.len() + i.title.as_ref().map_or(0, String::len) + 32).sum::<usize>()
            + self.problem.as_ref().map_or(0, String::len)
    }

    /// Held to the pane's limits: at most [`MAX_ITEMS`] items, labels on one line.
    pub(crate) fn bounded(mut self) -> RecallSent {
        self.items.truncate(MAX_ITEMS.saturating_mul(2));
        for item in &mut self.items {
            item.text = cut(shown(std::mem::take(&mut item.text)), super::record::MAX_TEXT.saturating_mul(4));
            item.title = item.title.take().map(|t| label(&t));
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
        for json in &self.json {
            strings(json, 0, &mut sent_text);
        }
        RecallSent { sent_text, ..self.sent.clone() }
    }

    /// The ids of the items this turn carries, for [`Told::record`] once the prompt was taken.
    pub(crate) fn fresh_ids(&self) -> Vec<String> {
        self.sent.items.iter().filter(|i| !i.again).map(|i| i.id.to_string()).collect()
    }

    /// The items not sent to this session before, as one data-grade source; none when there are
    /// none.
    pub(crate) fn into_sources(self) -> Vec<TurnSource> {
        let fresh: Vec<&Value> = self.json.iter().zip(&self.sent.items).filter(|(_, item)| !item.again).map(|(j, _)| j).collect();
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

/// Every string in `json`, as [`note_item`] and [`balance_item`] build it: three levels deep at most.
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
            item.again = self.keys.contains(&item.id.to_string());
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
fn compose(pool: &Pool, query: &Query) -> Recalled {
    let here: Vec<Candidate<'_>> = pool.here.notes.iter().map(|note| Candidate { note, case: &pool.here, place: Place::ThisCase }).collect();
    let theirs =
        pool.neighbors.iter().flat_map(|nb| nb.case.notes.iter().map(move |note| Candidate { note, case: &nb.case, place: Place::SameClient }));
    let candidates: Vec<Candidate<'_>> = here.into_iter().chain(theirs).collect();
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
        if ranked.score < MIN_SCORE {
            out.sent.left_out = out.sent.left_out.saturating_add(1);
            continue;
        }
        let c = ranked.candidate;
        let neighbor = pool.neighbors.iter().find(|nb| std::ptr::eq(&nb.case, c.case));
        if let Some(nb) = neighbor
            && per_neighbor.get(&nb.id).copied().unwrap_or(0) >= MAX_PER_NEIGHBOR
        {
            out.sent.left_out = out.sent.left_out.saturating_add(1);
            continue;
        }
        let (item, json) = note_item(&c, neighbor, now);
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
            let words = format!("the same client's case {}", serde_json::to_string(&title).unwrap_or_default());
            (Place::SameClient, Some(title), words)
        }
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

fn note_item(c: &Candidate<'_>, neighbor: Option<&Neighbor>, now: Option<Stance>) -> (RecalledItem, Value) {
    let note = c.note;
    let id = match neighbor {
        None => ItemId::Here { note: note.id },
        Some(nb) => ItemId::Neighbor { case: nb.id.clone(), note: note.id },
    };
    let (place, title, place_words) = place_words(neighbor);
    let then = c.case.stance_on(note.at);
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
    facts_json(&mut json, &c.case.applies(note.at));
    json.insert("balance".into(), Value::String(gap.words()));
    json.insert("confirmed".into(), Value::Bool(confirmed));
    let item = RecalledItem { id, place, title, kind: Some(note.kind()), text, balance: Some(gap), confirmed, again: false };
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
    let item = RecalledItem { id, place, title, kind: None, text, balance: Some(gap), confirmed: true, again: false };
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

    #[test]
    fn a_neighbor_gives_at_most_three_notes_and_its_balance_and_the_rest_is_counted() {
        let confirmed = Standing::Confirmed { on: day("2026-01-01") };
        let pool = Pool {
            here: Case::new("here"),
            neighbors: (1..=3).map(|n| Neighbor::of(id(n), case(&format!("case {n}"), 10, confirmed))).collect(),
            damaged: false,
            skipped: 0,
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
        let pool = Pool { here, neighbors: Vec::new(), damaged: false, skipped: 0 };
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

    #[test]
    fn a_session_that_retains_is_told_each_item_once_and_a_new_session_again() {
        let confirmed = Standing::Confirmed { on: day("2026-01-01") };
        let pool = Pool { here: case("here", 2, confirmed), neighbors: Vec::new(), damaged: false, skipped: 0 };
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

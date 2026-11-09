//! What similarity means, and how it uses the power balance. A past item is worth recalling when
//! it is about the thing at hand, comes from a similar deal, and still transfers given who holds
//! the power now. Those are three questions, so [`score`] multiplies three answers: a mismatch
//! discounts an item and can never make an irrelevant one relevant.

use super::record::{Applies, Case, Day, Kind, Note, Stance, Standing, Topics};
use super::text::{Bm25, Token, tokens, topics_in};

/// Where a candidate comes from, against the query's case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
        Query { topics: topics_in(about), tokens: tokens(about).collect(), applies, today }
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
const TRANSFER: [[f32; 6]; 5] = [
    [1.00, 1.00, 1.00, 1.00, 1.00, 1.00],
    [0.70, 0.85, 1.00, 0.85, 0.70, 0.85],
    [0.50, 0.75, 1.00, 0.90, 0.80, 0.80],
    [0.20, 0.50, 1.00, 0.60, 0.40, 0.60],
    [0.25, 0.60, 1.00, 0.95, 0.90, 0.60],
];

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
    TRANSFER.get(kind as usize).and_then(|row| row.get(column)).copied().unwrap_or(1.0)
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

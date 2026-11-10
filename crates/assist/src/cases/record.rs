//! The typed record of a case. What decides legal similarity is a few closed lists, so similarity
//! is a pure function and free text survives only inside [`Quoted`]. The record is also what the
//! shelf writes to `case.json` ([`super::store`]), and the only way to change one is a typed
//! operation: everything the person can do is a [`PersonOp`], everything a model can do is a
//! [`ModelWrite`], and the second list has no confirm, no facts, no bind and no delete.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::backend::shown;
use crate::memory::{is_key, key_of, normalized};

/// Most characters of a note's main text, a plan's goal or a step, and a title.
pub const MAX_TEXT: usize = 400;

/// Most characters of `how`, a trap, a guard, what a position was traded for, a factor's why and
/// a party's name.
pub const MAX_SHORT: usize = 120;

/// Most notes a case holds.
pub const MAX_NOTES: usize = 400;

/// Most unconfirmed notes a case holds. Past it a model's new note replaces the oldest never used.
pub const MAX_UNCONFIRMED: usize = 64;

/// Most dated balances. They only add up, so the oldest go when there are more.
pub const MAX_BALANCES: usize = 32;

/// Most factors one balance names. Four, not eight, so a case at every limit fits [`MAX_FILE`]
/// in three-byte characters.
///
/// [`MAX_FILE`]: super::store::MAX_FILE
pub const MAX_FACTORS: usize = 4;

/// Most parties a case names, and most steps a plan lists.
pub const MAX_LIST: usize = 16;

/// Most dismissed hashes kept; the oldest go first.
pub const MAX_DISMISSED: usize = 256;

/// The file's version this build reads and writes.
pub(crate) const VERSION: u32 = 1;

/// Text a model, a document or the person wrote. It reaches a model only quoted as data and the
/// pane through [`Quoted::for_person`]. No `Display`, `Deref`, `AsRef<str>` or `Into<String>`, so it
/// can't become a `String` by accident. Parsing strips what hides or reorders text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String")]
pub struct Quoted(String);

impl Quoted {
    pub fn parse(text: &str) -> Quoted {
        Quoted(shown(text.to_string()))
    }

    pub fn for_person(&self) -> &str {
        &self.0
    }

    pub(crate) fn raw(&self) -> &str {
        &self.0
    }

    fn chars(&self) -> usize {
        self.0.chars().count()
    }

    /// Cut to `max` characters; whether anything went.
    pub(crate) fn cut(&mut self, max: usize) -> bool {
        if self.chars() <= max {
            return false;
        }
        self.0 = self.0.chars().take(max).collect();
        true
    }
}

impl From<String> for Quoted {
    fn from(text: String) -> Quoted {
        Quoted(shown(text))
    }
}

/// A date, through chrono. Serialized as `YYYY-MM-DD`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Day(NaiveDate);

impl Day {
    /// `YYYY-MM-DD`.
    pub fn parse(text: &str) -> Option<Day> {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").ok().map(Day)
    }

    pub fn year(self) -> i32 {
        use chrono::Datelike;
        self.0.year()
    }

    /// The first day of `year`: what a lesson, which keeps only its year, ages from.
    pub fn first_of(year: i32) -> Option<Day> {
        NaiveDate::from_ymd_opt(year, 1, 1).map(Day)
    }

    /// Days from `earlier` to `self`, zero when `earlier` is later.
    pub fn days_since(self, earlier: Day) -> i64 {
        (self.0 - earlier.0).num_days().max(0)
    }
}

impl TryFrom<String> for Day {
    type Error = String;
    fn try_from(text: String) -> Result<Day, String> {
        Day::parse(&text).ok_or_else(|| "a date is YYYY-MM-DD".to_string())
    }
}

impl From<Day> for String {
    fn from(day: Day) -> String {
        day.0.format("%Y-%m-%d").to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContractKind {
    Nda,
    Msa,
    Saas,
    Licence,
    Services,
    Supply,
    Distribution,
    Employment,
    Lease,
    Loan,
    Acquisition,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OurSide {
    Vendor,
    Customer,
    Licensor,
    Licensee,
    Employer,
    Employee,
    Lender,
    Borrower,
    Buyer,
    Seller,
    Landlord,
    Tenant,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Paper {
    Ours,
    Theirs,
    ThirdParty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CounterpartyKind {
    Startup,
    Sme,
    Enterprise,
    Public,
    Individual,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SizeBand {
    Small,
    Medium,
    Large,
    Huge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Lang {
    En,
    Ja,
    Other,
}

/// Where a case stands. A closed case's positions are final.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    #[default]
    Negotiating,
    Signed,
    Abandoned,
}

/// ISO 3166: `JP`, `US-NY`. Fixed bytes, so `Copy`. Serialized as that string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Law {
    country: [u8; 2],
    region: Option<[u8; 3]>,
}

impl Law {
    /// Two ASCII letters, with an optional `-` and one to three ASCII letters or digits.
    pub fn parse(text: &str) -> Option<Law> {
        let (country, region) = match text.split_once('-') {
            Some((c, r)) => (c, Some(r)),
            None => (text, None),
        };
        let c = country.as_bytes();
        if c.len() != 2 || !c.iter().all(u8::is_ascii_alphabetic) {
            return None;
        }
        let region = match region {
            None => None,
            Some(r) if (1..=3).contains(&r.len()) && r.bytes().all(|b| b.is_ascii_alphanumeric()) => {
                let mut bytes = [b' '; 3];
                for (slot, b) in bytes.iter_mut().zip(r.bytes()) {
                    *slot = b.to_ascii_uppercase();
                }
                Some(bytes)
            }
            Some(_) => return None,
        };
        Some(Law { country: [c[0].to_ascii_uppercase(), c[1].to_ascii_uppercase()], region })
    }

    pub fn same_country(self, other: Law) -> bool {
        self.country == other.country
    }
}

impl TryFrom<String> for Law {
    type Error = String;
    fn try_from(text: String) -> Result<Law, String> {
        Law::parse(&text).ok_or_else(|| "a law is JP or US-NY".to_string())
    }
}

impl From<Law> for String {
    fn from(law: Law) -> String {
        let mut out: String = law.country.iter().map(|b| char::from(*b)).collect();
        if let Some(region) = law.region {
            out.push('-');
            out.extend(region.iter().filter(|b| **b != b' ').map(|b| char::from(*b)));
        }
        out
    }
}

/// What a note is about. Every topic has English and Japanese terms in `text::TERMS`, which is
/// how a Japanese note and an English prompt meet with no translation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Topic {
    Liability,
    Indemnity,
    Ip,
    Confidentiality,
    Termination,
    DataProtection,
    GoverningLaw,
    Assignment,
    Warranties,
    Sla,
    Payment,
}

impl Topic {
    pub const ALL: [Topic; 11] = [
        Topic::Liability,
        Topic::Indemnity,
        Topic::Ip,
        Topic::Confidentiality,
        Topic::Termination,
        Topic::DataProtection,
        Topic::GoverningLaw,
        Topic::Assignment,
        Topic::Warranties,
        Topic::Sla,
        Topic::Payment,
    ];

    fn bit(self) -> u16 {
        1 << (self as u16)
    }
}

/// A set of topics, a bit per [`Topic`]. Serialized as the list of names.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "Vec<Topic>", into = "Vec<Topic>")]
pub struct Topics(u16);

impl Topics {
    pub fn insert(&mut self, topic: Topic) {
        self.0 |= topic.bit();
    }

    pub fn contains(self, topic: Topic) -> bool {
        self.0 & topic.bit() != 0
    }

    pub fn union(self, other: Topics) -> Topics {
        Topics(self.0 | other.0)
    }

    /// How many topics both have.
    pub fn shared(self, other: Topics) -> u32 {
        (self.0 & other.0).count_ones()
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl FromIterator<Topic> for Topics {
    fn from_iter<I: IntoIterator<Item = Topic>>(iter: I) -> Topics {
        let mut set = Topics::default();
        for t in iter {
            set.insert(t);
        }
        set
    }
}

impl From<Vec<Topic>> for Topics {
    fn from(list: Vec<Topic>) -> Topics {
        list.into_iter().collect()
    }
}

impl From<Topics> for Vec<Topic> {
    fn from(set: Topics) -> Vec<Topic> {
        Topic::ALL.into_iter().filter(|t| set.contains(*t)).collect()
    }
}

/// Ours, against theirs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Stance {
    Stronger,
    Even,
    Weaker,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Side {
    Us,
    Them,
    Neither,
}

/// Who stands behind a record. Only [`Case::apply_person`] writes `Confirmed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub enum Standing {
    Unconfirmed,
    Confirmed { on: Day },
}

impl Standing {
    fn is_confirmed(self) -> bool {
        matches!(self, Standing::Confirmed { .. })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FactorKind {
    Dependence,
    Alternatives,
    Deadline,
    SoleSource,
    RepeatRelationship,
    Pen,
    Other,
}

/// One reason a balance stands where it does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Factor {
    pub kind: FactorKind,
    pub favors: Side,
    pub why: Option<Quoted>,
}

/// One dated assessment. A case holds them oldest first and they only add up: what a note was
/// worth depends on the balance on its date.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Balance {
    pub on: Day,
    pub stance: Stance,
    #[serde(default)]
    pub factors: Vec<Factor>,
    pub standing: Standing,
}

/// What decides similarity. The person's click sets all of it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Facts {
    pub kind: Option<ContractKind>,
    pub side: Option<OurSide>,
    pub paper: Option<Paper>,
    pub theirs: Option<CounterpartyKind>,
    pub law: Option<Law>,
    pub size: Option<SizeBand>,
    /// Shown, not scored: a lesson applies across languages and the topic table bridges them.
    pub language: Option<Lang>,
    pub stage: Stage,
    /// Names. They stay inside the wall.
    pub parties: Vec<Quoted>,
}

/// The part of a case that may travel with a lesson. It is `Copy`, so the compiler proves it holds
/// no text: a client's name can't cross a wall in this struct. Coarse on purpose: a year, not a
/// day, and no size band, so it doesn't identify the deal to a provider. Written to the lessons
/// file as it is, and a `size` key there reads as damage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Applies {
    pub kind: Option<ContractKind>,
    pub side: Option<OurSide>,
    pub paper: Option<Paper>,
    pub theirs: Option<CounterpartyKind>,
    pub law: Option<Law>,
    pub stance: Option<Stance>,
    pub year: Option<i32>,
}

const _: fn() = || {
    fn is_copy<T: Copy>() {}
    is_copy::<Applies>();
};

/// The variant of [`Body`] with no data. `TRANSFER` and the half-lives are indexed by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    Pitfall,
    Pattern,
    Lesson,
    Leverage,
    Position,
}

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::Pitfall, Kind::Pattern, Kind::Lesson, Kind::Leverage, Kind::Position];
}

/// What a note says, by kind. Each kind has only the fields it needs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub enum Body {
    Pattern {
        rule: Quoted,
        traps: Option<Quoted>,
    },
    Pitfall {
        trap: Quoted,
        guard: Option<Quoted>,
    },
    Lesson {
        rule: Quoted,
        traps: Option<Quoted>,
    },
    Leverage {
        what: Quoted,
        favors: Side,
        until: Option<Day>,
    },
    /// A move on a clause, quoting the terms verbatim.
    Position {
        topic: Topic,
        moved: Side,
        terms: Quoted,
        traded_for: Option<Quoted>,
        landed: bool,
    },
}

impl Body {
    pub fn kind(&self) -> Kind {
        match self {
            Body::Pattern { .. } => Kind::Pattern,
            Body::Pitfall { .. } => Kind::Pitfall,
            Body::Lesson { .. } => Kind::Lesson,
            Body::Leverage { .. } => Kind::Leverage,
            Body::Position { .. } => Kind::Position,
        }
    }

    /// Every text in the body, main text first.
    pub fn texts(&self) -> impl Iterator<Item = &Quoted> {
        let (main, more) = match self {
            Body::Pattern { rule, traps } | Body::Lesson { rule, traps } => (rule, traps.as_ref()),
            Body::Pitfall { trap, guard } => (trap, guard.as_ref()),
            Body::Leverage { what, .. } => (what, None),
            Body::Position { terms, traded_for, .. } => (terms, traded_for.as_ref()),
        };
        std::iter::once(main).chain(more)
    }

    /// The main text, then the short one, for cutting.
    fn texts_mut(&mut self) -> (&mut Quoted, Option<&mut Quoted>) {
        match self {
            Body::Pattern { rule, traps } | Body::Lesson { rule, traps } => (rule, traps.as_mut()),
            Body::Pitfall { trap, guard } => (trap, guard.as_mut()),
            Body::Leverage { what, .. } => (what, None),
            Body::Position { terms, traded_for, .. } => (terms, traded_for.as_mut()),
        }
    }

    /// Replace the main text, keeping the kind and every other field: what a review's correction
    /// changes.
    pub fn set_main(&mut self, text: Quoted) {
        *self.texts_mut().0 = text;
    }

    /// The topic a position names; other kinds state none here.
    pub(crate) fn topic(&self) -> Option<Topic> {
        match self {
            Body::Position { topic, .. } => Some(*topic),
            _ => None,
        }
    }
}

/// A note's id, `n12`. Numbers are never reused, so an edit that waited in another window can't
/// land on a different note.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct NoteId(u64);

/// The highest number an id is read with, so adding to it can't overflow.
const MAX_ID_NUMBER: u64 = 1 << 53;

impl NoteId {
    pub fn parse(text: &str) -> Option<NoteId> {
        let n = text.strip_prefix('n')?;
        if n.is_empty() || n.len() > 16 || !n.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        n.parse().ok().filter(|n| *n <= MAX_ID_NUMBER).map(NoteId)
    }
}

impl TryFrom<String> for NoteId {
    type Error = String;
    fn try_from(text: String) -> Result<NoteId, String> {
        NoteId::parse(&text).ok_or_else(|| "a note id is n12".to_string())
    }
}

impl From<NoteId> for String {
    fn from(id: NoteId) -> String {
        format!("n{}", id.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Note {
    pub id: NoteId,
    pub at: Day,
    pub body: Body,
    /// How it was known.
    pub how: Option<Quoted>,
    pub standing: Standing,
    /// Topics stated at write time. [`Note::topics`] adds what the term table finds in its text.
    #[serde(default)]
    pub stated_topics: Topics,
    #[serde(default)]
    pub used: u64,
    #[serde(default)]
    pub last_used: Option<Day>,
}

impl Note {
    pub fn kind(&self) -> Kind {
        self.body.kind()
    }

    /// Every text of the note, for searching.
    pub(crate) fn texts(&self) -> impl Iterator<Item = &str> {
        self.body.texts().chain(self.how.iter()).map(Quoted::raw)
    }

    pub fn topics(&self) -> Topics {
        let found: Topics = self.texts().map(super::text::topics_in).fold(Topics::default(), Topics::union);
        self.stated_topics.union(found).union(self.body.topic().into_iter().collect())
    }

    /// What two notes saying the same thing share: the main text, normalized.
    fn said(&self) -> String {
        self.body.texts().next().map(|q| normalized(q.raw())).unwrap_or_default()
    }

    /// Hold every text to its limit; whether anything was cut.
    fn clamp(&mut self) -> bool {
        let (main, short) = self.body.texts_mut();
        let mut cut = main.cut(MAX_TEXT);
        cut |= short.is_some_and(|s| s.cut(MAX_SHORT));
        cut |= self.how.as_mut().is_some_and(|h| h.cut(MAX_SHORT));
        cut
    }
}

/// Where the case is going. `standing` says who wrote it: a model's plan is `Unconfirmed`, and a
/// model never replaces a person's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub goal: Quoted,
    #[serde(default)]
    pub next: Vec<Quoted>,
    pub standing: Standing,
}

impl Plan {
    fn clamp(&mut self) -> bool {
        let mut cut = self.goal.cut(MAX_TEXT);
        if self.next.len() > MAX_LIST {
            self.next.truncate(MAX_LIST);
            cut = true;
        }
        for step in &mut self.next {
            cut |= step.cut(MAX_TEXT);
        }
        cut
    }
}

/// The record. Only the shelf writes one to disk, and only through [`Case::apply_person`] and
/// [`Case::apply_model`]. Every persisted type denies unknown fields, so a file from a build that
/// knows more reads as damaged instead of losing that field on the next write.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Case {
    #[serde(default)]
    version: u32,
    pub title: Quoted,
    #[serde(default)]
    pub facts: Facts,
    #[serde(default)]
    pub balance: Vec<Balance>,
    #[serde(default)]
    pub plan: Option<Plan>,
    #[serde(default)]
    pub notes: Vec<Note>,
    /// The prints of the drafts the person bound to this case ([`super::lineage`]), oldest first,
    /// at most [`MAX_DRAFTS`]. Only [`PersonOp::AddDraft`] adds one, and no turn reads them.
    ///
    /// [`MAX_DRAFTS`]: super::lineage::MAX_DRAFTS
    #[serde(default)]
    pub drafts: Vec<super::lineage::DraftPrint>,
    /// SHA-256 of a note's text the person removed, so the model doesn't note it again.
    #[serde(default)]
    dismissed: Vec<String>,
    /// The same for a note a review retired. Kept apart from `dismissed`, each capped on its
    /// own, so a model retiring its own notes over and over can push out nothing the person
    /// said no to. It blocks the model only.
    #[serde(default)]
    model_dismissed: Vec<String>,
    #[serde(default)]
    last_note: u64,
    /// Whether the case keeps a record of its conversations ([`super::log`]). Off for every new
    /// case; only [`PersonOp::Record`] turns it on, so no marker can.
    #[serde(default)]
    pub record: bool,
    /// The file held more than this build reads, so it can't be written back without losing
    /// something: the next person's write moves it aside first. Never written.
    #[serde(skip)]
    pub damaged: bool,
}

impl Case {
    /// An empty case called `title`.
    pub fn new(title: &str) -> Case {
        let mut title = Quoted::parse(title);
        title.cut(MAX_TEXT);
        Case {
            version: VERSION,
            title,
            facts: Facts::default(),
            balance: Vec::new(),
            plan: None,
            notes: Vec::new(),
            drafts: Vec::new(),
            dismissed: Vec::new(),
            model_dismissed: Vec::new(),
            last_note: 0,
            record: false,
            damaged: false,
        }
    }

    /// The newest confirmed assessment, else the newest unconfirmed one. A model's reading never
    /// overrides the person's.
    pub fn stance_now(&self) -> Option<(Stance, Standing)> {
        self.stance_among(|_| true)
    }

    /// The stance on a past date, derived and never copied onto a note.
    pub fn stance_on(&self, day: Day) -> Option<Stance> {
        self.stance_among(|b| b.on <= day).map(|(stance, _)| stance)
    }

    fn stance_among(&self, keep: impl Fn(&Balance) -> bool) -> Option<(Stance, Standing)> {
        let kept = || self.balance.iter().rev().filter(|b| keep(b));
        kept().find(|b| b.standing.is_confirmed()).or_else(|| kept().next()).map(|b| (b.stance, b.standing))
    }

    /// The facts, with the stance and year as of `on`.
    pub fn applies(&self, on: Day) -> Applies {
        let Facts { kind, side, paper, theirs, law, .. } = self.facts;
        Applies { kind, side, paper, theirs, law, stance: self.stance_on(on), year: Some(on.year()) }
    }

    pub fn dismissed(&self) -> &[String] {
        &self.dismissed
    }

    /// Read from a file's bytes: what this build can read of it, and `damaged` when that is less
    /// than the file held. An unknown version reads as nothing but its title.
    pub(crate) fn read(bytes: &[u8]) -> Case {
        let title_of = |v: &serde_json::Value| v.get("title").and_then(serde_json::Value::as_str).map(Quoted::parse);
        let mut case = match serde_json::from_slice::<Case>(bytes) {
            Ok(case) if case.version == VERSION => case,
            _ => {
                let mut case = Case::new("");
                case.title = serde_json::from_slice::<serde_json::Value>(bytes).ok().as_ref().and_then(title_of).unwrap_or(case.title);
                case.title.cut(MAX_TEXT);
                case.damaged = true;
                return case;
            }
        };
        case.damaged = case.clamp();
        case
    }

    pub(crate) fn to_bytes(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|e| e.to_string())
    }

    /// Hold everything to the limits; whether anything went.
    fn clamp(&mut self) -> bool {
        let mut cut = self.title.cut(MAX_TEXT);
        cut |= truncate(&mut self.notes, MAX_NOTES);
        cut |= truncate(&mut self.balance, MAX_BALANCES);
        cut |= truncate(&mut self.facts.parties, MAX_LIST);
        cut |= truncate(&mut self.dismissed, MAX_DISMISSED);
        cut |= truncate(&mut self.model_dismissed, MAX_DISMISSED);
        for note in &mut self.notes {
            cut |= note.clamp();
        }
        for b in &mut self.balance {
            cut |= truncate(&mut b.factors, MAX_FACTORS);
            for f in &mut b.factors {
                cut |= f.why.as_mut().is_some_and(|w| w.cut(MAX_SHORT));
            }
        }
        for p in &mut self.facts.parties {
            cut |= p.cut(MAX_SHORT);
        }
        cut |= self.plan.as_mut().is_some_and(Plan::clamp);
        cut |= drop_oldest(&mut self.drafts, super::lineage::MAX_DRAFTS);
        for d in &mut self.drafts {
            cut |= d.clamp();
        }
        for list in [&mut self.dismissed, &mut self.model_dismissed] {
            let before = list.len();
            list.retain(|k| is_key(k));
            cut |= list.len() != before;
        }
        let mut seen = std::collections::HashSet::new();
        let before = self.notes.len();
        self.notes.retain(|n| seen.insert(n.id));
        cut |= self.notes.len() != before;
        if self.last_note > MAX_ID_NUMBER {
            self.last_note = MAX_ID_NUMBER;
            cut = true;
        }
        self.last_note = self.last_note.max(self.highest_id());
        cut
    }

    fn highest_id(&self) -> u64 {
        self.notes.iter().map(|n| n.id.0).max().unwrap_or(0)
    }

    /// An id no note has or has had, or `Full("note ids")` when the next would not parse back.
    fn next_id(&mut self) -> Result<NoteId, CaseError> {
        let next = self.last_note.max(self.highest_id()).saturating_add(1);
        if next > MAX_ID_NUMBER {
            return Err(CaseError::Full("note ids"));
        }
        self.last_note = next;
        Ok(NoteId(next))
    }

    fn note_mut(&mut self, id: NoteId) -> Result<&mut Note, CaseError> {
        self.notes.iter_mut().find(|n| n.id == id).ok_or(CaseError::NoSuchNote(id))
    }

    /// A note that says `draft`'s main text already, or the person's dismissal of it. The
    /// person's own write answers to this too, as it did before reviews (#107, AS8.1).
    fn knows(&self, draft: &NoteDraft) -> bool {
        self.knows_text(&draft.main(), None) || self.dismissed.contains(&key_of(&draft.main()))
    }

    /// What a model's new text answers to: what [`Case::knows`] refuses, and what a review
    /// retired. `except` is the note being corrected, which may of course say its own text.
    fn model_knows(&self, text: &str, except: Option<NoteId>) -> bool {
        let key = key_of(text);
        self.knows_text(text, except) || self.dismissed.contains(&key) || self.model_dismissed.contains(&key)
    }

    /// Whether a note other than `except` says `text`, normalized.
    fn knows_text(&self, text: &str, except: Option<NoteId>) -> bool {
        let said = normalized(text);
        self.notes.iter().any(|n| Some(n.id) != except && n.said() == said)
    }

    /// Append a balance, after `evict` has made room: past the cap the person drops the oldest,
    /// and a model drops only the oldest unconfirmed one, so it never removes what the person wrote.
    fn push_balance(&mut self, draft: BalanceDraft, standing: Standing, today: Day, evict: usize) -> Result<(), CaseError> {
        let BalanceDraft { stance, factors } = draft;
        if factors.len() > MAX_FACTORS || factors.iter().any(|f| f.why.as_ref().is_some_and(|w| w.chars() > MAX_SHORT)) {
            return Err(CaseError::TooLong);
        }
        if self.balance.len() >= MAX_BALANCES && evict < self.balance.len() {
            self.balance.remove(evict);
        }
        self.balance.push(Balance { on: today, stance, factors, standing });
        self.balance.truncate(MAX_BALANCES);
        Ok(())
    }

    /// What the person did, with their click. Everything it writes is `Confirmed` today.
    pub(crate) fn apply_person(&mut self, op: PersonOp, today: Day) -> Result<(), CaseError> {
        let confirmed = Standing::Confirmed { on: today };
        match op {
            PersonOp::Title(title) => {
                let title = Quoted::parse(&title);
                if title.chars() > MAX_TEXT {
                    return Err(CaseError::TooLong);
                }
                self.title = title;
            }
            PersonOp::Facts(mut facts) => {
                if facts.parties.len() > MAX_LIST || facts.parties.iter().any(|p| p.chars() > MAX_SHORT) {
                    return Err(CaseError::TooLong);
                }
                facts.parties.retain(|p| !p.raw().trim().is_empty());
                self.facts = facts;
            }
            PersonOp::Balance(draft) => self.push_balance(draft, confirmed, today, 0)?,
            PersonOp::Plan(None) => self.plan = None,
            PersonOp::Plan(Some(plan)) => {
                let mut plan = Plan { standing: confirmed, ..plan };
                if plan.clamp() {
                    return Err(CaseError::TooLong);
                }
                self.plan = Some(plan);
            }
            PersonOp::AddNote(draft) => {
                if self.knows(&draft) {
                    return Ok(());
                }
                if self.notes.len() >= MAX_NOTES {
                    return Err(CaseError::Full("notes"));
                }
                let note = draft.into_note(self.next_id()?, today, confirmed)?;
                self.notes.push(note);
            }
            PersonOp::EditNote(id, draft) => {
                let at = self.note_mut(id)?.at;
                let edited = draft.into_note(id, at, confirmed)?;
                *self.note_mut(id)? = edited;
            }
            PersonOp::Confirm(id) => {
                let note = self.note_mut(id)?;
                if !note.standing.is_confirmed() {
                    note.standing = confirmed;
                }
            }
            PersonOp::RemoveNote(id) => self.remove_note(id, false)?,
            PersonOp::Close(stage) => self.facts.stage = stage,
            PersonOp::Record(on) => self.record = on,
            PersonOp::AddDraft(mut print) => {
                print.clamp();
                self.drafts.push(print);
                drop_oldest(&mut self.drafts, super::lineage::MAX_DRAFTS);
            }
        }
        Ok(())
    }

    /// The note goes, and a hash of its main text joins the person's or the model's dismissals so
    /// a model doesn't note it again.
    fn remove_note(&mut self, id: NoteId, by_model: bool) -> Result<(), CaseError> {
        let i = self.notes.iter().position(|n| n.id == id).ok_or(CaseError::NoSuchNote(id))?;
        let gone = self.notes.remove(i);
        let key = key_of(gone.body.texts().next().map_or("", Quoted::raw));
        let list = if by_model { &mut self.model_dismissed } else { &mut self.dismissed };
        if !list.contains(&key) {
            list.push(key);
        }
        if list.len() > MAX_DISMISSED {
            list.remove(0);
        }
        Ok(())
    }

    /// What a model wrote. Nothing here confirms, sets facts or removes what the person wrote:
    /// what it writes is `Unconfirmed`, and over what the person wrote it becomes a card. A
    /// review's correction or retirement of its own unconfirmed note is an in-case write like any
    /// other; of a confirmed note it is a card.
    pub(crate) fn apply_model(&mut self, write: ModelWrite, today: Day) -> Result<Applied, CaseError> {
        match write {
            ModelWrite::Correct { note, text, why } => {
                if self.note_mut(note)?.standing.is_confirmed() {
                    return Ok(Applied::Card(ModelWrite::Correct { note, text, why }));
                }
                // A correction is a new text: what a new note can't say, it can't either.
                if self.model_knows(text.raw(), Some(note)) {
                    return Ok(Applied::Known);
                }
                let n = self.note_mut(note)?;
                n.body.set_main(text);
                n.how = Some(why);
                if n.clamp() {
                    return Err(CaseError::TooLong);
                }
                Ok(Applied::Edited(note))
            }
            ModelWrite::Retire { note, why } => {
                if self.note_mut(note)?.standing.is_confirmed() {
                    return Ok(Applied::Card(ModelWrite::Retire { note, why }));
                }
                self.remove_note(note, true)?;
                Ok(Applied::Removed(note))
            }
            ModelWrite::Note(draft) => {
                if self.model_knows(&draft.main(), None) {
                    return Ok(Applied::Known);
                }
                let unconfirmed: Vec<usize> = self.notes.iter().enumerate().filter(|(_, n)| !n.standing.is_confirmed()).map(|(i, _)| i).collect();
                let evict = if unconfirmed.len() >= MAX_UNCONFIRMED {
                    // The oldest never used, else the oldest. `min_by_key` keeps the first of equals,
                    // and notes are kept in the order written.
                    let never_used = unconfirmed.iter().copied().filter(|i| self.notes.get(*i).is_some_and(|n| n.used == 0));
                    never_used.min_by_key(|i| self.notes.get(*i).map(|n| n.at)).or_else(|| unconfirmed.first().copied())
                } else if self.notes.len() >= MAX_NOTES {
                    return Err(CaseError::Full("notes"));
                } else {
                    None
                };
                let note = draft.into_note(self.next_id()?, today, Standing::Unconfirmed)?;
                let added = note.id;
                match evict {
                    Some(i) if i < self.notes.len() => {
                        let evicted = self.notes.remove(i).id;
                        self.notes.push(note);
                        Ok(Applied::Replaced { added, evicted })
                    }
                    _ => {
                        self.notes.push(note);
                        Ok(Applied::Added(added))
                    }
                }
            }
            ModelWrite::Plan(plan) => {
                if self.plan.as_ref().is_some_and(|p| p.standing.is_confirmed()) {
                    return Ok(Applied::Card(ModelWrite::Plan(plan)));
                }
                let mut plan = Plan { standing: Standing::Unconfirmed, ..plan };
                if plan.clamp() {
                    return Err(CaseError::TooLong);
                }
                self.plan = Some(plan);
                Ok(Applied::Changed)
            }
            ModelWrite::Balance(draft) => {
                let oldest_unconfirmed = self.balance.iter().position(|b| !b.standing.is_confirmed());
                let evict = match oldest_unconfirmed {
                    Some(i) => i,
                    None if self.balance.len() >= MAX_BALANCES => return Ok(Applied::Card(ModelWrite::Balance(draft))),
                    None => 0,
                };
                self.push_balance(draft, Standing::Unconfirmed, today, evict)?;
                Ok(Applied::Changed)
            }
            ModelWrite::Used(ids) => {
                for note in self.notes.iter_mut().filter(|n| ids.contains(&n.id)) {
                    note.used = note.used.saturating_add(1);
                    note.last_used = Some(today);
                }
                Ok(Applied::Changed)
            }
        }
    }
}

fn truncate<T>(list: &mut Vec<T>, max: usize) -> bool {
    if list.len() <= max {
        return false;
    }
    list.truncate(max);
    true
}

/// Keep the newest `max` of a list held oldest first; whether anything went.
fn drop_oldest<T>(list: &mut Vec<T>, max: usize) -> bool {
    let over = list.len().saturating_sub(max);
    if over == 0 {
        return false;
    }
    list.drain(..over);
    true
}

/// A note as a marker or a command gives it, before the shelf adds the id, the date and the
/// standing. Deserialized flat, so a marker's body is the note's fields beside `op`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteDraft {
    #[serde(flatten)]
    pub body: Body,
    #[serde(default)]
    pub how: Option<Quoted>,
    #[serde(default)]
    pub topics: Topics,
}

impl NoteDraft {
    fn main(&self) -> String {
        self.body.texts().next().map(|q| q.raw().to_string()).unwrap_or_default()
    }

    fn into_note(self, id: NoteId, at: Day, standing: Standing) -> Result<Note, CaseError> {
        let mut note = Note { id, at, body: self.body, how: self.how, standing, stated_topics: self.topics, used: 0, last_used: None };
        if note.clamp() {
            return Err(CaseError::TooLong);
        }
        Ok(note)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BalanceDraft {
    pub stance: Stance,
    #[serde(default)]
    pub factors: Vec<Factor>,
}

/// Everything the person can do to a case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersonOp {
    Title(String),
    Facts(Facts),
    Balance(BalanceDraft),
    Plan(Option<Plan>),
    AddNote(NoteDraft),
    EditNote(NoteId, NoteDraft),
    Confirm(NoteId),
    RemoveNote(NoteId),
    Close(Stage),
    /// Keep a record of this case's conversations, or stop.
    Record(bool),
    /// The print of a document the person bound to this case ([`super::lineage`]).
    AddDraft(super::lineage::DraftPrint),
}

/// Everything a model can do to a case. It has no Confirm, no Facts, no bind and no delete. The
/// last two come only from a review's answer ([`super::marker::CaseMarker::review_only`]) and
/// reach a confirmed note only as a card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelWrite {
    Note(NoteDraft),
    Plan(Plan),
    Balance(BalanceDraft),
    Used(Vec<NoteId>),
    /// The note's main text becomes `text`, and `why` its `how`.
    Correct {
        note: NoteId,
        text: Quoted,
        why: Quoted,
    },
    /// The note goes, its text joining `dismissed`.
    Retire {
        note: NoteId,
        why: Quoted,
    },
}

/// What [`Case::apply_model`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    Added(NoteId),
    Replaced {
        added: NoteId,
        evicted: NoteId,
    },
    /// A review corrected this unconfirmed note.
    Edited(NoteId),
    /// A review retired this unconfirmed note.
    Removed(NoteId),
    /// A note already says it, or the person said no to it.
    Known,
    Changed,
    /// It would replace what the person wrote: nothing changed, and the window shows it as a card.
    Card(ModelWrite),
}

/// Proof that the window's one guard saw the person's own click. A line a reviewer can see, not a
/// cryptographic proof.
#[derive(Clone, Copy, Debug)]
pub struct Click(());

impl Click {
    pub fn by_person(by_person: bool) -> Option<Click> {
        by_person.then_some(Click(()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CaseError {
    /// Deleted, in this window or another. A write never brings it back.
    #[error("this case was deleted")]
    Gone,
    #[error("{}", crate::atomic::BUSY)]
    Busy,
    #[error("the case holds the most {0} it can: remove one first")]
    Full(&'static str),
    /// A client still holds a case, perhaps one another window added since this one listed.
    #[error("this client still has a case: delete its cases first")]
    NotEmpty,
    #[error("a text is longer than a case keeps")]
    TooLong,
    #[error("{0}: no such note")]
    NoSuchNote(NoteId),
    #[error("{0}: no such lesson")]
    NoSuchLesson(super::lessons::LessonId),
    /// The file can't be read whole; a model's write waits for the person's, which moves it aside.
    #[error("this case's file is damaged: your next change moves it aside")]
    Damaged,
    #[error("{0}")]
    Io(String),
}

impl std::fmt::Display for NoteId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "n{}", self.0)
    }
}

//! The typed record of a case, as far as ranking needs it. What decides legal similarity is a few
//! closed lists, so similarity is a pure function and free text survives only inside [`Quoted`].

use chrono::NaiveDate;

use crate::backend::shown;

/// Text a model, a document or the person wrote. It reaches a model only quoted as data and the
/// pane through [`Quoted::for_person`]. No `Display`, `Deref`, `AsRef<str>` or `Into<String>`, so it
/// can't become a `String` by accident. Parsing strips what hides or reorders text.
#[derive(Clone, Debug, PartialEq, Eq)]
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
}

/// A date, through chrono.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
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

    /// Days from `earlier` to `self`, zero when `earlier` is later.
    pub fn days_since(self, earlier: Day) -> i64 {
        (self.0 - earlier.0).num_days().max(0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Paper {
    Ours,
    Theirs,
    ThirdParty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CounterpartyKind {
    Startup,
    Sme,
    Enterprise,
    Public,
    Individual,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SizeBand {
    Small,
    Medium,
    Large,
    Huge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lang {
    En,
    Ja,
    Other,
}

/// ISO 3166: `JP`, `US-NY`. Fixed bytes, so `Copy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

/// What a note is about. Every topic has English and Japanese terms in `text::TERMS`, which is
/// how a Japanese note and an English prompt meet with no translation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

/// A set of topics, a bit per [`Topic`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
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

/// Ours, against theirs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stance {
    Stronger,
    Even,
    Weaker,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Us,
    Them,
    Neither,
}

/// Who stands behind a record. Only the person's click writes `Confirmed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Standing {
    Unconfirmed,
    Confirmed { on: Day },
}

/// One dated assessment. A case holds them oldest first and they only add up: what a note was
/// worth depends on the balance on its date.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Balance {
    pub on: Day,
    pub stance: Stance,
    pub standing: Standing,
}

/// What decides similarity. The person's click sets all of it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    pub kind: Option<ContractKind>,
    pub side: Option<OurSide>,
    pub paper: Option<Paper>,
    pub theirs: Option<CounterpartyKind>,
    pub law: Option<Law>,
}

/// The part of a case that may travel with a lesson. It is `Copy`, so the compiler proves it holds
/// no text: a client's name can't cross a wall in this struct. Coarse on purpose: a year, not a
/// day, and no size band, so it doesn't identify the deal to a provider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
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

    /// The topic a position names; other kinds state none here.
    fn topic(&self) -> Option<Topic> {
        match self {
            Body::Position { topic, .. } => Some(*topic),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub at: Day,
    pub body: Body,
    /// How it was known.
    pub how: Option<Quoted>,
    pub standing: Standing,
    /// Topics stated at write time. [`Note::topics`] adds what the term table finds in its text.
    pub stated_topics: Topics,
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
}

/// The record, as far as ranking reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Case {
    pub title: Quoted,
    pub facts: Facts,
    pub balance: Vec<Balance>,
    pub notes: Vec<Note>,
}

impl Case {
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
        kept().find(|b| matches!(b.standing, Standing::Confirmed { .. })).or_else(|| kept().next()).map(|b| (b.stance, b.standing))
    }

    /// The facts, with the stance and year as of `on`.
    pub fn applies(&self, on: Day) -> Applies {
        let Facts { kind, side, paper, theirs, law } = self.facts;
        Applies { kind, side, paper, theirs, law, stance: self.stance_on(on), year: Some(on.year()) }
    }
}

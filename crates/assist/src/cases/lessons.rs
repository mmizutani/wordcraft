//! Lessons that cross clients (#107, decision 2). A [`KeptLesson`] is the one thing recall reads
//! from outside the bound case's wall: text the person approved on a card, with the coarse
//! [`Applies`] they left ticked, and nothing else. It is `Copy` facts and one `Quoted`, so a party's
//! name reaches another client's turn only if the person kept it after the card flagged it
//! ([`names_in`]). The file is `assistant-lessons.json` in the settings folder, beside
//! `assistant-cases/`, read and written by the shelf under its own lock ([`super::store`]).

use serde::{Deserialize, Serialize};

use super::record::{Applies, Day, MAX_TEXT, Quoted, VERSION};
use super::store::CaseId;
use super::text::tokens;

/// The file, in the settings folder.
pub const FILE: &str = "assistant-lessons.json";

/// Most lessons kept. Past it Keep is refused until one is removed.
pub const MAX_LESSONS: usize = 400;

/// Most bytes of the file: a file holding [`MAX_LESSONS`] lessons at [`MAX_TEXT`] three-byte
/// characters fits.
pub const MAX_FILE: u64 = 1 << 20;

/// A lesson's id, `l3`. Numbers are never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct LessonId(u64);

const MAX_ID_NUMBER: u64 = 1 << 53;

impl LessonId {
    pub fn parse(text: &str) -> Option<LessonId> {
        let n = text.strip_prefix('l')?;
        if n.is_empty() || n.len() > 16 || !n.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        n.parse().ok().filter(|n| *n <= MAX_ID_NUMBER).map(LessonId)
    }
}

impl TryFrom<String> for LessonId {
    type Error = String;
    fn try_from(text: String) -> Result<LessonId, String> {
        LessonId::parse(&text).ok_or_else(|| "a lesson id is l3".to_string())
    }
}

impl From<LessonId> for String {
    fn from(id: LessonId) -> String {
        id.to_string()
    }
}

impl std::fmt::Display for LessonId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "l{}", self.0)
    }
}

/// A lesson the person kept. It crosses walls, so it holds its text and `Applies` and nothing
/// that names a case to a model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KeptLesson {
    pub id: LessonId,
    pub text: Quoted,
    #[serde(default)]
    pub applies: Applies,
    #[serde(default)]
    pub topics: super::record::Topics,
    #[serde(default)]
    pub off: bool,
    /// The case it came from, on this computer only: never sent to a model, and cleared when that
    /// case is deleted and the lesson kept.
    #[serde(default)]
    from: Option<CaseId>,
    #[serde(default)]
    pub used: u64,
    #[serde(default)]
    pub last_used: Option<Day>,
}

impl KeptLesson {
    /// Whether it came from `case`. The id itself stays private to this module and the shelf.
    pub fn is_from(&self, case: &CaseId) -> bool {
        self.from.as_ref() == Some(case)
    }
}

/// What the person keeps: the text as the card showed it and the facts left ticked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LessonDraft {
    pub text: Quoted,
    #[serde(default)]
    pub applies: Applies,
    #[serde(default)]
    pub topics: super::record::Topics,
    #[serde(default)]
    pub from: Option<CaseId>,
}

/// A lesson a model proposes in a `[[case]]` marker, or the person asks for from a note's row:
/// always a card, never a write. `notes` are the notes of the conversation's case it rests on; the
/// card takes the facts as of the first one found, else today.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LessonProposal {
    pub text: Quoted,
    #[serde(default)]
    pub notes: Vec<super::record::NoteId>,
}

/// Everything the person can do to the lessons. There is no model op: a model proposes a lesson
/// through a card and writes nothing here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LessonOp {
    Keep(LessonDraft),
    Edit(LessonId, String),
    Remove(LessonId),
    Off(LessonId, bool),
    /// The case was deleted: its lessons go too, or stay and lose the link.
    CaseDeleted {
        case: CaseId,
        delete: bool,
    },
}

/// The file as this build reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Lessons {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    pub lessons: Vec<KeptLesson>,
    #[serde(default)]
    last_id: u64,
    /// The file held more than this build reads, or can't be read: the person's next write moves
    /// it aside first, and recall sends no lesson. Never written.
    #[serde(skip)]
    pub damaged: bool,
}

impl Default for Lessons {
    /// No file yet: empty, at this build's version, so the first write reads back.
    fn default() -> Lessons {
        Lessons { version: VERSION, lessons: Vec::new(), last_id: 0, damaged: false }
    }
}

impl Lessons {
    pub(crate) fn read(bytes: Option<&[u8]>) -> Lessons {
        let damaged = || Lessons { damaged: true, ..Lessons::default() };
        let Some(bytes) = bytes else { return damaged() };
        let mut lessons = match serde_json::from_slice::<Lessons>(bytes) {
            Ok(l) if l.version == VERSION => l,
            _ => return damaged(),
        };
        lessons.damaged = lessons.clamp();
        lessons
    }

    pub(crate) fn to_bytes(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|e| e.to_string())
    }

    /// Hold everything to the limits; whether anything went.
    fn clamp(&mut self) -> bool {
        let mut cut = self.lessons.len() > MAX_LESSONS;
        self.lessons.truncate(MAX_LESSONS);
        for l in &mut self.lessons {
            cut |= l.text.cut(MAX_TEXT);
        }
        let mut seen = std::collections::HashSet::new();
        let before = self.lessons.len();
        self.lessons.retain(|l| seen.insert(l.id));
        cut |= self.lessons.len() != before;
        if self.last_id > MAX_ID_NUMBER {
            self.last_id = MAX_ID_NUMBER;
            cut = true;
        }
        self.last_id = self.last_id.max(self.lessons.iter().map(|l| l.id.0).max().unwrap_or(0));
        cut
    }

    /// The lessons recall may send: those not off.
    pub(crate) fn on(&self) -> impl Iterator<Item = &KeptLesson> {
        self.lessons.iter().filter(|l| !l.off)
    }

    /// The lessons kept from `case`, for the delete prompt.
    pub fn from_case<'a>(&'a self, case: &'a CaseId) -> impl Iterator<Item = &'a KeptLesson> {
        self.lessons.iter().filter(move |l| l.is_from(case))
    }

    fn lesson_mut(&mut self, id: LessonId) -> Result<&mut KeptLesson, super::record::CaseError> {
        self.lessons.iter_mut().find(|l| l.id == id).ok_or(super::record::CaseError::NoSuchLesson(id))
    }

    pub(crate) fn apply(&mut self, op: LessonOp) -> Result<Option<LessonId>, super::record::CaseError> {
        use super::record::CaseError;
        match op {
            LessonOp::Keep(draft) => {
                let LessonDraft { mut text, applies, topics, from } = draft;
                if text.raw().trim().is_empty() || text.cut(MAX_TEXT) {
                    return Err(CaseError::TooLong);
                }
                if self.lessons.len() >= MAX_LESSONS {
                    return Err(CaseError::Full("lessons"));
                }
                let next = self.last_id.saturating_add(1);
                if next > MAX_ID_NUMBER {
                    return Err(CaseError::Full("lesson ids"));
                }
                self.last_id = next;
                let id = LessonId(next);
                self.lessons.push(KeptLesson { id, text, applies, topics, off: false, from, used: 0, last_used: None });
                Ok(Some(id))
            }
            LessonOp::Edit(id, text) => {
                let mut text = Quoted::parse(&text);
                if text.raw().trim().is_empty() || text.cut(MAX_TEXT) {
                    return Err(CaseError::TooLong);
                }
                self.lesson_mut(id)?.text = text;
                Ok(None)
            }
            LessonOp::Remove(id) => {
                let i = self.lessons.iter().position(|l| l.id == id).ok_or(CaseError::NoSuchLesson(id))?;
                self.lessons.remove(i);
                Ok(None)
            }
            LessonOp::Off(id, off) => {
                self.lesson_mut(id)?.off = off;
                Ok(None)
            }
            LessonOp::CaseDeleted { case, delete } => {
                if delete {
                    self.lessons.retain(|l| !l.is_from(&case));
                } else {
                    for l in self.lessons.iter_mut().filter(|l| l.is_from(&case)) {
                        l.from = None;
                    }
                }
                Ok(None)
            }
        }
    }
}

/// The words of `text` that name a party of the case, as the card shows them: a party's whole
/// name found in the text, or any word of a party's name found as a word of the text, both
/// folded as the tokenizer folds (case and full-width ASCII). A flag, not a wall: the person may
/// keep the text with the name after a second confirmation.
pub fn names_in(text: &str, parties: &[Quoted]) -> Vec<String> {
    let folded = super::text::folded(text);
    let words: std::collections::HashSet<String> = tokens(text).into_iter().map(|t| t.into_string()).collect();
    let mut out: Vec<String> = Vec::new();
    for party in parties {
        let name = super::text::folded(party.raw());
        if name.is_empty() {
            continue;
        }
        if folded.contains(&name) {
            out.push(name.clone());
            continue;
        }
        for word in tokens(party.raw()) {
            let word = word.into_string();
            if words.contains(&word) {
                out.push(word);
            }
        }
    }
    out.sort();
    out.dedup();
    out.truncate(16);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_party_s_name_is_flagged_folded_and_other_words_are_not() {
        let parties = [Quoted::parse("Acme Corp"), Quoted::parse("ＨＡＲＢＯＵＲ Line KK")];
        assert_eq!(names_in("Ask ACME for the SLA credit before the cap.", &parties), ["acme"]);
        assert_eq!(names_in("harbour wants the cap first.", &parties), ["harbour"]);
        assert_eq!(names_in("Ask for the SLA credit before the cap.", &parties), Vec::<String>::new());
        assert_eq!(names_in("acme corp signed", &parties), ["acme corp"]);
    }

    #[test]
    fn a_kept_lesson_s_file_has_no_size_key_and_one_with_it_reads_as_damaged() {
        let mut lessons = Lessons::default();
        let draft = LessonDraft {
            text: Quoted::parse("Ask for the SLA credit before the cap."),
            applies: Applies { kind: Some(super::super::record::ContractKind::Saas), year: Some(2025), ..Applies::default() },
            topics: Default::default(),
            from: None,
        };
        lessons.apply(LessonOp::Keep(draft)).unwrap();
        let bytes = lessons.to_bytes().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let applies = json["lessons"][0]["applies"].as_object().unwrap();
        assert!(!applies.contains_key("size"), "{applies:?}");
        assert_eq!(applies["year"], 2025);
        assert!(!Lessons::read(Some(&bytes)).damaged);
        let with_size = String::from_utf8(bytes).unwrap().replace(r#""year":2025"#, r#""year":2025,"size":"huge""#);
        assert!(Lessons::read(Some(with_size.as_bytes())).damaged, "{with_size}");
    }
}

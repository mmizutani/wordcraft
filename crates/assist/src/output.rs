//! What a conversation changed in the document, for the pane's Output: its tracked changes and
//! comments, from the same walks the Review pane lists them with, by the name the assistant joined
//! under. The list forgets a change once it is resolved and a change has no id of its own (one
//! revision holds all an author did in a second), so the transcript remembers each as it last stood
//! and this works out which is which. How a change ended is read from the text afterwards.

use serde::Serialize;
use serde_json::{Value, json};
use wordcraft_doc::Document;
use wordcraft_doc::change::Anchor;
use wordcraft_doc::para::OBJ;
use wordcraft_engine::Session;
use wordcraft_engine::cmd::review::{change_rows, comment_rows};

/// Most changes the list holds: a rewrite of a long document can make thousands, which would push
/// the conversation out of the transcript. The rest are counted.
pub const MAX_ROWS: usize = 256;

/// Most comments the list holds.
const MAX_COMMENTS: usize = 64;

/// Most bytes of a paragraph looked through for a change's words.
const MAX_PARAGRAPH: usize = 64 << 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum OutputKind {
    Insert,
    Delete,
    Format,
    Move,
    Comment,
}

/// How a change or a comment stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum OutputState {
    /// Waiting for someone to accept or reject it, or an open comment.
    Open,
    Accepted,
    Rejected,
    /// A resolved comment, or a change whose text can't say which way it went.
    Resolved,
    /// A comment that's gone.
    Deleted,
}

/// One change or comment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputRow {
    pub kind: OutputKind,
    /// Its text, cut short.
    pub text: String,
    pub state: OutputState,
    /// Where it is, by paragraph id, as it last stood.
    pub start: Option<Anchor>,
    pub end: Option<Anchor>,
    /// A comment's id.
    pub comment: Option<u32>,
    /// How many times its words were in its paragraph as it last stood; what [`ended`] compares.
    /// `None` when the paragraph was too long to look through each time the document changes.
    #[serde(skip)]
    pub in_paragraph: Option<u32>,
}

impl OutputRow {
    /// What `review.accept` and `review.reject` take for it alone, `name`'s: one text can carry a
    /// change of each kind, and a move's two ends go together.
    pub fn resolve_params(&self, name: &str) -> Value {
        let kind = match self.kind {
            OutputKind::Insert => Some("insert"),
            OutputKind::Delete => Some("delete"),
            OutputKind::Format => Some("format"),
            OutputKind::Move | OutputKind::Comment => None,
        };
        json!({"author": name, "kind": kind})
    }

    /// `select.range`'s parameters for it.
    pub fn range(&self) -> Option<Value> {
        let (start, end) = (self.start?, self.end?);
        Some(json!({"anchor": start, "focus": end}))
    }
}

/// `name`'s changes and comments in the document, and how many more changes it has than they hold.
pub(crate) fn rows(session: &Session, name: &str) -> (Vec<OutputRow>, usize) {
    let doc = &session.doc;
    let (changes, more) = change_rows(session, name, MAX_ROWS);
    let changes = changes.into_iter().map(|c| OutputRow {
        // None when it isn't in one paragraph: a change across paragraphs can't be counted in one.
        in_paragraph: occurrences(doc, doc.anchor_at(&c.a), &c.text).filter(|n| *n > 0),
        kind: match c.kind {
            "insert" => OutputKind::Insert,
            "delete" => OutputKind::Delete,
            "moveFrom" | "moveTo" => OutputKind::Move,
            _ => OutputKind::Format,
        },
        comment: None,
        start: doc.anchor_at(&c.a),
        end: doc.anchor_at(&c.b),
        text: c.text,
        state: OutputState::Open,
    });
    let (comments, more_comments) = comment_rows(session, name, MAX_COMMENTS);
    let comments = comments.into_iter().map(|c| {
        let at = c.at.as_ref().and_then(|p| doc.anchor_at(p));
        OutputRow {
            kind: OutputKind::Comment,
            text: c.text,
            state: if c.resolved { OutputState::Resolved } else { OutputState::Open },
            start: at,
            end: at,
            comment: Some(c.id),
            in_paragraph: None,
        }
    });
    (changes.chain(comments).collect(), more.saturating_add(more_comments))
}

/// How many times `words` are in the paragraph of `at`: 0 once it is gone. `None` when there is
/// nothing to count, or the paragraph is too long to look through.
fn occurrences(doc: &Document, at: Option<Anchor>, words: &str) -> Option<u32> {
    if words.is_empty() {
        return None;
    }
    let Some(at) = at.and_then(|at| doc.pos_of(at, None)) else { return Some(0) };
    let text = doc.para(at.story, &at.path).map_or("", |p| p.text.as_str());
    if text.len() > MAX_PARAGRAPH {
        return None;
    }
    let count = if text.contains(OBJ) { text.replace(OBJ, "").matches(words).count() } else { text.matches(words).count() };
    Some(u32::try_from(count).unwrap_or(u32::MAX))
}

/// How change `row`, gone from the list, ended, read from `doc`. Its words were in its paragraph
/// once, and now they aren't: they went, so an insertion was rejected and a deletion accepted. Still
/// once: they stayed, the other way round. With its words there more than once (a deletion beside
/// the insertion that replaces it, say), the count can't tell whose went. That, a formatting change,
/// a move, or a change with no words says only that it was resolved.
pub(crate) fn ended(doc: &Document, row: &OutputRow) -> OutputState {
    let (Some(1), true) = (row.in_paragraph, matches!(row.kind, OutputKind::Insert | OutputKind::Delete)) else {
        return OutputState::Resolved;
    };
    let Some(now) = occurrences(doc, row.start, &row.text) else { return OutputState::Resolved };
    let went = now == 0;
    if went == (row.kind == OutputKind::Delete) { OutputState::Accepted } else { OutputState::Rejected }
}

/// What to do with a row of the transcript's list.
pub(crate) enum Action {
    /// Put this at that index.
    Write(usize, OutputRow),
    /// Add this.
    New(OutputRow),
    /// Take away the row at that index.
    Drop(usize),
}

/// What to do with the transcript's `known` rows (with their indexes) given the rows `seen` in the
/// document now. A change has no id, so the same change is the same kind and words in the same
/// paragraph, the nearest such; then, if it is still open, one whose words hold the other's or are
/// held by them, as a change grows while its turn types; then the same words in another paragraph,
/// where a new paragraph mark took them. With rows past the limit, a row missing from `seen` may
/// only have moved past it: it goes from the list and counts with them, and comes back when the
/// list has room.
pub(crate) fn reconcile(known: &[(usize, OutputRow)], seen: Vec<OutputRow>, more: usize, ended: impl Fn(&OutputRow) -> OutputState) -> Vec<Action> {
    let mut taken = vec![false; known.len()];
    let mut out = Vec::new();
    let mut left = seen;
    for matches in [Match::SameWords, Match::Grown, Match::Moved] {
        let mut unmatched = Vec::new();
        for row in left {
            match nearest(known, &taken, &row, matches) {
                Some(n) => take(n, known, &mut taken, row, &mut out),
                None => unmatched.push(row),
            }
        }
        left = unmatched;
    }
    out.extend(left.into_iter().map(Action::New));
    for (n, (i, k)) in known.iter().enumerate() {
        if taken.get(n).copied().unwrap_or(true) {
            continue;
        }
        match (k.kind, k.state) {
            (OutputKind::Comment, OutputState::Deleted) => {}
            (_, OutputState::Open | OutputState::Resolved) if more > 0 && (k.kind == OutputKind::Comment || k.state == OutputState::Open) => {
                out.push(Action::Drop(*i));
            }
            (OutputKind::Comment, _) => out.push(Action::Write(*i, OutputRow { state: OutputState::Deleted, ..k.clone() })),
            (_, OutputState::Open) => out.push(Action::Write(*i, OutputRow { state: ended(k), ..k.clone() })),
            _ => {}
        }
    }
    out
}

/// How far a row may be from one known and still be the same change.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Match {
    SameWords,
    Grown,
    Moved,
}

/// The known row that is `row`, not taken yet: an open one first, then the nearest.
fn nearest(known: &[(usize, OutputRow)], taken: &[bool], row: &OutputRow, matches: Match) -> Option<usize> {
    let off = |r: &OutputRow| r.start.map_or(0, |a| a.off);
    known
        .iter()
        .enumerate()
        .filter(|(n, (_, k))| !taken.get(*n).copied().unwrap_or(true) && same(k, row, matches))
        .min_by_key(|(_, (_, k))| (k.state != OutputState::Open, off(k).abs_diff(off(row))))
        .map(|(n, _)| n)
}

/// Row `n` of `known` is `row` now: write it if it changed.
fn take(n: usize, known: &[(usize, OutputRow)], taken: &mut [bool], row: OutputRow, out: &mut Vec<Action>) {
    if let Some(flag) = taken.get_mut(n) {
        *flag = true;
    }
    if let Some((i, k)) = known.get(n)
        && *k != row
    {
        out.push(Action::Write(*i, row));
    }
}

/// Whether `known` and `row` are the same change or comment. A row that ended comes back only for
/// the same words in the same paragraph: Undo brought it back.
fn same(known: &OutputRow, row: &OutputRow, matches: Match) -> bool {
    if known.comment.is_some() || row.comment.is_some() {
        return known.comment == row.comment;
    }
    let same_block = known.start.map(|a| a.block) == row.start.map(|a| a.block);
    let open = known.state == OutputState::Open;
    let words = !known.text.is_empty() && !row.text.is_empty();
    known.kind == row.kind
        && match matches {
            Match::SameWords => same_block && known.text == row.text,
            Match::Grown => same_block && open && words && (known.text.contains(&row.text) || row.text.contains(&known.text)),
            Match::Moved => open && words && known.text == row.text,
        }
}

#[cfg(test)]
mod tests;

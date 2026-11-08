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
}

impl OutputRow {
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
    let comments = comment_rows(session, name, MAX_COMMENTS).into_iter().map(|c| {
        let at = c.at.as_ref().and_then(|p| doc.anchor_at(p));
        OutputRow {
            kind: OutputKind::Comment,
            text: c.text,
            state: if c.resolved { OutputState::Resolved } else { OutputState::Open },
            start: at,
            end: at,
            comment: Some(c.id),
        }
    });
    (changes.chain(comments).collect(), more)
}

/// How change `row`, gone from the list, ended, read from `doc`: an insertion whose words are still
/// where it was was accepted, a deletion whose words are still there was rejected, and either way
/// round for words gone. A formatting change, a move, or one with no words can't say.
pub(crate) fn ended(doc: &Document, row: &OutputRow) -> OutputState {
    if matches!(row.kind, OutputKind::Format | OutputKind::Move) || row.text.is_empty() {
        return OutputState::Resolved;
    }
    let still_there = row.start.and_then(|at| doc.pos_of(at, None)).is_some_and(|at| {
        let text = doc.para(at.story, &at.path).and_then(|p| p.text.get(at.off..)).unwrap_or_default();
        let mut shown = text.chars().filter(|c| *c != OBJ);
        row.text.chars().all(|c| shown.next() == Some(c))
    });
    let insertion = row.kind == OutputKind::Insert;
    if still_there == insertion { OutputState::Accepted } else { OutputState::Rejected }
}

/// What to do with the transcript's `known` rows (with their indexes) given the rows `seen` in the
/// document now: each row to write, at its index, or new. A change has no id, so one that is the
/// same kind in the same paragraph with the same words is the same one, then one whose words hold
/// the other's or are held by them: a change grows as its turn types on.
pub(crate) fn reconcile(
    known: &[(usize, OutputRow)],
    seen: Vec<OutputRow>,
    more: usize,
    ended: impl Fn(&OutputRow) -> OutputState,
) -> Vec<(Option<usize>, OutputRow)> {
    let mut taken = vec![false; known.len()];
    let mut out = Vec::new();
    let mut unmatched = Vec::new();
    // Exact matches first, so a later partial match can't take an exact one's row.
    for row in seen {
        match known.iter().enumerate().position(|(n, (_, k))| !taken.get(n).copied().unwrap_or(true) && same(k, &row, true)) {
            Some(n) => take(n, known, &mut taken, row, &mut out),
            None => unmatched.push(row),
        }
    }
    for row in unmatched {
        match known.iter().enumerate().position(|(n, (_, k))| !taken.get(n).copied().unwrap_or(true) && same(k, &row, false)) {
            Some(n) => take(n, known, &mut taken, row, &mut out),
            None => out.push((None, row)),
        }
    }
    for (n, (i, k)) in known.iter().enumerate() {
        if taken.get(n).copied().unwrap_or(true) {
            continue;
        }
        let state = match (k.kind, k.state) {
            (OutputKind::Comment, OutputState::Deleted) => continue,
            (OutputKind::Comment, _) => OutputState::Deleted,
            (_, OutputState::Open) if more == 0 => ended(k),
            _ => continue,
        };
        out.push((Some(*i), OutputRow { state, ..k.clone() }));
    }
    out
}

/// Row `n` of `known` is `row` now: write it if it changed.
fn take(n: usize, known: &[(usize, OutputRow)], taken: &mut [bool], row: OutputRow, out: &mut Vec<(Option<usize>, OutputRow)>) {
    if let Some(flag) = taken.get_mut(n) {
        *flag = true;
    }
    if let Some((i, k)) = known.get(n)
        && *k != row
    {
        out.push((Some(*i), row));
    }
}

/// Whether `known` and `row` are the same change or comment. A row that ended comes back only for
/// the very same words: Undo brought it back.
fn same(known: &OutputRow, row: &OutputRow, exactly: bool) -> bool {
    if known.comment.is_some() || row.comment.is_some() {
        return known.comment == row.comment;
    }
    let block = |r: &OutputRow| r.start.map(|a| a.block);
    if known.kind != row.kind || block(known) != block(row) {
        return false;
    }
    if exactly {
        return known.text == row.text;
    }
    known.state == OutputState::Open
        && !known.text.is_empty()
        && !row.text.is_empty()
        && (known.text.contains(&row.text) || row.text.contains(&known.text))
}

#[cfg(test)]
mod tests;

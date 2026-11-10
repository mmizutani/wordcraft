//! What the model writes in its answer for WordCraft rather than for the person (#86, #107): the
//! markers `[[remember: …]]`, a proposal to keep a preference, `[[used: m1]]`, which preference or
//! recalled item shaped this answer, and `[[case]]{json}[[/case]]`, a note for the conversation's
//! case. A marker works on every backend, adds no route into the window and costs no extra
//! request, and it carries no authority of its own: a document can make a model write one, so a
//! proposal is a card the person answers, and a case note is stored unconfirmed in the
//! conversation's own case and nowhere else ([`crate::cases::marker`]).
//!
//! Markers are parsed from answers only, never from thoughts, and stripped from what the pane
//! shows, including while one is split across pieces of the stream.

const REMEMBER: &str = "[[remember:";
const USED: &str = "[[used:";
const CLOSE: &str = "]]";
const CASE: &str = "[[case]]";
/// A case marker closes with its own tag, so its JSON may hold `]]`.
const CASE_CLOSE: &str = "[[/case]]";

/// Most bytes held back waiting for a marker to end: what starts like one and goes on longer isn't.
const MAX_HELD: usize = 2 << 10;

/// Most bytes of a case marker. Past this it is dropped and counted, never shown: half a note in
/// the answer would read as text.
pub const MAX_CASE: usize = 8 << 10;

/// A piece of an answer, in the order it came.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Piece {
    Text(String),
    Remember(String),
    Used(String),
    /// The body of a `[[case]]…[[/case]]` marker, unparsed.
    Case(String),
    /// A case marker too long to keep, or unclosed when the answer ended.
    CaseDropped,
}

/// The start of a marker held back until the rest of it comes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Markers {
    held: String,
    /// A case marker ran past [`MAX_CASE`]: everything up to its close is thrown away.
    dropping: bool,
}

impl Markers {
    /// Take the next piece of an answer.
    pub(crate) fn feed(&mut self, text: &str) -> Vec<Piece> {
        let mut out = Vec::new();
        self.held.push_str(text);
        if self.dropping {
            let all = std::mem::take(&mut self.held);
            match all.find(CASE_CLOSE) {
                Some(end) => {
                    self.dropping = false;
                    self.held = all.get(end.saturating_add(CASE_CLOSE.len())..).unwrap_or_default().to_string();
                }
                None => {
                    // Only a close's start could straddle this piece and the next.
                    self.held = tail_prefix_of(&all, CASE_CLOSE).to_string();
                    return out;
                }
            }
        }
        let all = std::mem::take(&mut self.held);
        let mut shown = String::new();
        let mut rest = all.as_str();
        while let Some(at) = rest.find('[') {
            let (before, from) = rest.split_at(at);
            shown.push_str(before);
            if from.starts_with(CASE) {
                match from.find(CASE_CLOSE) {
                    Some(end) if end <= MAX_CASE => {
                        let body = from.get(CASE.len()..end).unwrap_or_default().trim().to_string();
                        if !shown.is_empty() {
                            out.push(Piece::Text(std::mem::take(&mut shown)));
                        }
                        out.push(Piece::Case(body));
                        rest = from.get(end.saturating_add(CASE_CLOSE.len())..).unwrap_or_default();
                    }
                    Some(end) => {
                        if !shown.is_empty() {
                            out.push(Piece::Text(std::mem::take(&mut shown)));
                        }
                        out.push(Piece::CaseDropped);
                        rest = from.get(end.saturating_add(CASE_CLOSE.len())..).unwrap_or_default();
                    }
                    None if from.len() <= MAX_CASE => {
                        self.held = from.to_string();
                        rest = "";
                    }
                    None => {
                        if !shown.is_empty() {
                            out.push(Piece::Text(std::mem::take(&mut shown)));
                        }
                        out.push(Piece::CaseDropped);
                        self.dropping = true;
                        self.held = tail_prefix_of(from, CASE_CLOSE).to_string();
                        rest = "";
                    }
                }
                continue;
            }
            let opener = [REMEMBER, USED].into_iter().find(|o| from.starts_with(o));
            if let Some(opener) = opener {
                match from.find(CLOSE) {
                    Some(end) if end <= MAX_HELD => {
                        let inner = from.get(opener.len()..end).unwrap_or_default().trim().to_string();
                        if !shown.is_empty() {
                            out.push(Piece::Text(std::mem::take(&mut shown)));
                        }
                        out.push(if opener == REMEMBER { Piece::Remember(inner) } else { Piece::Used(inner) });
                        rest = from.get(end.saturating_add(CLOSE.len())..).unwrap_or_default();
                        continue;
                    }
                    None if from.len() <= MAX_HELD => {
                        self.held = from.to_string();
                        rest = "";
                        break;
                    }
                    _ => {}
                }
            } else if REMEMBER.starts_with(from) || USED.starts_with(from) || CASE.starts_with(from) {
                self.held = from.to_string();
                rest = "";
                break;
            }
            shown.push('[');
            rest = from.get(1..).unwrap_or_default();
        }
        shown.push_str(rest);
        if !shown.is_empty() {
            out.push(Piece::Text(shown));
        }
        out
    }

    /// The answer stopped or something else came: what was held back was only text, except an
    /// unclosed case marker, which is dropped and counted rather than shown as half a note.
    pub(crate) fn flush(&mut self) -> Vec<Piece> {
        let held = std::mem::take(&mut self.held);
        if std::mem::take(&mut self.dropping) {
            return vec![Piece::CaseDropped];
        }
        if held.starts_with(CASE) {
            return vec![Piece::CaseDropped];
        }
        if held.is_empty() { Vec::new() } else { vec![Piece::Text(held)] }
    }
}

/// The longest tail of `text` that is a proper prefix of `pattern`, on a char boundary.
fn tail_prefix_of<'a>(text: &'a str, pattern: &str) -> &'a str {
    (1..pattern.len())
        .rev()
        .filter_map(|n| text.len().checked_sub(n).and_then(|at| text.get(at..)))
        .find(|tail| pattern.starts_with(tail))
        .unwrap_or_default()
}

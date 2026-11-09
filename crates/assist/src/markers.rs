//! What the model writes in its answer for WordCraft rather than for the person (#86): the markers
//! `[[remember: …]]`, a proposal to keep a preference, and `[[used: m1]]`, which preference shaped
//! this answer. A marker works on every backend, adds no route into the window and costs no extra
//! request, and it carries no authority: a document can make a model write one, so a proposal is
//! only ever a card the person answers.
//!
//! Markers are parsed from answers only, never from thoughts, and stripped from what the pane
//! shows, including while one is split across pieces of the stream.

const REMEMBER: &str = "[[remember:";
const USED: &str = "[[used:";
const CLOSE: &str = "]]";

/// Most bytes held back waiting for a marker to end: what starts like one and goes on longer isn't.
const MAX_HELD: usize = 2 << 10;

/// A piece of an answer, in the order it came.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Piece {
    Text(String),
    Remember(String),
    Used(String),
}

/// The start of a marker held back until the rest of it comes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Markers {
    held: String,
}

impl Markers {
    /// Take the next piece of an answer.
    pub(crate) fn feed(&mut self, text: &str) -> Vec<Piece> {
        self.held.push_str(text);
        let all = std::mem::take(&mut self.held);
        let mut out = Vec::new();
        let mut shown = String::new();
        let mut rest = all.as_str();
        while let Some(at) = rest.find('[') {
            let (before, from) = rest.split_at(at);
            shown.push_str(before);
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
            } else if REMEMBER.starts_with(from) || USED.starts_with(from) {
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

    /// The answer stopped or something else came: what was held back was only text.
    pub(crate) fn flush(&mut self) -> String {
        std::mem::take(&mut self.held)
    }
}

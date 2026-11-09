//! A conversation as the pane shows it: what the person asked, what the assistant answered, its
//! tool calls, plans and permission requests, and how each turn ended. Held to a size: the
//! oldest entries go first.

use std::collections::VecDeque;

use serde::Serialize;

use crate::backend::{Answer, Event, PermissionRequest, PlanEntry, StopReason, ToolCall};
use crate::markers::{Markers, Piece};

/// Most entries a transcript keeps.
pub const MAX_ENTRIES: usize = 2048;

/// Most bytes of text a transcript keeps, roughly.
pub const MAX_BYTES: usize = 4 << 20;

/// Most bytes of text one answer or thought keeps: what streams in after it is left out, and
/// the entry ends with "…".
pub const MAX_ENTRY_BYTES: usize = 256 << 10;

/// Most proposals one turn shows; more are dropped.
pub const MAX_PROPOSALS: u8 = 3;

/// What the person said to a proposal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProposalAnswer {
    Kept,
    Dismissed,
}

/// One entry of a transcript.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "entry")]
pub enum Entry {
    /// What the person asked.
    Asked { text: String },
    /// The assistant's answer (its pieces, put together).
    Answer { text: String },
    /// What the agent thought.
    Thought { text: String },
    /// A tool call, as it stands.
    Tool(ToolCall),
    /// The agent's plan, as it stands.
    Plan { entries: Vec<PlanEntry> },
    /// A permission request, and its answer once given.
    Permission { request: PermissionRequest, answer: Option<Answer> },
    /// The model proposes that the person keep a preference ([`crate::memory`]). It carries no
    /// authority: only the person's click on the card stores anything.
    Proposal { id: u64, text: String, answer: Option<ProposalAnswer> },
    /// Which model answers and at what level ([`Event::Model`]), as it last said.
    Model { name: String, effort: Option<String>, asked: bool },
    /// Something the person should know about the turn ([`Event::Note`]).
    Note { text: String },
    /// The turn stopped other than by finishing its answer.
    Stopped { stop: StopReason },
    /// The turn failed.
    Error { message: String },
}

impl Entry {
    fn bytes(&self) -> usize {
        match self {
            Entry::Asked { text }
            | Entry::Answer { text }
            | Entry::Thought { text }
            | Entry::Note { text }
            | Entry::Proposal { text, .. }
            | Entry::Error { message: text } => text.len(),
            Entry::Model { name, effort, .. } => name.len() + effort.as_ref().map_or(0, String::len),
            Entry::Tool(t) => t.title.len() + t.detail.len() + t.id.len(),
            Entry::Plan { entries } => entries.iter().map(|e| e.content.len()).sum(),
            Entry::Permission { request, .. } => request.title.len() + request.options.iter().map(|o| o.name.len() + o.id.len()).sum::<usize>(),
            Entry::Stopped { .. } => 0,
        }
        .saturating_add(16)
    }
}

/// A conversation's entries, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Transcript {
    entries: VecDeque<Entry>,
    /// What the entries weigh ([`Entry::bytes`]).
    #[serde(skip)]
    bytes: usize,
    /// Where the current turn's entries start, counted from the front.
    #[serde(skip)]
    turn_start: usize,
    /// The start of a marker waiting for the rest of it.
    #[serde(skip)]
    markers: Markers,
    /// The last proposal's number.
    #[serde(skip)]
    last_proposal: u64,
    /// How many proposals the current turn has shown.
    #[serde(skip)]
    proposed: u8,
}

impl Transcript {
    /// The entries, oldest first.
    pub fn entries(&self) -> impl ExactSizeIterator<Item = &Entry> + DoubleEndedIterator {
        self.entries.iter()
    }

    /// How many entries it has.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether it has none.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The last turn's answer: its pieces (the text between its tool calls), each trimmed,
    /// joined by blank lines. None when the turn gave no text.
    pub fn last_answer(&self) -> Option<String> {
        let pieces: Vec<&str> = self
            .entries
            .iter()
            .skip(self.turn_start)
            .filter_map(|e| match e {
                Entry::Answer { text } => Some(text.trim()),
                _ => None,
            })
            .filter(|t| !t.is_empty())
            .collect();
        if pieces.is_empty() { None } else { Some(pieces.join("\n\n")) }
    }

    /// The person asked `text`: a new turn starts.
    pub fn asked(&mut self, text: &str) {
        self.flush_markers();
        self.turn_start = self.entries.len();
        self.proposed = 0;
        self.push(Entry::Asked { text: text.to_string() });
    }

    /// Add what `event` says.
    pub fn record(&mut self, event: &Event) {
        if !matches!(event, Event::Text { .. }) {
            self.flush_markers();
        }
        match event {
            Event::Text { text } => {
                for piece in self.markers.feed(text) {
                    match piece {
                        Piece::Text(t) => self.answer(&t),
                        Piece::Remember(t) => self.propose(&t),
                        // What shaped the answer is AS7.4's: for now the marker is only taken out.
                        Piece::Used(_) => {}
                    }
                }
            }
            Event::Thought { text } => self.append(text, |e| matches!(e, Entry::Thought { .. }), |t| Entry::Thought { text: t }),
            Event::Tool(call) => {
                let i = self.in_turn().find(|(_, e)| matches!(e, Entry::Tool(t) if t.id == call.id)).map(|(i, _)| i);
                match i {
                    Some(i) => self.replace(i, Entry::Tool(call.clone())),
                    None => self.push(Entry::Tool(call.clone())),
                }
            }
            Event::Plan { entries } => {
                let i = self.in_turn().find(|(_, e)| matches!(e, Entry::Plan { .. })).map(|(i, _)| i);
                match i {
                    Some(i) => self.replace(i, Entry::Plan { entries: entries.clone() }),
                    None => self.push(Entry::Plan { entries: entries.clone() }),
                }
            }
            Event::Permission(request) => self.push(Entry::Permission { request: request.clone(), answer: None }),
            Event::Answered { id, answer } => {
                let i = self.in_turn().find(|(_, e)| matches!(e, Entry::Permission { request, .. } if request.id == *id)).map(|(i, _)| i);
                if let Some(i) = i
                    && let Some(Entry::Permission { request, .. }) = self.entries.get(i).cloned()
                {
                    self.replace(i, Entry::Permission { request, answer: Some(answer.clone()) });
                }
            }
            Event::Model { name, effort, asked } => {
                let line = Entry::Model { name: name.clone(), effort: effort.clone(), asked: *asked };
                let i = self.in_turn().find(|(_, e)| matches!(e, Entry::Model { .. })).map(|(i, _)| i);
                match i {
                    Some(i) => self.replace(i, line),
                    None => self.push(line),
                }
            }
            Event::Note { text } => self.push(Entry::Note { text: text.clone() }),
            // What is offered isn't part of the conversation: [`crate::Assistant::offered`].
            Event::Options(_) => {}
            Event::Done { stop } => {
                if *stop != StopReason::EndTurn {
                    self.push(Entry::Stopped { stop: *stop });
                }
            }
            Event::Error { message } => self.push(Entry::Error { message: message.clone() }),
        }
    }

    /// The proposal `id`'s text, while the person hasn't answered it.
    pub fn pending_proposal(&self, id: u64) -> Option<&str> {
        self.entries.iter().rev().find_map(|e| match e {
            Entry::Proposal { id: i, text, answer: None } if *i == id => Some(text.as_str()),
            _ => None,
        })
    }

    /// The person answered proposal `id`: `kept`, what was stored, which they may have edited.
    pub fn answer_proposal(&mut self, id: u64, answer: ProposalAnswer, kept: Option<&str>) {
        let found = self.entries.iter().enumerate().rev().find_map(|(i, e)| match e {
            Entry::Proposal { id: n, text, .. } if *n == id => Some((i, text.clone())),
            _ => None,
        });
        if let Some((i, text)) = found {
            let text = kept.map_or(text, str::to_string);
            self.replace(i, Entry::Proposal { id, text, answer: Some(answer) });
        }
    }

    /// A piece of the answer. Only white space right after a proposal starts no entry of its own.
    fn answer(&mut self, text: &str) {
        if text.trim().is_empty() && matches!(self.entries.back(), Some(Entry::Proposal { .. })) {
            return;
        }
        self.append(text, |e| matches!(e, Entry::Answer { .. }), |t| Entry::Answer { text: t });
    }

    /// The model proposes `text`, within the most a turn shows. An empty one or a second of the
    /// same text in a turn is dropped. Numbered through the conversation.
    fn propose(&mut self, text: &str) {
        let Some(text) = crate::memory::clean(text) else { return };
        let again = self.in_turn().any(|(_, e)| matches!(e, Entry::Proposal { text: t, .. } if *t == text));
        if self.proposed >= MAX_PROPOSALS || again {
            return;
        }
        self.proposed += 1;
        self.last_proposal = self.last_proposal.saturating_add(1);
        self.push(Entry::Proposal { id: self.last_proposal, text, answer: None });
    }

    /// What was held back as the start of a marker was only text.
    fn flush_markers(&mut self) {
        let held = self.markers.flush();
        if !held.is_empty() {
            self.answer(&held);
        }
    }

    /// The current turn's entries, newest first, with their indexes.
    fn in_turn(&self) -> impl Iterator<Item = (usize, &Entry)> {
        self.entries.iter().enumerate().skip(self.turn_start).rev()
    }

    /// Add `text` to the current turn's last entry if `is` it and it is the newest, else start
    /// one with `make`.
    fn append(&mut self, text: &str, is: impl Fn(&Entry) -> bool, make: impl Fn(String) -> Entry) {
        let newest = self.entries.len().checked_sub(1).filter(|i| *i >= self.turn_start);
        if let Some(i) = newest
            && let Some(e) = self.entries.get_mut(i)
            && is(e)
            && let Entry::Answer { text: t } | Entry::Thought { text: t } = e
        {
            let before = t.len();
            grow(t, text);
            self.bytes = self.bytes.saturating_add(t.len().saturating_sub(before));
            self.trim();
            return;
        }
        let mut t = String::new();
        grow(&mut t, text);
        self.push(make(t));
    }

    fn push(&mut self, e: Entry) {
        self.bytes = self.bytes.saturating_add(e.bytes());
        self.entries.push_back(e);
        self.trim();
    }

    fn replace(&mut self, i: usize, e: Entry) {
        if let Some(slot) = self.entries.get_mut(i) {
            self.bytes = self.bytes.saturating_sub(slot.bytes()).saturating_add(e.bytes());
            *slot = e;
        }
        self.trim();
    }

    /// Drop the oldest entries until it holds at most [`MAX_ENTRIES`] and [`MAX_BYTES`] (the
    /// newest entry stays, whatever it weighs).
    fn trim(&mut self) {
        while self.entries.len() > 1 && (self.entries.len() > MAX_ENTRIES || self.bytes > MAX_BYTES) {
            if let Some(e) = self.entries.pop_front() {
                self.bytes = self.bytes.saturating_sub(e.bytes());
                self.turn_start = self.turn_start.saturating_sub(1);
            }
        }
    }
}

/// Add `more` to `text`, up to [`MAX_ENTRY_BYTES`]: a text that reaches it ends with "…" and
/// takes nothing more.
fn grow(text: &mut String, more: &str) {
    const MORE: &str = "…";
    if text.len().saturating_add(more.len()) <= MAX_ENTRY_BYTES {
        text.push_str(more);
    } else if !text.ends_with(MORE) || text.len() < MAX_ENTRY_BYTES.saturating_sub(MORE.len()) {
        let room = MAX_ENTRY_BYTES.saturating_sub(MORE.len()).saturating_sub(text.len());
        text.push_str(&crate::backend::cut(more.to_string(), room));
        text.push_str(MORE);
    }
}

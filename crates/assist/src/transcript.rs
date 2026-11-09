//! A conversation as the pane shows it: what the person asked, what the assistant answered, its
//! tool calls, plans and permission requests, and how each turn ended. Held to a size: the
//! oldest entries go first.

use std::collections::{HashSet, VecDeque};

use serde::Serialize;

use crate::backend::{Answer, Event, PermissionRequest, PlanEntry, PlanStatus, Source, SourceKind, StopReason, ToolCall, ToolStatus, Usage, label};
use crate::context::Sent;
use crate::output::{Action, OutputRow};

/// Most entries a transcript keeps.
pub const MAX_ENTRIES: usize = 2048;

/// Most bytes of text a transcript keeps, roughly.
pub const MAX_BYTES: usize = 4 << 20;

/// Most bytes of text one answer or thought keeps: what streams in after it is left out, and
/// the entry ends with "…".
pub const MAX_ENTRY_BYTES: usize = 256 << 10;

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
    /// Which model answered.
    Model { name: String },
    /// What the agent says it is. Details shows it; the conversation doesn't.
    Agent { name: String },
    /// What the turn used, as it last said. Details shows it; the conversation doesn't.
    Usage(Usage),
    /// What the turn gave the model of the document. Details shows it; the conversation doesn't.
    Context(Sent),
    /// A change or a comment the conversation made, as it last stood. Details shows it; the
    /// conversation doesn't.
    Output(OutputRow),
    /// The turn stopped other than by finishing its answer.
    Stopped { stop: StopReason },
    /// The turn failed.
    Error { message: String },
}

impl Entry {
    /// Whether only Details shows it: the conversation goes on as if it weren't there, and an
    /// answer that arrives in pieces around it stays one answer.
    pub fn details_only(&self) -> bool {
        matches!(self, Entry::Agent { .. } | Entry::Usage(_) | Entry::Context(_) | Entry::Output(_))
    }

    fn bytes(&self) -> usize {
        match self {
            Entry::Asked { text }
            | Entry::Answer { text }
            | Entry::Thought { text }
            | Entry::Model { name: text }
            | Entry::Agent { name: text }
            | Entry::Error { message: text } => text.len(),
            Entry::Tool(t) => {
                let sources = t.sources.iter().map(|s| s.title.len() + s.target.as_ref().map_or(0, String::len));
                t.title.len() + t.detail.len() + t.id.len() + sources.sum::<usize>()
            }
            Entry::Plan { entries } => entries.iter().map(|e| e.content.len()).sum(),
            Entry::Permission { request, .. } => request.title.len() + request.options.iter().map(|o| o.name.len() + o.id.len()).sum::<usize>(),
            Entry::Usage(u) => u.cost.as_ref().map_or(0, String::len),
            Entry::Context(sent) => sent.document.len() + sent.knowledge.iter().map(|s| s.name.len()).sum::<usize>(),
            Entry::Output(row) => row.text.len(),
            Entry::Stopped { .. } => 0,
        }
        .saturating_add(16)
    }
}

/// Most sources Details lists for one turn.
pub const MAX_TURN_SOURCES: usize = 64;

/// Most characters of a prompt where Details names its turn.
const ASKED_CHARS: usize = 80;

/// How far a step has got: the pane's marks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StepState {
    Waiting,
    Running,
    Done,
    Failed,
}

/// One step of a turn: an entry of its plan, or a tool call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub title: String,
    pub state: StepState,
}

/// One turn, as Details shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnDetails {
    /// What the person asked, on one line and cut short. `None` when trimming took it.
    pub asked: Option<String>,
    /// The plan when the agent gave one, else the tool calls.
    pub steps: Vec<Step>,
    /// What the turn gave the model of the document.
    pub context: Option<Sent>,
    /// What its tool calls read and looked for, in order, each once, at most [`MAX_TURN_SOURCES`].
    pub sources: Vec<Source>,
    /// How many more [`TurnDetails::sources`] leaves out.
    pub more_sources: u32,
    /// The changes and comments that first showed during it, oldest first.
    pub output: Vec<OutputRow>,
    /// What its answers cite, each once, at most [`crate::answer::MAX_CITES`].
    pub citations: Vec<Citation>,
    /// How many citations' words the document and the sources don't have
    /// ([`crate::Assistant::details`] can't tell: the window looks, and leaves those out).
    pub unfound: u32,
}

/// What an answer cites.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Citation {
    pub words: String,
    /// The attached source that has them; `None`: the document, if anything.
    pub source: Option<String>,
}

/// What runs a conversation, which its transcript doesn't hold ([`crate::Assistant::details`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Properties {
    /// The backend's name in the picker.
    pub backend: String,
    /// The name it joined the document under.
    pub name: String,
    /// The shared document's title.
    pub document: String,
    pub role: String,
    /// Whether its edits are tracked changes.
    pub tracked: bool,
}

/// A conversation in summary: what the pane's Details shows, and `assist.details` answers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Details {
    pub properties: Option<Properties>,
    /// Oldest first.
    pub turns: Vec<TurnDetails>,
    /// What the agent says it is, with its version.
    pub agent: Option<String>,
    /// The model that answered last.
    pub model: Option<String>,
    /// What the last turn that said used.
    pub usage: Option<Usage>,
    /// How many more changes there are than the list holds ([`crate::output::MAX_ROWS`]).
    pub more_output: u32,
}

/// The turn [`Transcript::details`] is reading.
#[derive(Default)]
struct TurnBuilder<'a> {
    asked: Option<String>,
    plan: Option<Vec<Step>>,
    calls: Vec<Step>,
    context: Option<Sent>,
    sources: Vec<Source>,
    more_sources: u32,
    output: Vec<OutputRow>,
    citations: Vec<Citation>,
    /// The sources seen, listed or not, so each counts once.
    seen: HashSet<(SourceKind, &'a str, Option<&'a str>)>,
}

impl<'a> TurnBuilder<'a> {
    /// Add the turn to `turns`, if it has anything to show, and start another.
    fn finish(&mut self, turns: &mut Vec<TurnDetails>) {
        let TurnBuilder { asked, plan, calls, context, sources, more_sources, output, citations, .. } = std::mem::take(self);
        let steps = plan.unwrap_or(calls);
        if asked.is_some() || !steps.is_empty() || context.is_some() || !sources.is_empty() || !output.is_empty() || !citations.is_empty() {
            turns.push(TurnDetails { asked, steps, context, sources, more_sources, output, citations, unfound: 0 });
        }
    }

    /// What answer `text` cites.
    fn cite(&mut self, text: &str) {
        for words in crate::answer::citations(text) {
            if self.citations.len() < crate::answer::MAX_CITES && !self.citations.iter().any(|c| c.words == words) {
                self.citations.push(Citation { words, source: None });
            }
        }
    }

    /// What tool call `call` read. An agent's read that doesn't say where is listed by its title.
    /// Details is worked out each frame it shows: what isn't listed isn't copied.
    fn read(&mut self, call: &'a ToolCall) {
        for source in &call.sources {
            self.source(source.kind, &source.title, source.target.as_deref());
        }
        if call.sources.is_empty() && matches!(call.kind.as_str(), "read" | "search" | "fetch") {
            self.source(SourceKind::Other, &call.title, None);
        }
    }

    fn source(&mut self, kind: SourceKind, title: &'a str, target: Option<&'a str>) {
        if !self.seen.insert((kind, title, target)) {
            return;
        }
        if self.sources.len() < MAX_TURN_SOURCES {
            self.sources.push(Source { kind, title: title.to_string(), target: target.map(str::to_string) });
        } else {
            self.more_sources = self.more_sources.saturating_add(1);
        }
    }
}

/// `text` on one line, cut short.
fn short(text: &str) -> String {
    let line = label(text);
    if line.chars().count() > ASKED_CHARS { line.chars().take(ASKED_CHARS).chain(['…']).collect() } else { line }
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
    /// How many more changes the document has than the entries list ([`crate::output::MAX_ROWS`]).
    #[serde(skip)]
    more_output: usize,
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

    /// What the entries say about the conversation, turn by turn: what the pane's Details shows
    /// and `assist.details` answers. It keeps nothing of its own, so it can't disagree with the
    /// conversation, and what was trimmed from it is gone from here too.
    pub fn details(&self) -> Details {
        let mut out = Details { more_output: u32::try_from(self.more_output).unwrap_or(u32::MAX), ..Details::default() };
        let mut turn = TurnBuilder::default();
        for entry in &self.entries {
            match entry {
                Entry::Asked { text } => {
                    turn.finish(&mut out.turns);
                    turn.asked = Some(short(text));
                }
                Entry::Plan { entries } => {
                    let state = |s: PlanStatus| match s {
                        PlanStatus::Pending => StepState::Waiting,
                        PlanStatus::InProgress => StepState::Running,
                        PlanStatus::Completed => StepState::Done,
                    };
                    turn.plan = Some(entries.iter().map(|p| Step { title: p.content.clone(), state: state(p.status) }).collect());
                }
                Entry::Tool(call) => {
                    let state = match call.status {
                        ToolStatus::Pending => StepState::Waiting,
                        ToolStatus::InProgress => StepState::Running,
                        ToolStatus::Completed => StepState::Done,
                        ToolStatus::Failed => StepState::Failed,
                    };
                    turn.calls.push(Step { title: call.title.clone(), state });
                    turn.read(call);
                }
                Entry::Model { name, .. } => out.model = Some(name.clone()),
                Entry::Agent { name } => out.agent = Some(name.clone()),
                Entry::Usage(usage) => out.usage = Some(usage.clone()),
                Entry::Context(sent) => turn.context = Some(sent.clone()),
                Entry::Output(row) => turn.output.push(row.clone()),
                Entry::Answer { text } => turn.cite(text),
                Entry::Thought { .. } | Entry::Permission { .. } | Entry::Stopped { .. } | Entry::Error { .. } => {}
            }
        }
        turn.finish(&mut out.turns);
        out
    }

    /// What the turn that just started gave the model of the document.
    pub fn sent(&mut self, sent: Sent) {
        self.push(Entry::Context(sent));
    }

    /// The changes and comments the conversation has in the document now, from [`crate::output`]:
    /// a row seen before is brought up to date, a new one joins the current turn, and one the
    /// document no longer has ended, as `ended` says. `more`: the document has more changes than
    /// `seen`, so a row missing from it may only be past the limit.
    pub(crate) fn note_output(&mut self, seen: Vec<OutputRow>, more: usize, ended: impl Fn(&OutputRow) -> crate::output::OutputState) {
        self.more_output = more;
        let rows: Vec<(usize, OutputRow)> =
            self.entries.iter().enumerate().filter_map(|(i, e)| if let Entry::Output(row) = e { Some((i, row.clone())) } else { None }).collect();
        // Rows are brought up to date before anything moves them: taking one away moves those after
        // it, and what is added can trim the oldest entries.
        let actions = crate::output::reconcile(&rows, seen, more, ended);
        let mut dropped = Vec::new();
        let mut new = Vec::new();
        for action in actions {
            match action {
                Action::Write(i, row) => self.overwrite(i, Entry::Output(row)),
                Action::Drop(i) => dropped.push(i),
                Action::New(row) => new.push(row),
            }
        }
        dropped.sort_unstable();
        for i in dropped.into_iter().rev() {
            self.remove_at(i);
        }
        self.trim();
        for row in new {
            self.push(Entry::Output(row));
        }
        self.prune_output();
    }

    /// Take the entry at `i` away.
    fn remove_at(&mut self, i: usize) {
        if let Some(e) = self.entries.remove(i) {
            self.bytes = self.bytes.saturating_sub(e.bytes());
            if i < self.turn_start {
                self.turn_start = self.turn_start.saturating_sub(1);
            }
        }
    }

    /// Hold the list of what the conversation changed to [`crate::output::MAX_ROWS`]: the oldest
    /// rows that ended go, so they never push the conversation out of the transcript.
    fn prune_output(&mut self) {
        let is_row = |e: &Entry| matches!(e, Entry::Output(_));
        let mut extra = self.entries.iter().filter(|e| is_row(e)).count().saturating_sub(crate::output::MAX_ROWS);
        let mut i = 0;
        while extra > 0 && i < self.entries.len() {
            let ended = matches!(self.entries.get(i), Some(Entry::Output(row)) if row.state != crate::output::OutputState::Open);
            if ended {
                self.remove_at(i);
                extra = extra.saturating_sub(1);
            } else {
                i = i.saturating_add(1);
            }
        }
    }

    /// The person asked `text`: a new turn starts.
    pub fn asked(&mut self, text: &str) {
        self.turn_start = self.entries.len();
        self.push(Entry::Asked { text: text.to_string() });
    }

    /// Add what `event` says.
    pub fn record(&mut self, event: &Event) {
        match event {
            Event::Text { text } => self.append(text, |e| matches!(e, Entry::Answer { .. }), |t| Entry::Answer { text: t }),
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
            Event::Model { name } => {
                if !self.in_turn().any(|(_, e)| matches!(e, Entry::Model { name: n } if n == name)) {
                    self.push(Entry::Model { name: name.clone() });
                }
            }
            Event::Agent { name } => {
                if !self.in_turn().any(|(_, e)| matches!(e, Entry::Agent { name: n } if n == name)) {
                    self.push(Entry::Agent { name: name.clone() });
                }
            }
            Event::Usage(usage) => {
                let i = self.in_turn().find(|(_, e)| matches!(e, Entry::Usage(_))).map(|(i, _)| i);
                match i {
                    Some(i) => self.replace(i, Entry::Usage(usage.clone())),
                    None => self.push(Entry::Usage(usage.clone())),
                }
            }
            Event::Done { stop } => {
                if *stop != StopReason::EndTurn {
                    self.push(Entry::Stopped { stop: *stop });
                }
            }
            Event::Error { message } => self.push(Entry::Error { message: message.clone() }),
        }
    }

    /// The current turn's entries, newest first, with their indexes.
    fn in_turn(&self) -> impl Iterator<Item = (usize, &Entry)> {
        self.entries.iter().enumerate().skip(self.turn_start).rev()
    }

    /// Add `text` to the current turn's newest entry the conversation shows if `is` it, else start
    /// one with `make`.
    fn append(&mut self, text: &str, is: impl Fn(&Entry) -> bool, make: impl Fn(String) -> Entry) {
        let newest = self.in_turn().find(|(_, e)| !e.details_only()).map(|(i, _)| i);
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
        self.overwrite(i, e);
        self.trim();
    }

    /// Put `e` at `i`, which moves nothing.
    fn overwrite(&mut self, i: usize, e: Entry) {
        if let Some(slot) = self.entries.get_mut(i) {
            self.bytes = self.bytes.saturating_sub(slot.bytes()).saturating_add(e.bytes());
            *slot = e;
        }
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

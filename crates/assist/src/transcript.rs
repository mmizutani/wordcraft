//! A conversation as the pane shows it: what the person asked, what the assistant answered, its
//! tool calls, plans and permission requests, and how each turn ended. Held to a size: the
//! oldest entries go first.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{DefaultHasher, Hasher};

use serde::Serialize;

use crate::backend::{Answer, Event, PermissionRequest, PlanEntry, PlanStatus, Source, SourceKind, StopReason, ToolCall, ToolStatus, Usage, label};
use crate::cases::marker::{Card, CaseMarker, MAX_PER_TURN};
use crate::cases::recall::RecallSent;
use crate::cases::record::NoteId;
use crate::context::Sent;
use crate::markers::{Markers, Piece};
use crate::output::{Action, OutputRow};

/// Most entries a transcript keeps.
pub const MAX_ENTRIES: usize = 2048;

/// Most bytes of text a transcript keeps, roughly.
pub const MAX_BYTES: usize = 4 << 20;

/// Most bytes of text one answer or thought keeps: what streams in after it is left out, and
/// the entry ends with "…".
pub const MAX_ENTRY_BYTES: usize = 256 << 10;

/// Most proposals one turn shows; more are dropped.
pub const MAX_PROPOSALS: u8 = 3;

/// The shared run, in bytes, that makes a change a quote of recalled notes: about six English words
/// or twelve Japanese characters. Shorter runs are phrases any contract has.
const QUOTE_BYTES: usize = 36;

/// Most windows of recalled text [`SentWindows`] keeps: 64 turns of [`crate::cases::recall::BUDGET`]
/// of ASCII, which has a window at every byte. Full, it holds about 12 MiB: 16 bytes a count in
/// the map, 8 a hash in its batch.
pub(crate) const MAX_WINDOWS: usize = 64 * crate::cases::recall::BUDGET;

/// Why a turn recalled nothing once the conversation's set of windows is full.
pub(crate) const RECALL_WITHHELD: &str = "this conversation has recalled as much as can be checked; start a new one";

/// The windows of `text` ([`crate::cases::text::windows`]), folded, as the set keeps and looks them up.
fn windows_of(text: &str) -> Vec<u64> {
    let folded = crate::cases::text::folded(text);
    crate::cases::text::windows(&folded, QUOTE_BYTES).map(hash).collect()
}

/// How many windows a recall that sent `texts` puts in the set: what [`Transcript::recall_room`]
/// is measured against before the text goes to a turn.
pub(crate) fn recall_windows<'a>(texts: impl Iterator<Item = &'a str>) -> usize {
    texts.map(|t| windows_of(t).len()).sum()
}

/// Every window ([`crate::cases::text::windows`]) of the folded text sent as recall in the
/// conversation, hashed, apart from the entries, so trimming the conversation doesn't forget what
/// the model was given. A row is checked with one lookup per window of its own text, whatever was
/// sent. Nothing leaves it: a retaining agent session still holds what it was sent, so a recall
/// that wouldn't fit is withheld before it reaches a turn ([`crate::cases::recall::RecallAsk`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SentWindows {
    /// How many kept windows hash to each value: a window two recalls sent stays while either does.
    counts: HashMap<u64, u32>,
    /// Each recall's window hashes, oldest first.
    batches: VecDeque<Vec<u64>>,
    len: usize,
    #[cfg(test)]
    lookups: std::cell::Cell<usize>,
}

impl SentWindows {
    /// A recall sent `texts`.
    fn add<'a>(&mut self, texts: impl Iterator<Item = &'a str>) {
        let batch: Vec<u64> = texts.flat_map(windows_of).collect();
        // A recall too short for one window keeps no batch, and one past the cap keeps nothing
        // either: the turn was withheld by the same count, so this only fails closed.
        if batch.is_empty() || self.len.saturating_add(batch.len()) > MAX_WINDOWS {
            return;
        }
        for h in &batch {
            let n = self.counts.entry(*h).or_default();
            *n = n.saturating_add(1);
        }
        self.len = self.len.saturating_add(batch.len());
        self.batches.push_back(batch);
    }

    /// Whether a window of `text` was sent.
    fn has(&self, text: &str) -> bool {
        let folded = crate::cases::text::folded(text);
        crate::cases::text::windows(&folded, QUOTE_BYTES).any(|w| {
            #[cfg(test)]
            self.lookups.set(self.lookups.get() + 1);
            self.counts.contains_key(&hash(w))
        })
    }
}

/// Whether a row's full text stopped at [`wordcraft_engine::cmd::review::ROW_FULL_BYTES`]: the
/// character that didn't fit is at most four bytes.
fn cut_at_cap(full: &str) -> bool {
    full.len().saturating_add(4) > wordcraft_engine::cmd::review::ROW_FULL_BYTES
}

fn hash(window: &str) -> u64 {
    let mut h = DefaultHasher::new();
    h.write(window.as_bytes());
    h.finish()
}

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
    /// The turn's answer says preference `id`, which the turn sent, shaped it. Its place in the
    /// turn's list is its chip's number, and the answer's text holds the chip ([`crate::answer`]).
    /// Details shows it; the conversation doesn't.
    Used { id: String },
    /// Which model answers and at what level ([`Event::Model`]), as it last said.
    Model { name: String, effort: Option<String>, asked: bool },
    /// What the agent says it is. Details shows it; the conversation doesn't.
    Agent { name: String },
    /// What the turn used, as it last said. Details shows it; the conversation doesn't.
    Usage(Usage),
    /// What the turn gave the model of the document. Details shows it; the conversation doesn't.
    Context(Sent),
    /// What the turn recalled from the person's cases, as data ([`crate::cases::recall`]), or why
    /// nothing. Details shows it; the conversation doesn't.
    Recalled(RecallSent),
    /// A model's marker wrote this into the conversation's case, unconfirmed
    /// ([`crate::cases::marker`]): a quiet row under the answer. A note's row has Confirm, Edit and
    /// Remove, which are the person's commands.
    CaseWritten { what: Written, note: Option<NoteId>, text: String },
    /// A model proposes facts, or a write the case didn't take without a click: a card. It carries
    /// no authority: only the person's click on the card stores anything.
    CaseCard { id: u64, card: Card, answer: Option<ProposalAnswer> },
    /// How many case markers of the turn were dropped: too long, unclosed, over the turn's most, or
    /// not one this build reads. Details shows it; the conversation doesn't.
    Markers { dropped: u32 },
    /// A change or a comment the conversation made, as it last stood. Details shows it; the
    /// conversation doesn't.
    Output(OutputRow),
    /// Something the person should know about the turn ([`Event::Note`]).
    Note { text: String },
    /// The turn stopped other than by finishing its answer.
    Stopped { stop: StopReason },
    /// The turn failed.
    Error { message: String },
}

/// What a model's marker wrote into the case.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Written {
    Note,
    Plan,
    Balance,
}

impl Entry {
    /// Whether only Details shows it: the conversation goes on as if it weren't there, and an
    /// answer that arrives in pieces around it stays one answer.
    pub fn details_only(&self) -> bool {
        matches!(
            self,
            Entry::Agent { .. }
                | Entry::Usage(_)
                | Entry::Context(_)
                | Entry::Recalled(_)
                | Entry::Output(_)
                | Entry::Used { .. }
                | Entry::Markers { .. }
        )
    }

    fn bytes(&self) -> usize {
        match self {
            Entry::Asked { text }
            | Entry::Answer { text }
            | Entry::Thought { text }
            | Entry::Note { text }
            | Entry::Proposal { text, .. }
            | Entry::Agent { name: text }
            | Entry::Used { id: text }
            | Entry::Error { message: text } => text.len(),
            Entry::Model { name, effort, .. } => name.len() + effort.as_ref().map_or(0, String::len),
            Entry::Tool(t) => {
                let sources = t.sources.iter().map(|s| s.title.len() + s.target.as_ref().map_or(0, String::len));
                t.title.len() + t.detail.len() + t.id.len() + sources.sum::<usize>()
            }
            Entry::Plan { entries } => entries.iter().map(|e| e.content.len()).sum(),
            Entry::Permission { request, .. } => request.title.len() + request.options.iter().map(|o| o.name.len() + o.id.len()).sum::<usize>(),
            Entry::Usage(u) => u.cost.as_ref().map_or(0, String::len),
            Entry::Context(sent) => {
                let memory = sent.memory.iter().map(|p| p.id.len() + p.text.len()).sum::<usize>();
                sent.document.len() + sent.at.len() + memory + sent.knowledge.iter().map(|s| s.name.len()).sum::<usize>()
            }
            Entry::Output(row) => row.text.len().saturating_add(row.full.len()),
            Entry::Recalled(sent) => sent.bytes(),
            Entry::CaseWritten { text, .. } => text.len(),
            // A card is at most a few short texts; the marker that made it was held to 8 KiB.
            Entry::CaseCard { .. } => crate::markers::MAX_CASE,
            Entry::Stopped { .. } | Entry::Markers { .. } => 0,
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
    /// What it recalled from the person's cases, once the conversation's thread looked.
    pub recalled: Option<RecallSent>,
    /// What its tool calls read and looked for, in order, each once, at most [`MAX_TURN_SOURCES`].
    pub sources: Vec<Source>,
    /// How many more [`TurnDetails::sources`] leaves out.
    pub more_sources: u32,
    /// The changes and comments that first showed during it, oldest first.
    pub output: Vec<OutputRow>,
    /// What its answers cite, each once, at most [`crate::answer::MAX_CITES`].
    pub citations: Vec<Citation>,
    /// The preferences its answer marked, in chip order: a chip's number is its place here, from 1.
    /// The others [`TurnDetails::context`] says it sent apply to the whole answer.
    pub used: Vec<String>,
    /// How many citations' words the document and the sources don't have
    /// ([`crate::Assistant::details`] can't tell: the window looks, and leaves those out).
    pub unfound: u32,
    /// How many case markers the turn's answer carried that were dropped ([`Entry::Markers`]).
    pub markers_dropped: u32,
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
    /// The reasoning level it answered at, when one is known.
    pub effort: Option<String>,
    /// [`Self::effort`] is what was asked for, as the backend didn't say what ran.
    pub effort_asked: bool,
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
    recalled: Option<RecallSent>,
    sources: Vec<Source>,
    more_sources: u32,
    output: Vec<OutputRow>,
    citations: Vec<Citation>,
    used: Vec<String>,
    markers_dropped: u32,
    /// The sources seen, listed or not, so each counts once.
    seen: HashSet<(SourceKind, &'a str, Option<&'a str>)>,
}

impl<'a> TurnBuilder<'a> {
    /// Add the turn to `turns`, if it has anything to show, and start another.
    fn finish(&mut self, turns: &mut Vec<TurnDetails>) {
        let TurnBuilder { asked, plan, calls, context, recalled, sources, more_sources, output, citations, used, markers_dropped, .. } =
            std::mem::take(self);
        let steps = plan.unwrap_or(calls);
        if asked.is_some() || !steps.is_empty() || context.is_some() || !sources.is_empty() || !output.is_empty() || !citations.is_empty() {
            turns.push(TurnDetails { asked, steps, context, recalled, sources, more_sources, output, citations, used, unfound: 0, markers_dropped });
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
    /// The start of a marker waiting for the rest of it.
    #[serde(skip)]
    markers: Markers,
    /// The last proposal's number.
    #[serde(skip)]
    last_proposal: u64,
    /// How many proposals the current turn has shown.
    #[serde(skip)]
    proposed: u8,
    /// The preferences marked that the window hasn't counted yet ([`Transcript::take_used`]).
    #[serde(skip)]
    uncounted: Vec<String>,
    /// The case markers parsed that the window hasn't routed yet ([`Transcript::take_case_markers`]).
    #[serde(skip)]
    unrouted: Vec<CaseMarker>,
    /// How many case markers the current turn has kept.
    #[serde(skip)]
    case_marked: u8,
    /// The last card's number.
    #[serde(skip)]
    last_card: u64,
    /// How many more changes the document has than the entries list ([`crate::output::MAX_ROWS`]).
    #[serde(skip)]
    more_output: usize,
    /// What the conversation recalled, as windows, for [`Transcript::quotes_recalled`].
    #[serde(skip)]
    sent_windows: SentWindows,
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
                Entry::Model { name, effort, asked } => {
                    out.model = Some(name.clone());
                    out.effort = effort.clone();
                    out.effort_asked = *asked;
                }
                Entry::Agent { name } => out.agent = Some(name.clone()),
                Entry::Usage(usage) => out.usage = Some(usage.clone()),
                Entry::Context(sent) => turn.context = Some(sent.clone()),
                Entry::Recalled(sent) => turn.recalled = Some(sent.clone()),
                Entry::Output(row) => turn.output.push(row.shown()),
                Entry::Answer { text } => turn.cite(text),
                Entry::Used { id } => turn.used.push(id.clone()),
                Entry::Markers { dropped } => turn.markers_dropped = *dropped,
                Entry::Thought { .. }
                | Entry::Permission { .. }
                | Entry::Proposal { .. }
                | Entry::CaseWritten { .. }
                | Entry::CaseCard { .. }
                | Entry::Note { .. }
                | Entry::Stopped { .. }
                | Entry::Error { .. } => {}
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
                Action::Write(i, mut row) => {
                    // Seen rows come without the flag: a row whose words are the same keeps it (only
                    // its state changed), and one whose words changed is read again. New words cut
                    // at the cap may hold the quote past the cut, so they can't clear the flag.
                    row.quotes = match self.entries.get(i) {
                        Some(Entry::Output(k)) if k.full == row.full => k.quotes,
                        Some(Entry::Output(k)) => (k.quotes && cut_at_cap(&row.full)) || self.quotes_recalled(&row.full),
                        _ => self.quotes_recalled(&row.full),
                    };
                    self.overwrite(i, Entry::Output(row));
                }
                Action::Drop(i) => dropped.push(i),
                Action::New(mut row) => {
                    row.quotes = self.quotes_recalled(&row.full);
                    new.push(row);
                }
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

    /// Whether `text` shares [`QUOTE_BYTES`] with what any turn of the conversation recalled, trimmed
    /// from the entries or not.
    fn quotes_recalled(&self, text: &str) -> bool {
        self.sent_windows.has(text)
    }

    /// How many windows of recalled text the conversation can still take before its recalls are
    /// withheld.
    pub(crate) fn recall_room(&self) -> usize {
        MAX_WINDOWS.saturating_sub(self.sent_windows.len)
    }

    /// How many recalls' windows are kept.
    #[cfg(test)]
    pub(crate) fn batches_for_test(&self) -> usize {
        self.sent_windows.batches.len()
    }

    #[cfg(test)]
    pub(crate) fn quotes_recalled_for_test(&self, text: &str) -> bool {
        self.quotes_recalled(text)
    }

    /// How many window lookups the quote checks have made.
    #[cfg(test)]
    pub(crate) fn lookups_for_test(&self) -> usize {
        self.sent_windows.lookups.get()
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
        self.flush_markers();
        self.turn_start = self.entries.len();
        self.proposed = 0;
        self.case_marked = 0;
        self.push(Entry::Asked { text: text.to_string() });
    }

    /// Add what `event` says.
    pub fn record(&mut self, event: &Event) {
        // What only Details shows doesn't end an answer, so a marker goes on around it too.
        if !matches!(event, Event::Text { .. } | Event::Agent { .. } | Event::Usage(_) | Event::Options(_) | Event::Recalled(_)) {
            self.flush_markers();
        }
        match event {
            Event::Text { text } => {
                for piece in self.markers.feed(text) {
                    self.piece(piece);
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
            Event::Note { text } => self.push(Entry::Note { text: text.clone() }),
            // What is offered isn't part of the conversation: [`crate::Assistant::offered`].
            Event::Options(_) => {}
            Event::Recalled(sent) => {
                let mut sent = sent.clone();
                let texts = std::mem::take(&mut sent.sent_text);
                self.sent_windows.add(texts.iter().map(String::as_str));
                self.push(Entry::Recalled(sent));
            }
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

    /// The preferences marked since the last call, each once a turn: what `used` counts.
    pub fn take_used(&mut self) -> Vec<String> {
        std::mem::take(&mut self.uncounted)
    }

    /// The case markers parsed since the last call, at most [`MAX_PER_TURN`] a turn, for the
    /// window to route to the conversation's case.
    pub fn take_case_markers(&mut self) -> Vec<CaseMarker> {
        std::mem::take(&mut self.unrouted)
    }

    /// A marker's write landed in the case: a quiet row under the answer.
    pub fn case_written(&mut self, what: Written, note: Option<NoteId>, text: &str) {
        self.push(Entry::CaseWritten { what, note, text: text.to_string() });
    }

    /// A marker became a card for the person: its number, through the conversation.
    pub fn case_card(&mut self, card: Card) -> u64 {
        self.last_card = self.last_card.saturating_add(1);
        self.push(Entry::CaseCard { id: self.last_card, card, answer: None });
        self.last_card
    }

    /// Card `id`, while the person hasn't answered it.
    pub fn pending_case_card(&self, id: u64) -> Option<&Card> {
        self.entries.iter().rev().find_map(|e| match e {
            Entry::CaseCard { id: i, card, answer: None } if *i == id => Some(card),
            _ => None,
        })
    }

    /// The person answered card `id`.
    pub fn answer_case_card(&mut self, id: u64, answer: ProposalAnswer) {
        let found = self.entries.iter().enumerate().rev().find_map(|(i, e)| match e {
            Entry::CaseCard { id: n, card, .. } if *n == id => Some((i, card.clone())),
            _ => None,
        });
        if let Some((i, card)) = found {
            self.replace(i, Entry::CaseCard { id, card, answer: Some(answer) });
        }
    }

    /// One piece of the answer, by what it is.
    fn piece(&mut self, piece: Piece) {
        match piece {
            Piece::Text(t) => self.answer(&t),
            Piece::Remember(t) => self.propose(&t),
            Piece::Used(id) => self.mark_used(&id),
            Piece::Case(body) => match CaseMarker::parse(&body) {
                Some(marker) if self.case_marked < MAX_PER_TURN => {
                    self.case_marked = self.case_marked.saturating_add(1);
                    self.unrouted.push(marker);
                }
                _ => self.dropped_marker(),
            },
            Piece::CaseDropped => self.dropped_marker(),
        }
    }

    /// One more case marker of the turn was dropped: Details says how many.
    fn dropped_marker(&mut self) {
        let found = self.in_turn().find_map(|(i, e)| match e {
            Entry::Markers { dropped } => Some((i, *dropped)),
            _ => None,
        });
        match found {
            Some((i, dropped)) => self.replace(i, Entry::Markers { dropped: dropped.saturating_add(1) }),
            None => self.push(Entry::Markers { dropped: 1 }),
        }
    }

    /// The preferences marked in the turn a tracked change dated `date` was made in: the last one
    /// started at or before then. Dates are as `wordcraft_engine::cmd::now_iso` writes them, which
    /// sort as they read.
    pub fn marked_at(&self, date: &str) -> Vec<crate::memory::SentPreference> {
        let mut turn = None;
        let mut marked = Vec::new();
        for entry in &self.entries {
            match entry {
                Entry::Context(sent) if sent.at.as_str() > date => break,
                Entry::Context(sent) => {
                    turn = Some(sent);
                    marked.clear();
                }
                Entry::Used { id } => marked.extend(turn.and_then(|sent| sent.memory.iter().find(|p| p.id == *id)).cloned()),
                _ => {}
            }
        }
        marked
    }

    /// What preference or recalled item `id` said in the turn of the entry at `entry`, if that turn
    /// sent it: the person may have reworded a preference since.
    pub fn sent_preference(&self, entry: usize, id: &str) -> Option<&str> {
        let turn = self.entries.iter().take(entry.saturating_add(1)).rev().take_while(|e| !matches!(e, Entry::Asked { .. }));
        turn.into_iter().find_map(|e| match e {
            Entry::Context(sent) => sent.memory.iter().find(|p| p.id == id).map(|p| p.text.as_str()),
            Entry::Recalled(sent) => sent.items.iter().find(|i| i.id.to_string() == id).map(|i| i.text.as_str()),
            _ => None,
        })
    }

    /// The model says preference or recalled item `id` shaped the answer here: a chip goes in,
    /// numbered through the turn. One the turn didn't send does nothing, whatever the model writes.
    fn mark_used(&mut self, id: &str) {
        let sent = self.in_turn().any(|(_, e)| match e {
            Entry::Context(sent) => sent.memory.iter().any(|p| p.id == id),
            Entry::Recalled(sent) => sent.items.iter().any(|i| i.id.to_string() == id),
            _ => false,
        });
        if !sent {
            return;
        }
        let marked: Vec<&str> =
            self.entries.iter().skip(self.turn_start).filter_map(|e| if let Entry::Used { id } = e { Some(id.as_str()) } else { None }).collect();
        let number = match marked.iter().position(|m| *m == id) {
            Some(place) => place.saturating_add(1),
            None => {
                let number = marked.len().saturating_add(1);
                self.uncounted.push(id.to_string());
                self.push(Entry::Used { id: id.to_string() });
                number
            }
        };
        self.answer(&crate::answer::used_marker(number, id));
    }

    /// A piece of the answer. Only white space right after a proposal starts no entry of its own.
    fn answer(&mut self, text: &str) {
        if text.trim().is_empty() && matches!(self.in_turn().map(|(_, e)| e).find(|e| !e.details_only()), Some(Entry::Proposal { .. })) {
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

    /// What was held back as the start of a marker was only text, or an unclosed case marker,
    /// which is dropped.
    fn flush_markers(&mut self) {
        for piece in self.markers.flush() {
            self.piece(piece);
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

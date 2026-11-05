//! What runs an assistant's turns ([`Backend`]: ChatGPT, an ACP agent, the scripted [`crate::Fake`])
//! and what a turn sends back while it runs ([`Event`]).
//!
//! A backend runs one turn at a time on its conversation's thread ([`crate::Assistant`]): it
//! streams what the model says as [`Event`]s, calls WordCraft's tools through the turn's
//! [`TurnCtx`] when it runs the model loop in WordCraft, and returns why the turn stopped. The
//! window never waits for it: it reads the events each frame.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use wordcraft_engine::room::Role;

/// Most bytes of text one event carries; more is cut at a character boundary.
pub const MAX_EVENT_TEXT: usize = 64 << 10;

/// Most options a permission request offers; more are dropped.
pub const MAX_OPTIONS: usize = 16;

/// Most characters in a title, a name or an option shown in the pane.
pub const MAX_LABEL: usize = 500;

/// Most entries in a plan; more are dropped.
pub const MAX_PLAN: usize = 64;

/// What a backend is: the pane's backend picker shows it, and the assistant joins the shared
/// document under its author name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendInfo {
    /// Its id, as `assist.backend {id}` names it: `chatgpt`, `acp:claude-code`, `fake`…
    pub id: String,
    /// Its name in the picker.
    pub label: String,
    /// The name it joins under, which its tracked changes and comments carry: "ChatGPT",
    /// "Claude Code"…
    pub author: String,
    /// Whether it calls WordCraft's tools in this process ([`ToolHost`]): a backend that runs
    /// the model loop in WordCraft (ChatGPT, the fake) does, on a copy of the document the
    /// conversation holds for it. An ACP agent doesn't: it reaches a copy of its own through
    /// WordCraft's MCP server, with the seat's key ([`Seat`]).
    pub in_process_tools: bool,
}

/// The assistant's place in the shared document, which the backend gets when the conversation
/// starts ([`Backend::begin`]): who it is, and the key of the invite it joined with, which an
/// agent outside the process connects with (WordCraft's MCP server, `WORDCRAFT_CONTROL_KEY`).
#[derive(Clone)]
pub struct Seat {
    /// The name it joined under: the author of its comments and tracked changes.
    pub name: String,
    /// Its role in the shared document.
    pub role: Role,
    /// The invite's key: whoever holds it joins as this participant. Never logged or shown.
    pub key: String,
    /// The title of the shared document, for the note on the first turn.
    pub document: String,
}

impl Seat {
    /// Whether its edits are tracked changes in its name (a suggester's).
    pub fn tracked(&self) -> bool {
        self.role == Role::Suggester
    }
}

impl std::fmt::Debug for Seat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The key stays out of logs.
        f.debug_struct("Seat").field("name", &self.name).field("role", &self.role).field("document", &self.document).finish_non_exhaustive()
    }
}

/// One turn's input.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Turn {
    /// What the person asked.
    pub prompt: String,
    /// The document around the selection, as text ([`crate::Context`]), when the turn has it.
    pub context: Option<String>,
    /// The note the conversation's first turn carries: which document, the assistant's role,
    /// whether its edits are tracked, and how to read and follow the document ([`crate::note`]).
    pub note: Option<String>,
}

/// Why a turn stopped (ACP's stop reasons, which the ChatGPT backend maps its own to).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model finished its answer.
    EndTurn,
    /// The model reached its most tokens for an answer.
    MaxTokens,
    /// The turn reached its most model requests (tool rounds).
    MaxTurnRequests,
    /// The model declined to go on.
    Refusal,
    /// The person stopped it.
    Cancelled,
}

impl StopReason {
    /// What the pane says, plainly, when a turn stopped other than by finishing.
    pub fn says(self) -> Option<&'static str> {
        match self {
            StopReason::EndTurn => None,
            StopReason::MaxTokens => Some("The answer was cut off: it reached the most the model writes at once."),
            StopReason::MaxTurnRequests => Some("The assistant stopped after the most steps one request may take."),
            StopReason::Refusal => Some("The assistant declined to go on."),
            StopReason::Cancelled => Some("Stopped."),
        }
    }
}

/// How far a tool call got.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

/// A tool call, as the pane shows it: one row per `id`, which later events with the same id
/// update.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    /// The call's id in its turn.
    pub id: String,
    /// What it does: the tool's name and command, or the agent's title for it.
    pub title: String,
    /// What kind of call it is (ACP's kinds: read, edit, delete, move, search, execute, think,
    /// fetch, other).
    pub kind: String,
    pub status: ToolStatus,
    /// A short account of what it did or why it failed.
    pub detail: String,
    /// The version of the shared document once the call's change was in it, for a call that
    /// changed the document in this process.
    pub version: Option<u64>,
}

/// How a plan entry stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}

/// How much a plan entry matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    High,
    Medium,
    Low,
}

/// One entry of an agent's plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanEntry {
    pub content: String,
    pub priority: Priority,
    pub status: PlanStatus,
}

/// What an option of a permission request does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

impl PermissionKind {
    /// Whether the option lets the agent go ahead.
    pub fn allows(self) -> bool {
        matches!(self, PermissionKind::AllowOnce | PermissionKind::AllowAlways)
    }
}

/// One answer an agent offers to its permission request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    /// Its id, which the answer names.
    pub id: String,
    /// What the pane's button says.
    pub name: String,
    pub kind: PermissionKind,
}

/// An agent asks to do something (ACP's `session/request_permission`); its turn waits for the
/// answer ([`TurnCtx::ask`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRequest {
    /// The request's number in its conversation, which the answer names
    /// ([`crate::Assistant::answer`]); [`TurnCtx::ask`] gives it.
    pub id: u64,
    /// What it asks to do: the tool call's title.
    pub title: String,
    /// The answers it offers, at most [`MAX_OPTIONS`].
    pub options: Vec<PermissionOption>,
}

impl PermissionRequest {
    /// A request for `title` with `options`, held to the pane's limits: at most [`MAX_OPTIONS`]
    /// options, labels of at most [`MAX_LABEL`] characters, options without an id dropped.
    pub fn bounded(title: &str, options: impl IntoIterator<Item = PermissionOption>) -> PermissionRequest {
        let options = options
            .into_iter()
            .filter(|o| !o.id.is_empty() && o.id.len() <= MAX_LABEL)
            .take(MAX_OPTIONS)
            .map(|o| PermissionOption { name: label(&o.name), ..o })
            .collect();
        PermissionRequest { id: 0, title: label(title), options }
    }
}

/// The answer to a permission request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
pub enum Answer {
    /// The option with this id was chosen.
    Selected { option: String },
    /// No answer: the turn was stopped, or the conversation ended.
    Cancelled,
}

/// How a conversation answers its agent's permission requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionPolicy {
    /// The person answers in the pane ([`crate::Assistant::answer`]).
    Ask,
    /// Rejected at once, and the transcript says so: a programmatic run (control channel, MCP,
    /// CLI) never opens a prompt, and rejects unless its setting allows.
    #[default]
    Reject,
    /// Allowed once at once, and the transcript says so.
    Allow,
}

/// What a turn sends back while it runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "event")]
pub enum Event {
    /// A piece of the answer, to add to what came before.
    Text { text: String },
    /// A piece of what the agent thinks, shown dimmed.
    Thought { text: String },
    /// A tool call started or changed.
    Tool(ToolCall),
    /// The agent's plan, whole: it replaces the one before.
    Plan { entries: Vec<PlanEntry> },
    /// The agent asks to do something; its turn waits for the answer.
    Permission(PermissionRequest),
    /// A permission request was answered, by the person or by the conversation's policy.
    Answered { id: u64, answer: Answer },
    /// Which model answered.
    Model { name: String },
    /// The turn ended.
    Done { stop: StopReason },
    /// The turn failed: why, said plainly. The turn ended.
    Error { message: String },
}

impl Event {
    /// Whether the event ends its turn.
    pub fn ends_turn(&self) -> bool {
        matches!(self, Event::Done { .. } | Event::Error { .. })
    }

    /// The event held to the pane's limits: text cut at [`MAX_EVENT_TEXT`] bytes, labels at
    /// [`MAX_LABEL`] characters, plans at [`MAX_PLAN`] entries, and no character that reorders
    /// or hides the text around it ([`shown`], [`label`]).
    pub fn bounded(self) -> Event {
        let text = |t: String| cut(shown(t), MAX_EVENT_TEXT);
        match self {
            Event::Text { text: t } => Event::Text { text: text(t) },
            Event::Thought { text: t } => Event::Thought { text: text(t) },
            Event::Tool(t) => Event::Tool(ToolCall { id: label(&t.id), title: label(&t.title), kind: label(&t.kind), detail: text(t.detail), ..t }),
            Event::Plan { entries } => {
                Event::Plan { entries: entries.into_iter().take(MAX_PLAN).map(|e| PlanEntry { content: label(&e.content), ..e }).collect() }
            }
            Event::Permission(r) => Event::Permission(PermissionRequest { id: r.id, ..PermissionRequest::bounded(&r.title, r.options) }),
            Event::Model { name } => Event::Model { name: label(&name) },
            Event::Error { message } => Event::Error { message: text(message) },
            e @ (Event::Answered { .. } | Event::Done { .. }) => e,
        }
    }
}

/// `text` cut to at most `max` bytes, at a character boundary.
pub fn cut(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// `text` as a label (a title, a name, a permission option): at most [`MAX_LABEL`] characters,
/// without control characters or invisible ones ([`invisible_in_label`]), so a backend can't
/// make a label read as something it isn't: "\u{202E}wollA" shows as "wollA", not "Allow".
pub fn label(text: &str) -> String {
    text.chars().filter(|c| !c.is_control() && !invisible_in_label(*c)).take(MAX_LABEL).collect()
}

/// `text` as the pane shows it in an answer, a thought or an error: without the characters that
/// reorder or hide the text around them ([`reorders_or_hides`]). Line breaks and the marks
/// right-to-left text uses stay.
pub fn shown(text: String) -> String {
    if text.chars().any(reorders_or_hides) { text.chars().filter(|c| !reorders_or_hides(*c)).collect() } else { text }
}

/// The bidi embeddings, overrides and isolates (U+202A–U+202E, U+2066–U+2069), which show text in
/// an order other than its own, and the tag characters (U+E0000–U+E007F), which a model reads but
/// the person doesn't see.
fn reorders_or_hides(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{E0000}'..='\u{E007F}')
}

/// What a label leaves out besides control characters: [`reorders_or_hides`], the bidi marks
/// (U+061C, U+200E, U+200F), the zero-width space (U+200B), the word joiner and invisible
/// operators (U+2060–U+2064), the byte order mark (U+FEFF) and the interlinear annotation signs
/// (U+FFF9–U+FFFB). The zero-width joiner and non-joiner stay: scripts and emoji need them.
fn invisible_in_label(c: char) -> bool {
    reorders_or_hides(c)
        || matches!(c, '\u{061C}' | '\u{200B}' | '\u{200E}' | '\u{200F}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}')
}

/// A function tool a backend offers its model: the MCP server's tools, for one shared document
/// ([`crate::tools`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// The JSON schema of its arguments.
    pub parameters: Value,
}

/// What a tool call gave back, for the model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutput {
    /// Whether it worked.
    pub ok: bool,
    /// Its result as text (JSON for most tools), or why it failed; at most
    /// [`crate::tools::MAX_OUTPUT`] bytes.
    pub text: String,
    /// The version of the shared document once the call's change was in it, when it changed
    /// the document.
    pub version: Option<u64>,
}

impl ToolOutput {
    /// A failed call, with why.
    pub fn error(why: impl Into<String>) -> ToolOutput {
        ToolOutput { ok: false, text: why.into(), version: None }
    }
}

/// Where a turn's tool calls go: the copy of the shared document the conversation holds for a
/// backend that runs the model loop in WordCraft ([`crate::tools::CopyTools`]).
pub trait ToolHost {
    /// The tools the model may call.
    fn tools(&self) -> &[ToolSpec];
    /// Call tool `name` with `args`. Junk arguments and unknown tools are error outputs, never
    /// panics.
    fn call(&mut self, name: &str, args: &Value) -> ToolOutput;
}

/// No tools: for a backend whose agent reaches the document on its own (ACP).
pub struct NoTools;

impl ToolHost for NoTools {
    fn tools(&self) -> &[ToolSpec] {
        &[]
    }
    fn call(&mut self, name: &str, _: &Value) -> ToolOutput {
        ToolOutput::error(format!("`{name}`: this assistant reaches the document through its own connection"))
    }
}

/// Whether a turn was stopped, which the conversation sets ([`crate::Assistant::cancel`]) and the
/// backend checks between steps, or waits on. Clones are the same flag.
#[derive(Clone, Default)]
pub struct Cancel(Arc<(Mutex<bool>, Condvar)>);

impl Cancel {
    fn lock(&self) -> MutexGuard<'_, bool> {
        self.0.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Stop the turn: whoever waits wakes.
    pub fn set(&self) {
        *self.lock() = true;
        self.0.1.notify_all();
    }

    /// A new turn starts.
    pub fn clear(&self) {
        *self.lock() = false;
    }

    /// Whether the turn was stopped.
    pub fn is_set(&self) -> bool {
        *self.lock()
    }

    /// Wait until the turn is stopped or `timeout` has passed: whether it was stopped.
    pub fn wait(&self, timeout: Duration) -> bool {
        let guard = self.lock();
        let (guard, _) = self.0.1.wait_timeout_while(guard, timeout, |set| !*set).unwrap_or_else(PoisonError::into_inner);
        *guard
    }
}

/// Called when an event is sent: the window asks to be drawn again.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Where a conversation's events go: to the conversation ([`crate::Assistant::poll`]), which
/// the window reads each frame. Clones send to the same place.
#[derive(Clone)]
pub struct Events {
    tx: Sender<Event>,
    wake: Option<Waker>,
}

impl Events {
    pub(crate) fn new(tx: Sender<Event>, wake: Option<Waker>) -> Events {
        Events { tx, wake }
    }

    /// Send `event`, held to the pane's limits ([`Event::bounded`]), and wake the window.
    pub fn send(&self, event: Event) {
        // Nobody reads any more once the conversation is gone: the event goes nowhere.
        let _ = self.tx.send(event.bounded());
        if let Some(wake) = &self.wake {
            wake();
        }
    }
}

/// The permission request a turn waits on, and its answer, shared by the conversation's thread
/// and the conversation.
#[derive(Clone, Default)]
pub(crate) struct Permissions(Arc<(Mutex<Asking>, Condvar)>);

#[derive(Default)]
pub(crate) struct Asking {
    /// The request waiting for an answer, by its number.
    waiting: Option<u64>,
    /// Its answer, once given.
    answer: Option<Answer>,
    /// The last request's number.
    last: u64,
}

impl Permissions {
    fn lock(&self) -> MutexGuard<'_, Asking> {
        self.0.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A number for a new request, which waits for its answer from now on.
    fn open(&self) -> u64 {
        let mut a = self.lock();
        a.last = a.last.saturating_add(1);
        a.waiting = Some(a.last);
        a.answer = None;
        a.last
    }

    /// Answer request `id`, if it's the one waiting: whether it was.
    pub(crate) fn answer(&self, id: u64, answer: Answer) -> bool {
        let mut a = self.lock();
        if a.waiting != Some(id) || a.answer.is_some() {
            return false;
        }
        a.answer = Some(answer);
        self.0.1.notify_all();
        true
    }

    /// Wake the turn waiting for an answer: the request is cancelled.
    pub(crate) fn cancel(&self) {
        let mut a = self.lock();
        if a.waiting.is_some() && a.answer.is_none() {
            a.answer = Some(Answer::Cancelled);
        }
        self.0.1.notify_all();
    }

    /// Wait for request `id`'s answer: [`Answer::Cancelled`] once `cancel` is set or `timeout`
    /// has passed.
    fn wait(&self, id: u64, cancel: &Cancel, timeout: Duration) -> Answer {
        let until = Instant::now() + timeout;
        let mut a = self.lock();
        loop {
            if a.waiting == Some(id)
                && let Some(answer) = a.answer.take()
            {
                a.waiting = None;
                return answer;
            }
            let left = until.saturating_duration_since(Instant::now());
            if cancel.is_set() || left.is_zero() || a.waiting != Some(id) {
                a.waiting = None;
                return Answer::Cancelled;
            }
            // Short waits: a Stop sets `cancel` without this condition's lock.
            let (next, _) = self.0.1.wait_timeout(a, left.min(Duration::from_millis(50))).unwrap_or_else(PoisonError::into_inner);
            a = next;
        }
    }

    /// The request waiting for an answer, if one is.
    pub(crate) fn waiting(&self) -> Option<u64> {
        let a = self.lock();
        a.waiting.filter(|_| a.answer.is_none())
    }
}

/// Longest a turn waits for the person to answer a permission request.
pub const PERMISSION_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// What a turn works with: where its events go, its tools, whether it was stopped, and how its
/// permission requests are answered.
pub struct TurnCtx<'a> {
    pub(crate) tools: &'a mut dyn ToolHost,
    pub(crate) events: &'a Events,
    pub(crate) cancel: &'a Cancel,
    pub(crate) permissions: &'a Permissions,
    pub(crate) policy: PermissionPolicy,
    /// Tool calls made in this turn, for ids of calls a backend didn't name.
    pub(crate) calls: u64,
}

impl TurnCtx<'_> {
    /// Send `event` to the conversation.
    pub fn emit(&self, event: Event) {
        self.events.send(event);
    }

    /// Send a piece of the answer.
    pub fn say(&self, text: impl Into<String>) {
        self.emit(Event::Text { text: text.into() });
    }

    /// Whether the turn was stopped: the backend ends it as soon as it can, with
    /// [`StopReason::Cancelled`].
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_set()
    }

    /// The turn's stop flag, to wait on or hand to a reader thread.
    pub fn cancel(&self) -> &Cancel {
        self.cancel
    }

    /// The tools the model may call.
    pub fn tools(&self) -> &[ToolSpec] {
        self.tools.tools()
    }

    /// Call tool `name` with `args` for the model, and show the call in the pane: a row with
    /// the call's `id` (one is made up when it's empty), in progress, then done or failed.
    /// Refused once the turn was stopped.
    pub fn call_tool(&mut self, id: &str, name: &str, args: &Value) -> ToolOutput {
        self.calls = self.calls.saturating_add(1);
        let id = if id.is_empty() { format!("call-{}", self.calls) } else { id.to_string() };
        let title = crate::tools::title(name, args);
        let kind = crate::tools::kind(name).to_string();
        if self.is_cancelled() {
            return ToolOutput::error("stopped: the person stopped this turn");
        }
        self.emit(Event::Tool(ToolCall {
            id: id.clone(),
            title: title.clone(),
            kind: kind.clone(),
            status: ToolStatus::InProgress,
            detail: String::new(),
            version: None,
        }));
        let out = self.tools.call(name, args);
        let status = if out.ok { ToolStatus::Completed } else { ToolStatus::Failed };
        let detail = crate::tools::summary(&out);
        self.emit(Event::Tool(ToolCall { id, title, kind, status, detail, version: out.version }));
        out
    }

    /// Ask the person `request` and wait for the answer, per the conversation's policy: the
    /// pane shows it ([`PermissionPolicy::Ask`]), or it is answered at once. The answer is
    /// [`Answer::Cancelled`] when the turn is stopped or nobody answers in
    /// [`PERMISSION_TIMEOUT`].
    pub fn ask(&self, request: PermissionRequest) -> Answer {
        let id = self.permissions.open();
        let request = PermissionRequest { id, ..PermissionRequest::bounded(&request.title, request.options) };
        let pick = |allow: bool| {
            let kinds: &[PermissionKind] = if allow {
                &[PermissionKind::AllowOnce, PermissionKind::AllowAlways]
            } else {
                &[PermissionKind::RejectOnce, PermissionKind::RejectAlways]
            };
            kinds
                .iter()
                .find_map(|k| request.options.iter().find(|o| o.kind == *k))
                .map_or(Answer::Cancelled, |o| Answer::Selected { option: o.id.clone() })
        };
        let answer = match self.policy {
            PermissionPolicy::Ask => {
                self.emit(Event::Permission(request.clone()));
                self.permissions.wait(id, self.cancel, PERMISSION_TIMEOUT)
            }
            PermissionPolicy::Reject => {
                let a = pick(false);
                self.permissions.answer(id, a.clone());
                self.permissions.wait(id, self.cancel, Duration::ZERO);
                a
            }
            PermissionPolicy::Allow => {
                let a = pick(true);
                self.permissions.answer(id, a.clone());
                self.permissions.wait(id, self.cancel, Duration::ZERO);
                a
            }
        };
        if self.policy != PermissionPolicy::Ask {
            log::info!("assistant: permission request {:?} answered {answer:?} by the {:?} policy", request.title, self.policy);
            self.emit(Event::Permission(request));
        }
        self.emit(Event::Answered { id, answer: answer.clone() });
        answer
    }
}

/// What runs an assistant's turns.
pub trait Backend: Send {
    /// What it is.
    fn info(&self) -> BackendInfo;

    /// The conversation starts, with the assistant's seat in the shared document: get ready (an
    /// agent process starts and opens its session). Called once, on the conversation's thread,
    /// before the first turn; an error ends the conversation's turns with it.
    fn begin(&mut self, seat: &Seat) -> Result<(), String> {
        let _ = seat;
        Ok(())
    }

    /// Run one turn to its end, on the conversation's thread: stream what the model says into
    /// `ctx`, call tools through it, and stop as soon as it can once `ctx` is cancelled
    /// ([`StopReason::Cancelled`]). An error is said to the person as it is.
    fn turn(&mut self, turn: &Turn, ctx: &mut TurnCtx<'_>) -> Result<StopReason, String>;

    /// The conversation ended: stop what it started (kill the agent process, close
    /// connections). Called once, on the conversation's thread.
    fn end(&mut self) {}
}

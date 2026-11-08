//! The ChatGPT backend: the Responses API with the signed-in account's plan, streamed, with
//! WordCraft's tools as function tools that run in this process, on the conversation's copy of
//! the document ([`crate::TurnCtx::call_tool`]).
//!
//! Each request carries the whole conversation (`store: false`): the earlier turns as the
//! person's words and the assistant's answers, then this turn's items as the API gave them
//! (reasoning, messages, function calls) and the tools' outputs. Older outputs of this turn are
//! cut short, the latest kept whole. A turn makes at most [`MAX_ROUNDS`] requests.
//!
//! Each request runs on a thread of its own, which takes the token (refreshing it, and once
//! more when the API turns it away), sends, and reads the stream into a channel; the turn's
//! thread reads the channel, and checks Stop every [`POLL`], so a Stop ends the turn at once
//! even while the network hangs. The request thread stops once its attempt is over, however it
//! ended (a later turn's start doesn't undo a Stop): a request whose token came after a Stop
//! isn't sent, and a stream is let go of within one read. A stream has an hour in all, and five
//! minutes between events (comments don't count). Errors become plain sentences without
//! tokens, and are retried as the API's error codes say (a busy plan with backoff; a usage
//! limit never).

use std::cell::Cell;
use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, channel, sync_channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::auth::{AuthError, access_token};
use super::http::{self, Reply, redact};
use super::models::{self, Resolved, valid_model};
use super::sse;
use super::{AUTHOR, Endpoints, ID, USAGE_PAGE};
use crate::backend::{Backend, BackendInfo, Cancel, Event, ModelOption, Seat, StopReason, ToolSpec, Turn, TurnCtx, cut};

/// Most requests (model rounds) one turn makes.
pub const MAX_ROUNDS: usize = 25;

/// How often a waiting turn looks whether it was stopped.
pub const POLL: Duration = Duration::from_millis(100);

/// The namespace WordCraft's tools are offered in.
const NAMESPACE: &str = "wordcraft";

/// Most function calls of one answer that run; the rest are answered "not run".
const MAX_CALLS: usize = 32;

/// Most output items of one answer.
const MAX_ITEMS: usize = 256;

/// Most bytes of a call's arguments.
const MAX_ARGUMENTS: usize = 1 << 20;

/// Events of an answer that aren't JSON, past which it can't be read.
const MAX_JUNK: usize = 32;

/// An earlier round's tool output longer than this is cut to it.
const ELIDE_AFTER: usize = 2 << 10;

/// Most bytes of an earlier turn's answer the history keeps.
const MAX_EARLIER_ANSWER: usize = 16 << 10;

/// Most bytes of earlier turns the history keeps; the oldest go first.
const MAX_EARLIER: usize = 256 << 10;

/// Most bytes of one request: a turn that grows past it stops.
const MAX_REQUEST: usize = 16 << 20;

/// Most attempts at one request.
const MAX_ATTEMPTS: u32 = 4;

/// How the turn's requests are retried and how long their answers may stream (tests make them
/// quick).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tuning {
    pub(crate) max_rounds: usize,
    /// The first wait before a retry; each one after waits twice as long.
    pub(crate) backoff: Duration,
    /// Longest `Retry-After` waited for; a longer one ends the turn.
    pub(crate) max_wait: Duration,
    /// Longest an answer streams in all.
    pub(crate) max_stream: Duration,
    /// Longest an answer streams without an event (comments, keep-alives, don't count).
    pub(crate) max_quiet: Duration,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning {
            max_rounds: MAX_ROUNDS,
            backoff: Duration::from_secs(1),
            max_wait: Duration::from_secs(20),
            max_stream: Duration::from_secs(60 * 60),
            max_quiet: Duration::from_secs(5 * 60),
        }
    }
}

/// The ChatGPT backend, for one conversation.
pub struct ChatGpt {
    settings: PathBuf,
    endpoints: Endpoints,
    tuning: Tuning,
    /// The account's models, once they were read.
    listed: Option<Vec<ModelOption>>,
    /// What every request's `instructions` say: the note, and how to work.
    instructions: String,
    /// The earlier turns, as input items, and their size.
    earlier: VecDeque<Value>,
    earlier_bytes: usize,
    /// Request fields ChatGPT said it doesn't support here: left out until the model or the
    /// reasoning level changes (a level one model refuses, another may take).
    unsupported: Vec<String>,
    /// The model and level the last turn resolved to, which `unsupported` is about.
    ran: Option<(String, Option<String>)>,
}

impl ChatGpt {
    /// ChatGPT with the account signed in in the settings folder `settings`, at OpenAI.
    pub fn new(settings: &Path) -> ChatGpt {
        ChatGpt::with_endpoints(settings, Endpoints::default())
    }

    /// ChatGPT at `endpoints` (fake servers, in tests).
    pub fn with_endpoints(settings: &Path, endpoints: Endpoints) -> ChatGpt {
        ChatGpt {
            settings: settings.to_path_buf(),
            endpoints,
            tuning: Tuning::default(),
            listed: None,
            instructions: instructions("You are ChatGPT, a writing assistant inside WordCraft, a word processor."),
            earlier: VecDeque::new(),
            earlier_bytes: 0,
            unsupported: Vec::new(),
            ran: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn tuned(self, tuning: Tuning) -> ChatGpt {
        ChatGpt { tuning, ..self }
    }

    /// What this turn runs ([`models::resolve`]), reading the account's list the first time, and
    /// again for a chosen model it doesn't have: it may be older than the pane's. None when the
    /// turn was stopped meanwhile. A list that can't be read doesn't stop a turn with a choice:
    /// the choice runs as a model that names no levels would, and the next turn reads the list
    /// again.
    fn resolved(&mut self, turn: &Turn, ctx: &TurnCtx<'_>) -> Result<Option<Resolved>, String> {
        let (model, effort) = (turn.model.as_ref(), turn.effort.as_ref());
        let stale = |list: &[ModelOption]| model.is_some_and(|m| !list.iter().any(|l| l.id == *m));
        if self.listed.as_deref().is_none_or(stale) {
            let (settings, endpoints) = (self.settings.clone(), self.endpoints.clone());
            let Some(read) = off_thread(ctx.cancel(), move || models::models_with(&settings, &endpoints))? else {
                return Ok(None);
            };
            match (read, &model) {
                // A list with nothing usable in it isn't kept: the next turn reads it again.
                (Ok(list), _) if list.is_empty() => {}
                (Ok(list), _) => self.listed = Some(list),
                (Err(e), Some(_)) if !matches!(e, AuthError::NotSignedIn | AuthError::SignInAgain | AuthError::Denied) => {
                    log::info!("ChatGPT: the models can't be listed ({e}); the turn runs what was chosen");
                }
                (Err(e), _) => return Err(auth_says(&e)),
            }
        }
        let unlisted: Vec<ModelOption> = model
            .filter(|m| valid_model(m))
            .map(|id| ModelOption { id: id.clone(), name: id.clone(), ..ModelOption::default() })
            .into_iter()
            .collect();
        let list = self.listed.as_deref().unwrap_or(&unlisted);
        let resolved = models::resolve(list, model.map(String::as_str), effort.map(String::as_str));
        resolved.map(Some).ok_or_else(|| "ChatGPT offers no model to this account.".to_string())
    }

    /// A request's body: the model and reasoning level, the instructions, the history and this
    /// turn's items, the tools.
    fn body(&self, model: &str, effort: Option<&str>, tools: &[ToolSpec], items: &[Value]) -> Value {
        let input: Vec<Value> = self.earlier.iter().chain(items).cloned().collect();
        let mut reasoning = json!({"summary": "auto"});
        if let Some(effort) = effort {
            reasoning["effort"] = json!(effort);
        }
        let mut body = json!({
            "model": model,
            "instructions": self.instructions,
            "input": input,
            "store": false,
            "stream": true,
            "include": ["reasoning.encrypted_content"],
            "reasoning": reasoning,
        });
        if !tools.is_empty() {
            body["tools"] = json!([namespace(tools)]);
        }
        for field in &self.unsupported {
            leave_out(&mut body, field);
        }
        body
    }

    /// Whether the effort asked for this model was refused.
    fn effort_refused(&self) -> bool {
        self.unsupported.iter().any(|f| f == EFFORT || f == REASONING)
    }

    /// The turn's rounds: a request, the tool calls its answer makes, and the next request
    /// with their outputs, until an answer makes none.
    fn rounds(&mut self, turn: &Turn, ctx: &mut TurnCtx<'_>, progress: &mut Progress) -> Result<StopReason, String> {
        let Some(resolved) = self.resolved(turn, ctx)? else {
            return Ok(StopReason::Cancelled);
        };
        let ran = (resolved.id.clone(), resolved.effort.clone());
        if self.ran.as_ref() != Some(&ran) {
            self.unsupported.clear();
            self.ran = Some(ran);
        }
        fallback_notes(&resolved, turn, ctx);
        let Resolved { id: model, name, effort, .. } = resolved;
        progress.running = Running { id: model.clone(), name, effort: effort.filter(|_| !self.effort_refused()) };
        for round in 1..=self.tuning.max_rounds.max(1) {
            let mut body = self.body(&model, progress.running.effort.as_deref(), ctx.tools(), &progress.items);
            let answer = match self.exchange(ctx, &mut body, progress)? {
                Outcome::Cancelled => return Ok(StopReason::Cancelled),
                Outcome::Incomplete(answer, stop) => {
                    progress.items.extend(answer.items);
                    return Ok(stop);
                }
                Outcome::Completed(answer) => answer,
            };
            // What was asked for, when the answer didn't say what ran.
            if progress.shown.is_none() {
                show_model(None, None, progress, ctx);
            }
            let Answer { items, calls, refused, .. } = answer;
            progress.items.extend(items);
            if calls.is_empty() {
                return Ok(if refused { StopReason::Refusal } else { StopReason::EndTurn });
            }
            if round >= self.tuning.max_rounds {
                break;
            }
            let outputs = run_calls(ctx, &calls, progress);
            elide(&mut progress.items);
            progress.items.extend(outputs);
            if ctx.is_cancelled() {
                return Ok(StopReason::Cancelled);
            }
        }
        Ok(StopReason::MaxTurnRequests)
    }

    /// Send `body` and read its answer, retrying what may be retried.
    fn exchange(&mut self, ctx: &mut TurnCtx<'_>, body: &mut Value, progress: &mut Progress) -> Result<Outcome, String> {
        let mut tries = 0u32;
        loop {
            let bytes = serde_json::to_vec(body).map_err(|e| format!("the request couldn't be made: {e}"))?;
            if bytes.len() > MAX_REQUEST {
                return Err("This turn grew too long for ChatGPT to take in: ask for less at once, or start a new conversation.".into());
            }
            let (failure, shown) = match self.attempt(ctx, bytes, progress) {
                Attempt::Completed(answer) => return Ok(Outcome::Completed(answer)),
                Attempt::Incomplete(answer, stop) => return Ok(Outcome::Incomplete(answer, stop)),
                Attempt::Cancelled => return Ok(Outcome::Cancelled),
                Attempt::Failed(failure, shown) => (failure, shown),
            };
            let decision = if tries.saturating_add(1) >= MAX_ATTEMPTS {
                Decision::Stop(failure.says())
            } else {
                decide(&failure, tries, !shown, &self.tuning, &self.unsupported)
            };
            let wait = match decision {
                Decision::Stop(message) => return Err(message),
                Decision::Retry(wait) => wait,
                Decision::Without(field) => {
                    log::info!("ChatGPT: the request goes on without `{field}`, which isn't supported here");
                    leave_out(body, &field);
                    if field == EFFORT || field == REASONING {
                        effort_not_applied(progress, ctx);
                    }
                    self.unsupported.push(field);
                    Duration::ZERO
                }
            };
            tries = tries.saturating_add(1);
            log::info!("ChatGPT: trying again in {} ms ({})", wait.as_millis(), failure.says());
            if ctx.cancel().wait(wait) {
                return Ok(Outcome::Cancelled);
            }
        }
    }

    /// One request: sent on a thread of its own, its stream read here as it comes. The thread
    /// stops once this returns, however it returns.
    fn attempt(&self, ctx: &mut TurnCtx<'_>, body: Vec<u8>, progress: &mut Progress) -> Attempt {
        let (tx, rx) = sync_channel(64);
        let job = Job { settings: self.settings.clone(), endpoints: self.endpoints.clone(), body, tuning: self.tuning.clone() };
        let over = Over(Arc::new(AtomicBool::new(false)));
        let stop = Stop { over: over.0.clone(), cancel: ctx.cancel().clone() };
        let spawned = std::thread::Builder::new().name("chatgpt request".into()).spawn(move || fetch(job, &tx, &stop));
        if let Err(e) = spawned {
            return Attempt::Failed(Failure::Network(format!("the request can't start here: {e}")), false);
        }
        let mut answer = Answer::default();
        let mut junk = 0usize;
        loop {
            let Some(msg) = next(&rx, ctx) else {
                return Attempt::Cancelled;
            };
            let failure = match msg {
                Msg::Data(data) => {
                    let step = match parse(&data) {
                        Parsed::Event(event) => on_event(&event, &mut answer, progress, ctx),
                        Parsed::Done => Step::Continue,
                        Parsed::Junk => {
                            junk = junk.saturating_add(1);
                            if junk > MAX_JUNK { Step::Failed(Failure::Broken("its events aren't JSON".into())) } else { Step::Continue }
                        }
                    };
                    match step {
                        Step::Continue => continue,
                        Step::Completed => return Attempt::Completed(answer),
                        Step::Incomplete(stop) => return Attempt::Incomplete(answer, stop),
                        Step::Failed(failure) => failure,
                    }
                }
                Msg::End => Failure::Broken("it ended before it was finished".into()),
                Msg::Broken(why) => Failure::Broken(why),
                Msg::Network(why) => Failure::Network(why),
                Msg::Refused(reply) => Failure::refused(&reply),
                Msg::Auth(e) => Failure::Auth(e),
            };
            return Attempt::Failed(failure, answer.shown);
        }
    }

    /// Keep the turn in the history, as the person's words and what the assistant said (and
    /// which tools it used), within [`MAX_EARLIER`].
    fn remember(&mut self, prompt: &str, progress: &Progress) {
        if progress.said.trim().is_empty() && progress.tools.is_empty() {
            return;
        }
        let mut answer = progress.said.trim().to_string();
        if !progress.tools.is_empty() {
            let mut used = progress.tools.clone();
            used.dedup();
            answer.push_str(&format!("\n\n[Tools used in this turn: {}]", used.join(", ")));
        }
        for item in [user_text(prompt), assistant_text(answer.trim())] {
            self.earlier_bytes = self.earlier_bytes.saturating_add(item.to_string().len());
            self.earlier.push_back(item);
        }
        while self.earlier_bytes > MAX_EARLIER && self.earlier.len() > 2 {
            for _ in 0..2 {
                if let Some(old) = self.earlier.pop_front() {
                    self.earlier_bytes = self.earlier_bytes.saturating_sub(old.to_string().len());
                }
            }
        }
    }
}

impl Backend for ChatGpt {
    fn info(&self) -> BackendInfo {
        BackendInfo { id: ID.into(), label: AUTHOR.into(), author: AUTHOR.into(), in_process_tools: true }
    }

    fn begin(&mut self, seat: &Seat) -> Result<(), String> {
        // The first turn brings the note too; this one stands in until it does.
        self.instructions = instructions(&crate::note(seat, true));
        Ok(())
    }

    fn turn(&mut self, turn: &Turn, ctx: &mut TurnCtx<'_>) -> Result<StopReason, String> {
        if let Some(note) = turn.note.as_deref().filter(|n| !n.trim().is_empty()) {
            self.instructions = instructions(note);
        }
        let mut progress = Progress { items: vec![user_message(turn)], ..Progress::default() };
        let result = self.rounds(turn, ctx, &mut progress);
        self.remember(&turn.prompt, &progress);
        result
    }
}

/// The instructions every request carries.
fn instructions(note: &str) -> String {
    format!(
        "{note}\n\nThe person's message may start with the part of the document they see (their selection and the text around \
         it): read more with the tools when you need it. Change the document with the tools, not by writing the new text in \
         your answer."
    )
}

/// The tools, as one namespace of function tools.
fn namespace(tools: &[ToolSpec]) -> Value {
    let functions: Vec<Value> = tools
        .iter()
        .map(|t| json!({"type": "function", "name": t.name, "description": t.description, "parameters": t.parameters, "strict": false}))
        .collect();
    json!({
        "type": "namespace",
        "name": NAMESPACE,
        "description": "WordCraft's tools for the document the person has open: read it, follow its changes, and change it through \
                        its commands.",
        "tools": functions,
    })
}

/// The person's message: the document around the selection first, when the turn has it.
fn user_message(turn: &Turn) -> Value {
    let mut parts = Vec::new();
    if let Some(context) = turn.context.as_deref().filter(|c| !c.trim().is_empty()) {
        parts.push(json!({"type": "input_text", "text": format!("The document as the person sees it now:\n\n{context}")}));
    }
    parts.push(json!({"type": "input_text", "text": turn.prompt}));
    json!({"type": "message", "role": "user", "content": parts})
}

fn user_text(text: &str) -> Value {
    json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]})
}

fn assistant_text(text: &str) -> Value {
    json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]})
}

/// What a turn has so far.
#[derive(Default)]
struct Progress {
    /// This turn's input items: the person's message, then each answer's items as received and
    /// the tools' outputs.
    items: Vec<Value>,
    /// What the assistant said in this turn, for the history (at most [`MAX_EARLIER_ANSWER`]).
    said: String,
    /// The tools it called, in order.
    tools: Vec<String>,
    /// What this turn asks for.
    running: Running,
    /// What the pane was last told runs.
    shown: Option<Shown>,
}

/// What a turn asks for, as its requests say it.
#[derive(Default)]
struct Running {
    id: String,
    name: String,
    /// `None` once ChatGPT refused it.
    effort: Option<String>,
}

/// What the pane was told ([`Event::Model`]).
#[derive(Clone, PartialEq, Eq)]
struct Shown {
    name: String,
    effort: Option<String>,
    asked: bool,
}

/// One answer, as its stream gave it.
#[derive(Default)]
struct Answer {
    /// Its output items, as received.
    items: Vec<Value>,
    /// Its function calls.
    calls: Vec<Call>,
    /// Whether the model declined.
    refused: bool,
    /// Whether any of it reached the pane (a retry would show it twice).
    shown: bool,
    /// Whether its text came in pieces (else each message is shown whole when it's done).
    streamed: bool,
}

/// A function call of an answer.
struct Call {
    call_id: String,
    name: String,
    arguments: String,
}

/// How a request ended.
enum Outcome {
    Completed(Answer),
    Incomplete(Answer, StopReason),
    Cancelled,
}

/// How one attempt at a request ended.
enum Attempt {
    Completed(Answer),
    Incomplete(Answer, StopReason),
    Cancelled,
    /// It failed: why, and whether any of its answer was shown.
    Failed(Failure, bool),
}

/// Why an attempt failed.
#[derive(Debug)]
enum Failure {
    /// The sign-in: no token.
    Auth(AuthError),
    /// The request didn't reach ChatGPT.
    Network(String),
    /// The answer broke off, or couldn't be read.
    Broken(String),
    /// ChatGPT said no: before the stream (an HTTP status) or in it (status 0).
    Api { status: u16, code: String, message: String, param: Option<String>, retry_after: Option<Duration> },
}

impl Failure {
    /// An error answer.
    fn refused(reply: &Reply) -> Failure {
        let (code, message, param) = api_error(&reply.json());
        let message = if message.is_empty() { String::from_utf8_lossy(&reply.body).chars().take(300).collect() } else { message };
        log::warn!("ChatGPT: the request was refused (HTTP {}, {code}, request {:?})", reply.status, reply.request_id);
        Failure::Api { status: reply.status, code, message: redact(&message, &[]), param, retry_after: reply.retry_after }
    }

    /// An error in the stream.
    fn in_stream(error: &Value) -> Failure {
        let (code, message, param) = api_error(error);
        Failure::Api { status: 0, code, message: redact(&message, &[]), param, retry_after: None }
    }

    /// What the person reads when it isn't retried.
    fn says(&self) -> String {
        match self {
            Failure::Auth(e) => auth_says(e),
            Failure::Network(why) => format!("ChatGPT can't be reached: {why}."),
            Failure::Broken(why) => format!("ChatGPT's answer was cut off ({why}). Try again."),
            Failure::Api { status, code, message, .. } => api_says(*status, code, message),
        }
    }
}

/// The `code`, `message` and `param` of an error answer: `{"error": {…}}`, an in-stream
/// error, or `{"detail": "…"}`.
fn api_error(json: &Value) -> (String, String, Option<String>) {
    let error = json.get("error").filter(|e| e.is_object()).unwrap_or(json);
    let text = |v: Option<&Value>| v.and_then(Value::as_str).map(|s| crate::backend::label(s).chars().take(300).collect::<String>());
    // An in-stream `error` event's own type is no code.
    let kind = text(error.get("type")).filter(|t| t != "error");
    let code = text(error.get("code")).or(kind).unwrap_or_default();
    let message = text(error.get("message")).or_else(|| text(json.get("detail"))).or_else(|| text(json.get("error"))).unwrap_or_default();
    (code, message, text(error.get("param")).filter(|p| !p.is_empty()))
}

/// What a refusal means for the person.
fn api_says(status: u16, code: &str, message: &str) -> String {
    let because = if message.is_empty() { String::new() } else { format!(" ({})", message.trim_end_matches('.')) };
    match code {
        "subscription_sharing_usage_limit_exceeded" => {
            format!("You've reached your ChatGPT plan's usage limit for now. See your usage and limits at {USAGE_PAGE}.")
        }
        "subscription_sharing_user_not_eligible" => "This ChatGPT account can't be used in WordCraft: it takes a ChatGPT Plus or Pro plan.".into(),
        "subscription_sharing_route_not_supported" => "ChatGPT doesn't take this kind of request from WordCraft with your plan.".into(),
        "subscription_sharing_usage_unavailable" | "subscription_sharing_user_unavailable" => {
            "ChatGPT can't answer with your plan right now: try again in a moment.".into()
        }
        "subscription_sharing_invalid_user" => {
            "ChatGPT didn't accept your sign-in: try again, or sign out and sign in again in Options › Account.".into()
        }
        "chatpass_v2_scope_not_authorized" | "chatpass_v2_invalid_authorization_context" => {
            "ChatGPT didn't let WordCraft use your plan: sign in again in Options › Account and allow it.".into()
        }
        _ => match status {
            401 => format!("ChatGPT didn't accept your sign-in{because}: try again, or sign out and sign in again in Options › Account."),
            429 => "ChatGPT has too many requests right now: try again in a moment.".into(),
            500..=599 => format!("ChatGPT had a problem (HTTP {status}): try again in a moment."),
            0 => format!("ChatGPT couldn't finish its answer{because}."),
            _ => format!("ChatGPT refused the request{because}."),
        },
    }
}

/// What a sign-in problem means for a turn, as a sentence.
fn auth_says(e: &AuthError) -> String {
    let text = e.to_string();
    let mut chars = text.chars();
    let first = chars.next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default();
    let rest: String = chars.collect();
    let end = if text.ends_with('.') { "" } else { "." };
    format!("{first}{rest}{end}")
}

/// What to do about a failed attempt.
enum Decision {
    Stop(String),
    Retry(Duration),
    /// Try again without this request field.
    Without(String),
}

/// Whether to retry `failure` (`tries` retries made so far; `fresh` when nothing of its
/// answer was shown).
fn decide(failure: &Failure, tries: u32, fresh: bool, tuning: &Tuning, dropped: &[String]) -> Decision {
    let backoff = tuning.backoff.saturating_mul(2u32.saturating_pow(tries));
    let stop = || Decision::Stop(failure.says());
    match failure {
        Failure::Auth(_) => stop(),
        Failure::Network(_) | Failure::Broken(_) => {
            if fresh && tries == 0 {
                Decision::Retry(backoff)
            } else {
                stop()
            }
        }
        Failure::Api { status, code, param, retry_after, .. } => {
            if let Some(field) = droppable(code, param.as_deref()).filter(|f| !dropped.iter().any(|d| d == f)) {
                return Decision::Without(field);
            }
            let waited = retry_after.unwrap_or(backoff);
            let retry = match code.as_str() {
                "subscription_sharing_usage_unavailable" | "subscription_sharing_user_unavailable" => tries < 2,
                "" | "server_error" | "rate_limit_exceeded" | "internal_error" => match status {
                    429 => tries == 0,
                    500..=599 => tries < 2,
                    0 => tries == 0,
                    _ => false,
                },
                _ => false,
            };
            if retry && fresh && waited <= tuning.max_wait { Decision::Retry(waited) } else { stop() }
        }
    }
}

/// The request field that says how hard the model reasons.
const EFFORT: &str = "reasoning.effort";

/// The request's whole reasoning object.
const REASONING: &str = "reasoning";

/// The request field to leave out when ChatGPT says `param` isn't supported: the reasoning level
/// or the reasoning summary, alone, so refusing one doesn't lose the other; or all of the
/// reasoning, or the encrypted reasoning.
fn droppable(code: &str, param: Option<&str>) -> Option<String> {
    let unsupported = matches!(code, "subscription_sharing_unsupported_capability" | "unsupported_parameter" | "unsupported_value");
    let param = param.filter(|_| unsupported)?;
    [EFFORT, "reasoning.summary", REASONING, "include"]
        .into_iter()
        .find(|f| param == *f || param.starts_with(&format!("{f}.")) || param.starts_with(&format!("{f}[")))
        .map(str::to_string)
}

/// Take `field` out of a request's body: a top-level field, or `parent.child`, the parent going
/// with its last child.
fn leave_out(body: &mut Value, field: &str) {
    let Some(fields) = body.as_object_mut() else { return };
    match field.split_once('.') {
        None => {
            fields.remove(field);
        }
        Some((parent, child)) => {
            let Some(inner) = fields.get_mut(parent).and_then(Value::as_object_mut) else { return };
            inner.remove(child);
            if inner.is_empty() {
                fields.remove(parent);
            }
        }
    }
}

/// What a request thread sends the turn.
enum Msg {
    /// No token.
    Auth(AuthError),
    /// The request didn't reach ChatGPT: why.
    Network(String),
    /// An error answer.
    Refused(Reply),
    /// An event's data.
    Data(String),
    /// The stream ended.
    End,
    /// The stream broke off: why.
    Broken(String),
}

/// What a request thread needs.
struct Job {
    settings: PathBuf,
    endpoints: Endpoints,
    body: Vec<u8>,
    tuning: Tuning,
}

/// Whether a request thread stops: its attempt is over, or the turn was stopped. A later turn
/// clearing the turn's [`Cancel`] doesn't undo the first.
struct Stop {
    over: Arc<AtomicBool>,
    cancel: Cancel,
}

impl Stop {
    fn is_set(&self) -> bool {
        self.over.load(Ordering::Relaxed) || self.cancel.is_set()
    }
}

/// An attempt that isn't over yet: it is, once this is dropped.
struct Over(Arc<AtomicBool>);

impl Drop for Over {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// The request thread: take the token, send, and read the answer into `tx`. A token the API
/// turns away is refreshed, and the request sent again, once. Nothing is sent once `stop` is
/// set, and the body is let go of once it was taken.
fn fetch(job: Job, tx: &SyncSender<Msg>, stop: &Stop) {
    let Job { settings, endpoints, body, tuning } = job;
    let mut body = Some(body);
    let mut refused: Option<String> = None;
    for _ in 0..2 {
        if stop.is_set() {
            return;
        }
        let token = match access_token(&settings, &endpoints, refused.as_deref()) {
            Ok(c) if !c.plan_usage() => Err(AuthError::Denied),
            Ok(c) => Ok(c.access_token),
            Err(e) => Err(e),
        };
        let token = match token {
            Ok(t) => t,
            Err(e) => {
                let _ = tx.send(Msg::Auth(e));
                return;
            }
        };
        // Stopped while the token was taken (a refresh, or a wait for another one): not sent.
        if stop.is_set() {
            return;
        }
        let agent = http::agent(endpoints.is_local(), true);
        let url = format!("{}/responses", endpoints.api.trim_end_matches('/'));
        let sent = agent
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .send(body.as_deref().unwrap_or_default());
        let response = match sent {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(Msg::Network(http::network(&e)));
                return;
            }
        };
        let status = response.status().as_u16();
        if status == 401 && refused.is_none() {
            log::info!("ChatGPT: the token was turned away; refreshing it once");
            refused = Some(token);
            continue;
        }
        // Taken: not kept for as long as the answer streams.
        drop(body.take());
        if !(200..300).contains(&status) {
            let mut reply = http::read(response, http::MAX_ERROR_BODY).unwrap_or(Reply { status, ..Reply::default() });
            // A server that repeats the token back doesn't get it shown.
            reply.body = String::from_utf8_lossy(&reply.body).replace(&token, "[redacted]").into_bytes();
            let _ = tx.send(Msg::Refused(reply));
            return;
        }
        stream(http::body_reader(response), tx, stop, &tuning);
        return;
    }
}

/// Read the stream's events into `tx` until it ends, breaks, `stop` is set, or nobody reads.
/// `stop` is looked at before each read (the stream is let go of within one), and so are the
/// limits: [`Tuning::max_stream`] in all, [`Tuning::max_quiet`] without an event.
fn stream(reader: impl Read, tx: &SyncSender<Msg>, stop: &Stop, tuning: &Tuning) {
    let mut events = sse::Events::new(reader);
    let started = Instant::now();
    let last_event = Cell::new(started);
    let mut watch = || {
        if stop.is_set() {
            return Err(String::new());
        }
        if started.elapsed() > tuning.max_stream {
            return Err(format!("it took longer than {}", span(tuning.max_stream)));
        }
        if last_event.get().elapsed() > tuning.max_quiet {
            return Err(format!("it sent nothing for {}", span(tuning.max_quiet)));
        }
        Ok(())
    };
    loop {
        let msg = match events.next_event(&mut watch) {
            Ok(Some(data)) => {
                last_event.set(Instant::now());
                Msg::Data(data)
            }
            Ok(None) => Msg::End,
            Err(_) if stop.is_set() => return,
            Err(why) => Msg::Broken(why),
        };
        let last = !matches!(msg, Msg::Data(_));
        if tx.send(msg).is_err() || last {
            return;
        }
    }
}

/// `d` as the person reads it: `300 ms`, `20 s`, `5 min`.
fn span(d: Duration) -> String {
    match d.as_secs() {
        0 => format!("{} ms", d.as_millis()),
        s if s < 120 => format!("{s} s"),
        s => format!("{} min", s / 60),
    }
}

/// The next message of the request thread: None once the turn was stopped.
fn next(rx: &Receiver<Msg>, ctx: &TurnCtx<'_>) -> Option<Msg> {
    loop {
        if ctx.is_cancelled() {
            return None;
        }
        match rx.recv_timeout(POLL) {
            Ok(msg) => return Some(msg),
            Err(RecvTimeoutError::Timeout) => {}
            // A request thread stops without a word when the turn was stopped, and may see the
            // Stop before this does.
            Err(RecvTimeoutError::Disconnected) if ctx.is_cancelled() => return None,
            Err(RecvTimeoutError::Disconnected) => return Some(Msg::Broken("the request stopped unexpectedly".into())),
        }
    }
}

/// Run `job` on a thread of its own and wait for it, looking at Stop every [`POLL`]: None once
/// the turn was stopped (the job finishes on its own, unread).
fn off_thread<T: Send + 'static>(cancel: &Cancel, job: impl FnOnce() -> T + Send + 'static) -> Result<Option<T>, String> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("chatgpt".into())
        .spawn(move || {
            let _ = tx.send(job());
        })
        .map_err(|e| format!("ChatGPT can't be asked from here: {e}"))?;
    loop {
        if cancel.is_set() {
            return Ok(None);
        }
        match rx.recv_timeout(POLL) {
            Ok(v) => return Ok(Some(v)),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err("ChatGPT stopped unexpectedly.".into()),
        }
    }
}

/// An event's data.
enum Parsed {
    Event(Value),
    /// The `[DONE]` some streams end with.
    Done,
    Junk,
}

fn parse(data: &str) -> Parsed {
    if data.trim() == "[DONE]" {
        return Parsed::Done;
    }
    match serde_json::from_str::<Value>(data) {
        Ok(v) if v.is_object() => Parsed::Event(v),
        _ => Parsed::Junk,
    }
}

/// What an event means for the attempt.
enum Step {
    Continue,
    Completed,
    Incomplete(StopReason),
    Failed(Failure),
}

/// Take in one event of the answer: text and thoughts go to the pane at once, items are kept.
fn on_event(event: &Value, answer: &mut Answer, progress: &mut Progress, ctx: &TurnCtx<'_>) -> Step {
    let text = |name: &str| event.get(name).and_then(Value::as_str).unwrap_or_default();
    match text("type") {
        "response.created" | "response.in_progress" => {
            let (model, effort) = (event.pointer("/response/model"), event.pointer("/response/reasoning/effort"));
            show_model(model.and_then(Value::as_str), effort.and_then(Value::as_str), progress, ctx);
        }
        "response.output_text.delta" => {
            answer.streamed = true;
            say(text("delta"), answer, progress, ctx);
        }
        "response.refusal.delta" => {
            answer.streamed = true;
            answer.refused = true;
            say(text("delta"), answer, progress, ctx);
        }
        "response.reasoning_summary_text.delta" if !text("delta").is_empty() => {
            answer.shown = true;
            ctx.emit(Event::Thought { text: text("delta").to_string() });
        }
        "response.reasoning_summary_part.added" if event.get("summary_index").and_then(Value::as_u64).is_some_and(|i| i > 0) => {
            ctx.emit(Event::Thought { text: "\n\n".into() });
        }
        "response.output_item.done" => return keep(event.get("item"), answer, progress, ctx),
        "response.completed" => return completed(event.get("response"), answer, progress, ctx),
        "response.incomplete" => {
            let reason = event.pointer("/response/incomplete_details/reason").and_then(Value::as_str).unwrap_or_default();
            return Step::Incomplete(match reason {
                "content_filter" => StopReason::Refusal,
                _ => StopReason::MaxTokens,
            });
        }
        "response.failed" => {
            return Step::Failed(Failure::in_stream(event.pointer("/response/error").unwrap_or(&Value::Null)));
        }
        "error" => return Step::Failed(Failure::in_stream(event)),
        _ => {}
    }
    Step::Continue
}

/// Tell the pane which model answers and at what level: `model` and `effort` are what the
/// answer says, and what the turn asked for stands in for what it doesn't.
fn show_model(model: Option<&str>, effort: Option<&str>, progress: &mut Progress, ctx: &TurnCtx<'_>) {
    let model = model.map(str::trim).filter(|m| !m.is_empty());
    let name = model.filter(|m| !same_model(m, &progress.running.id)).unwrap_or(&progress.running.name).to_string();
    let said = effort.map(str::trim).filter(|e| !e.is_empty());
    let line = Shown {
        name,
        effort: said.map(str::to_string).or_else(|| progress.running.effort.clone()),
        asked: said.is_none() && progress.running.effort.is_some(),
    };
    // A later event that doesn't report the level leaves what an earlier one reported.
    let settled = progress.shown.as_ref().is_some_and(|s| *s == line || (said.is_none() && !s.asked && s.name == line.name));
    if !settled && !line.name.is_empty() {
        ctx.emit(Event::Model { name: line.name.clone(), effort: line.effort.clone(), asked: line.asked });
        progress.shown = Some(line);
    }
}

/// Whether the answer's `answered` is the model `asked` for: the same, or a dated form of it
/// (`gpt-x-2026-01-01`).
fn same_model(answered: &str, asked: &str) -> bool {
    answered == asked
        || answered.strip_prefix(asked).and_then(|rest| rest.strip_prefix('-')).is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
}

/// Say what runs instead of what the person chose.
fn fallback_notes(resolved: &Resolved, turn: &Turn, ctx: &TurnCtx<'_>) {
    if let Some(chosen) = turn.model.as_ref().filter(|_| resolved.model_gone) {
        ctx.emit(Event::Note { text: format!("{chosen} isn't offered to this account now: {} runs instead.", resolved.name) });
    }
    if let Some(chosen) = turn.effort.as_ref().filter(|_| resolved.effort_gone) {
        let instead = resolved.effort.as_deref().map_or("no level asked for".to_string(), |e| format!("{e} instead"));
        ctx.emit(Event::Note { text: format!("{} doesn't offer {chosen} effort: it runs at {instead}.", resolved.name) });
    }
}

/// ChatGPT refused the level asked for: the turn goes on without one, and says so.
fn effort_not_applied(progress: &mut Progress, ctx: &TurnCtx<'_>) {
    if let Some(effort) = progress.running.effort.take() {
        ctx.emit(Event::Note { text: format!("ChatGPT didn't take {effort} effort for {}: it wasn't applied.", progress.running.name) });
        // A line the stream already showed says what was refused: it changes now, in case the
        // retry never gets as far.
        if progress.shown.take().is_some() {
            show_model(None, None, progress, ctx);
        }
    }
}

/// Show a piece of the answer.
fn say(delta: &str, answer: &mut Answer, progress: &mut Progress, ctx: &TurnCtx<'_>) {
    if delta.is_empty() {
        return;
    }
    answer.shown = true;
    if progress.said.len() < MAX_EARLIER_ANSWER {
        progress.said.push_str(delta);
        progress.said = cut(std::mem::take(&mut progress.said), MAX_EARLIER_ANSWER);
    }
    ctx.say(delta);
}

/// Keep an output item as received; a function call is also run once the answer is complete,
/// and a message whose text didn't come in pieces is shown now.
fn keep(item: Option<&Value>, answer: &mut Answer, progress: &mut Progress, ctx: &TurnCtx<'_>) -> Step {
    let Some(item) = item.filter(|i| i.is_object()) else {
        return Step::Continue;
    };
    if answer.items.len() >= MAX_ITEMS {
        return Step::Failed(Failure::Broken(format!("it has more than {MAX_ITEMS} parts")));
    }
    if !answer.streamed {
        say(&whole_text(item), answer, progress, ctx);
    }
    let field = |name: &str| item.get(name).and_then(Value::as_str).map(str::to_string);
    match field("type").as_deref() {
        Some("function_call") => {
            // A call without an id can't be answered: it stays out of the history.
            let Some(call_id) = field("call_id").filter(|id| !id.is_empty() && id.len() <= 256) else {
                log::warn!("ChatGPT: a function call without an id was left out");
                return Step::Continue;
            };
            let name = field("name").unwrap_or_default();
            // The arguments are JSON text; an object given as it is is taken too.
            let arguments = match item.get("arguments") {
                Some(Value::String(text)) => text.clone(),
                Some(args @ Value::Object(_)) => args.to_string(),
                _ => String::new(),
            };
            answer.calls.push(Call { call_id, name, arguments });
        }
        Some("message") => {
            let refusal = item
                .get("content")
                .and_then(Value::as_array)
                .is_some_and(|c| c.iter().any(|p| p.get("type").and_then(Value::as_str) == Some("refusal")));
            answer.refused |= refusal;
        }
        _ => {}
    }
    answer.items.push(item.clone());
    Step::Continue
}

/// A message item's text, whole: its text and refusal parts.
fn whole_text(item: &Value) -> String {
    if item.get("type").and_then(Value::as_str) != Some("message") {
        return String::new();
    }
    let parts = item.get("content").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    parts.iter().filter_map(|p| p.get("text").or_else(|| p.get("refusal")).and_then(Value::as_str)).collect()
}

/// The answer is complete: its items, from the stream (else from the response itself).
fn completed(response: Option<&Value>, answer: &mut Answer, progress: &mut Progress, ctx: &TurnCtx<'_>) -> Step {
    if answer.items.is_empty()
        && let Some(output) = response.and_then(|r| r.get("output")).and_then(Value::as_array)
    {
        for item in output {
            if let Step::Failed(f) = keep(Some(item), answer, progress, ctx) {
                return Step::Failed(f);
            }
        }
    }
    Step::Completed
}

/// Run an answer's function calls, in order: their outputs, for the next request. Once the turn
/// is stopped, or past [`MAX_CALLS`], a call isn't run, and its output says so.
fn run_calls(ctx: &mut TurnCtx<'_>, calls: &[Call], progress: &mut Progress) -> Vec<Value> {
    let mut outputs = Vec::with_capacity(calls.len());
    for (n, call) in calls.iter().enumerate() {
        let output = if ctx.is_cancelled() {
            "Not run: the person stopped the turn.".to_string()
        } else if n >= MAX_CALLS {
            format!("Not run: at most {MAX_CALLS} tool calls at once.")
        } else {
            run_call(ctx, call, progress)
        };
        outputs.push(json!({"type": "function_call_output", "call_id": call.call_id, "output": output}));
    }
    outputs
}

/// Run one call: its output for the model.
fn run_call(ctx: &mut TurnCtx<'_>, call: &Call, progress: &mut Progress) -> String {
    let name = call.name.strip_prefix(NAMESPACE).and_then(|rest| rest.strip_prefix('.').or_else(|| rest.strip_prefix("__"))).unwrap_or(&call.name);
    let args = if call.arguments.len() > MAX_ARGUMENTS {
        Err(format!("the arguments are longer than {} KB", MAX_ARGUMENTS >> 10))
    } else if call.arguments.trim().is_empty() {
        Ok(json!({}))
    } else {
        serde_json::from_str::<Value>(&call.arguments).map_err(|e| format!("the arguments aren't JSON: {e}"))
    };
    if progress.tools.len() < 64 {
        progress.tools.push(crate::backend::label(name).chars().take(64).collect());
    }
    match args {
        Ok(args) => {
            let out = ctx.call_tool(&call.call_id, name, &args);
            if out.ok { out.text } else { format!("Error: {}", out.text) }
        }
        Err(why) => format!("Error: {why}"),
    }
}

/// Cut the tool outputs already in the turn (earlier rounds') to [`ELIDE_AFTER`] bytes: the
/// next round's outputs, added after this, stay whole.
fn elide(items: &mut [Value]) {
    for item in items.iter_mut().filter(|i| i.get("type").and_then(Value::as_str) == Some("function_call_output")) {
        let Some(output) = item.get_mut("output") else {
            continue;
        };
        let Some(text) = output.as_str().filter(|t| t.len() > ELIDE_AFTER) else {
            continue;
        };
        let total = text.len();
        let kept = cut(text.to_string(), ELIDE_AFTER / 2);
        let left = total.saturating_sub(kept.len());
        *output = json!(format!("{kept}\n…[{left} more bytes of this earlier result left out: call the tool again if you need them]"));
    }
}

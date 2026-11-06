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
//! even while the network hangs. Errors become plain sentences without tokens, and are retried
//! as the API's error codes say (a busy plan with backoff; a usage limit never).

use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, channel, sync_channel};
use std::time::Duration;

use serde_json::{Value, json};

use super::auth::{self, AuthError, access_token};
use super::http::{self, Reply, redact};
use super::sse;
use super::{AUTHOR, Endpoints, ID, USAGE_PAGE};
use crate::backend::{Backend, BackendInfo, Cancel, Event, Seat, StopReason, ToolSpec, Turn, TurnCtx, cut};

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

/// How the turn's requests are retried (tests make them quick).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tuning {
    pub(crate) max_rounds: usize,
    /// The first wait before a retry; each one after waits twice as long.
    pub(crate) backoff: Duration,
    /// Longest `Retry-After` waited for; a longer one ends the turn.
    pub(crate) max_wait: Duration,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning { max_rounds: MAX_ROUNDS, backoff: Duration::from_secs(1), max_wait: Duration::from_secs(20) }
    }
}

/// The ChatGPT backend, for one conversation.
pub struct ChatGpt {
    settings: PathBuf,
    endpoints: Endpoints,
    tuning: Tuning,
    /// The model the list offered first, once it was read (used when none was chosen).
    listed: Option<String>,
    /// What every request's `instructions` say: the note, and how to work.
    instructions: String,
    /// The earlier turns, as input items, and their size.
    earlier: VecDeque<Value>,
    earlier_bytes: usize,
    /// Request fields ChatGPT said it doesn't support here: left out from then on.
    unsupported: Vec<String>,
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
        }
    }

    #[cfg(test)]
    pub(crate) fn tuned(self, tuning: Tuning) -> ChatGpt {
        ChatGpt { tuning, ..self }
    }

    /// The model for this turn: the one the person chose, else the first the account's list
    /// offers (read once). None when the turn was stopped meanwhile.
    fn model(&mut self, ctx: &TurnCtx<'_>) -> Result<Option<String>, String> {
        if let Some(chosen) = auth::chosen_model(&self.settings) {
            return Ok(Some(chosen));
        }
        if let Some(listed) = &self.listed {
            return Ok(Some(listed.clone()));
        }
        let (settings, endpoints) = (self.settings.clone(), self.endpoints.clone());
        let Some(models) = off_thread(ctx.cancel(), move || auth::models_with(&settings, &endpoints))? else {
            return Ok(None);
        };
        let first = models.map_err(|e| auth_says(&e))?.into_iter().next().ok_or("ChatGPT offers no model to this account.")?;
        self.listed = Some(first.slug.clone());
        Ok(Some(first.slug))
    }

    /// A request's body: the model, the instructions, the history and this turn's items, the
    /// tools.
    fn body(&self, model: &str, tools: &[ToolSpec], items: &[Value]) -> Value {
        let input: Vec<Value> = self.earlier.iter().chain(items).cloned().collect();
        let mut body = json!({
            "model": model,
            "instructions": self.instructions,
            "input": input,
            "store": false,
            "stream": true,
            "include": ["reasoning.encrypted_content"],
            "reasoning": {"summary": "auto"},
        });
        if !tools.is_empty() {
            body["tools"] = json!([namespace(tools)]);
        }
        if let Some(fields) = body.as_object_mut() {
            for field in &self.unsupported {
                fields.remove(field);
            }
        }
        body
    }

    /// The turn's rounds: a request, the tool calls its answer makes, and the next request
    /// with their outputs, until an answer makes none.
    fn rounds(&mut self, ctx: &mut TurnCtx<'_>, progress: &mut Progress) -> Result<StopReason, String> {
        let Some(model) = self.model(ctx)? else {
            return Ok(StopReason::Cancelled);
        };
        for round in 1..=self.tuning.max_rounds.max(1) {
            let mut body = self.body(&model, ctx.tools(), &progress.items);
            let answer = match self.exchange(ctx, &mut body, progress)? {
                Outcome::Cancelled => return Ok(StopReason::Cancelled),
                Outcome::Incomplete(answer, stop) => {
                    progress.items.extend(answer.items);
                    return Ok(stop);
                }
                Outcome::Completed(answer) => answer,
            };
            // The model asked for, when the answer didn't say which one answered.
            show_model(Some(&model), progress, ctx);
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
                    if let Some(fields) = body.as_object_mut() {
                        fields.remove(&field);
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

    /// One request: sent on a thread of its own, its stream read here as it comes.
    fn attempt(&self, ctx: &mut TurnCtx<'_>, body: Vec<u8>, progress: &mut Progress) -> Attempt {
        let (tx, rx) = sync_channel(64);
        let job = Job { settings: self.settings.clone(), endpoints: self.endpoints.clone(), body };
        let cancel = ctx.cancel().clone();
        let spawned = std::thread::Builder::new().name("chatgpt request".into()).spawn(move || fetch(&job, &tx, &cancel));
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
        let result = self.rounds(ctx, &mut progress);
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
    /// Whether the pane was told which model answers.
    model_shown: bool,
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

/// The request field to leave out when ChatGPT says `param` isn't supported: the reasoning
/// summary, or the encrypted reasoning.
fn droppable(code: &str, param: Option<&str>) -> Option<String> {
    let unsupported = matches!(code, "subscription_sharing_unsupported_capability" | "unsupported_parameter" | "unsupported_value");
    let param = param.filter(|_| unsupported)?;
    ["reasoning", "include"]
        .into_iter()
        .find(|f| param == *f || param.starts_with(&format!("{f}.")) || param.starts_with(&format!("{f}[")))
        .map(str::to_string)
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
}

/// The request thread: take the token, send, and read the answer into `tx`. A token the API
/// turns away is refreshed, and the request sent again, once.
fn fetch(job: &Job, tx: &SyncSender<Msg>, cancel: &Cancel) {
    let mut refused: Option<String> = None;
    for _ in 0..2 {
        if cancel.is_set() {
            return;
        }
        let token = match access_token(&job.settings, &job.endpoints, refused.as_deref()) {
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
        let agent = http::agent(job.endpoints.is_local(), true);
        let url = format!("{}/responses", job.endpoints.api.trim_end_matches('/'));
        let sent = agent
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .send(job.body.as_slice());
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
        if !(200..300).contains(&status) {
            let mut reply = http::read(response, http::MAX_ERROR_BODY).unwrap_or(Reply { status, ..Reply::default() });
            // A server that repeats the token back doesn't get it shown.
            reply.body = String::from_utf8_lossy(&reply.body).replace(&token, "[redacted]").into_bytes();
            let _ = tx.send(Msg::Refused(reply));
            return;
        }
        stream(http::body_reader(response), tx, cancel);
        return;
    }
}

/// Read the stream's events into `tx` until it ends, breaks, the turn stops, or nobody reads.
fn stream(reader: impl Read, tx: &SyncSender<Msg>, cancel: &Cancel) {
    let mut events = sse::Events::new(reader);
    loop {
        if cancel.is_set() {
            return;
        }
        let msg = match events.next_event() {
            Ok(Some(data)) => Msg::Data(data),
            Ok(None) => Msg::End,
            Err(why) => Msg::Broken(why),
        };
        let last = !matches!(msg, Msg::Data(_));
        if tx.send(msg).is_err() || last {
            return;
        }
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
            show_model(event.pointer("/response/model").and_then(Value::as_str), progress, ctx);
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

/// Tell the pane which model answers, once a turn.
fn show_model(model: Option<&str>, progress: &mut Progress, ctx: &TurnCtx<'_>) {
    if let Some(name) = model.filter(|m| !m.trim().is_empty() && !progress.model_shown) {
        progress.model_shown = true;
        ctx.emit(Event::Model { name: name.to_string() });
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

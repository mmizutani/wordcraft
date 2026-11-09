//! The window's assistant (#15, #17): the conversation the Assistant pane shows, the assistant
//! (backend) new conversations use, and the `assist` requests the engine's `assist.*` commands
//! hand the window.
//!
//! The window owns the conversation ([`Assistant`]). The first prompt shares the document if it
//! isn't shared yet (the assistant joins its room as a participant of its own) and starts a
//! conversation with the role chosen for it, else the person's setting (`assist.defaultRole`);
//! a prompt for another role starts a new conversation. Every frame [`tick`] takes what came
//! back and tells the session what the assistant can do ([`AssistantStatus`]), so its commands
//! are greyed out, with a tooltip saying why, when there is no assistant to ask.
//!
//! The writing actions (#18) are prompts like any other; [`action_items`] lists them for the
//! mini toolbar's Assistant menu and the right-click menu, and the pane has them as buttons.
//!
//! When an agent asks permission (#23), the pane asks the person in the turns they asked for
//! (the pane, a menu, a shortcut: [`WordApp::run`]). A turn a program asked for (the control
//! channel, MCP) has no one to ask: it answers per the person's setting
//! (`assist.allowUnattended`), refusing unless they allowed it, and the answer is logged. A tool
//! call's row links to the change it made in the shared document (`assist.showChange`).
//!
//! The web app (#26) has the pane and the commands but no assistant: its backends would sign in
//! through a loopback port or start agents, which take the desktop app ([`desktop`]). The pane
//! says so, and the buttons' tooltips too.
//!
//! Signing in to ChatGPT (#19) is [`chatgpt`]'s: File › Options › Account and the pane start it,
//! and [`tick`] takes how it ended, so ChatGPT is offered ready at once.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use wordcraft_assist::knowledge::{self, MAX_SOURCES};
use wordcraft_assist::{Answer, Assistant, Backend, Caps, Context, Host, Knowledge, Offer, Options, PermissionPolicy, Readiness};
use wordcraft_engine::cmd::assist::AssistantStatus;
use wordcraft_engine::room::Role;

use crate::WordApp;

pub(crate) mod chatgpt;
pub(crate) mod models;

/// Seconds between two askings of what is offered while the pane is open (a sign-in or an
/// install done meanwhile shows up).
const OFFERS_EVERY: f64 = 10.0;

/// Frames in which the prompt box takes the keyboard once asked to ([`AssistState::focus_prompt`]).
pub(crate) const FOCUS_FRAMES: u8 = 3;

/// The window's assistant.
#[derive(Default)]
pub(crate) struct AssistState {
    /// The conversation, once one started: kept when it ends, so the pane still shows it.
    pub(crate) conversation: Option<Conversation>,
    /// The assistants offered, as last asked ([`wordcraft_assist::offers::offers`]).
    pub(crate) offers: Vec<Offer>,
    /// When they were (`clock::now_ms`); `None` before the first time.
    offers_at: Option<f64>,
    /// The role chosen in the pane for the conversation (`None`: the setting).
    pub(crate) role: Option<Role>,
    /// The prompt being typed in the pane.
    pub(crate) prompt: String,
    /// The prompt box says what to draft at the caret (`assist.draft`) rather than asking.
    pub(crate) draft: bool,
    /// Frames left in which the prompt box takes the keyboard: a few, as the click that opened
    /// the pane hands the keyboard back to the page.
    pub(crate) focus_prompt: u8,
    /// The sources attached for the conversation: they carry on until a new one starts.
    pub(crate) knowledge: Vec<Knowledge>,
    /// The document they were attached in: no other document's turn gets them.
    knowledge_for: Option<u64>,
    /// Sources being read off the frame thread.
    adding: Vec<Receiver<Result<Knowledge, String>>>,
    /// Sources' Add shows what to add.
    pub(crate) add_open: bool,
    /// Where citations' words were found, for the document and revision and sources it was looked
    /// at with.
    cited: Option<CitedHeld>,
    /// Goes up each time the sources change: what they hold was looked at with it.
    knowledge_rev: u64,
    /// The document and its revision the conversation's record of what it changed last read.
    output_at: Option<(u64, u64)>,
    /// What the pane shows: the conversation, or what the assistant is doing and what runs it.
    pub(crate) view: PaneView,
    /// Why the last prompt didn't start, or a change couldn't be shown, for the pane.
    pub(crate) notice: Option<String>,
    /// The command running now is the person's ([`WordApp::run`]: the pane, a menu, a
    /// shortcut), not a program's: a turn it asks for asks them its agent's permission requests.
    pub(crate) by_person: bool,
    /// The permission request of the conversation the pane last opened for: it opens once for
    /// each, so the person sees what the turn waits for; closed while it waits, the request is
    /// answered cancelled. A new conversation numbers its requests from 1 again.
    opened_for: Option<u64>,
    /// How many changes the conversation's assistant has in the document, and for which
    /// (document, revision, name): Accept all and Reject all are greyed out without any.
    pub(crate) changes_by: Option<((u64, u64, String), usize)>,
    /// The backend the next conversation starts with, in place of the chosen one: tests and
    /// headless screenshots with a scripted assistant.
    next: Option<Box<dyn Backend>>,
    /// Offer what the web app offers, which is no assistant (#26): the web build always does;
    /// tests and headless screenshots of its pane on the desktop ([`WordApp::assist_as_web`]).
    pub(crate) web: bool,
    /// File › Options › Assistant's agents while the page shows them (#24): saved only through
    /// the agents' settings file, never logged.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) setup: Option<crate::options::agent_setup::AgentSetup>,
    /// The ChatGPT sign-in (#19): who is signed in, and a sign-in or sign-out under way.
    pub(crate) chatgpt: chatgpt::ChatGptState,
}

/// What the Assistant pane shows under its role.
/// The document's text and where citations' words were found, for what it was looked at with: the
/// document, its revision and the sources' [`AssistState::knowledge_rev`].
struct CitedHeld {
    at: (u64, u64, u64),
    text: String,
    found: HashMap<String, Cited>,
}

/// Where a citation's words are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Cited {
    Document,
    Source(String),
    Nowhere,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PaneView {
    #[default]
    Conversation,
    Details,
}

/// One conversation, and the document it is about.
pub(crate) struct Conversation {
    pub(crate) assistant: Assistant,
    /// The document ([`wordcraft_engine::Session::document_id`]).
    pub(crate) document: u64,
}

impl WordApp {
    /// Start the next conversation with `backend` instead of the assistant chosen in the pane:
    /// for tests and headless screenshots with a scripted assistant ([`wordcraft_assist::Fake`]).
    /// The conversation there is ends, so the next prompt starts one. A window that offers what
    /// the web app does ([`WordApp::assist_as_web`]) runs no assistant, this one included.
    pub fn assist_with(&mut self, backend: Box<dyn Backend>) {
        end(self);
        self.assist.next = Some(backend);
        self.session.assistant = status(self);
    }

    /// Offer what the web app offers, which is no assistant, as the web build does (#26): for
    /// tests and headless screenshots of the web's pane on the desktop. The conversation there is
    /// ends.
    pub fn assist_as_web(&mut self) {
        end(self);
        self.assist.next = None;
        self.assist.web = true;
        refresh_offers(self);
        self.session.assistant = status(self);
    }

    /// The conversation about the document the window shows, if there is one.
    pub(crate) fn conversation(&self) -> Option<&Assistant> {
        self.assist.conversation.as_ref().filter(|c| c.document == self.session.document_id()).map(|c| &c.assistant)
    }

    pub(crate) fn conversation_mut(&mut self) -> Option<&mut Assistant> {
        let document = self.session.document_id();
        self.assist.conversation.as_mut().filter(|c| c.document == document).map(|c| &mut c.assistant)
    }
}

/// What the window has, for the backends it offers. An agent's MCP server reaches the shared
/// document through the room's own port, which takes only invites' keys (the lobby's address,
/// set when the app opened it), never through the control channel's.
pub(crate) fn host(app: &WordApp) -> Host {
    Host { settings: app.services.settings_dir.clone(), room: app.lobby.addr(), cli: cli(), fake: fake_offered() && desktop(app) }
}

/// Whether the window offers the backends that sign in or start agent processes: the desktop
/// app's does; the web app's offers none (#26).
pub(crate) fn desktop(app: &WordApp) -> bool {
    wordcraft_assist::offers::DESKTOP && !app.assist.web
}

/// `wordcraft-cli` beside the app's executable, which an agent's MCP server runs; `None` when
/// it isn't there (the agents then say they're unavailable).
fn cli() -> Option<std::path::PathBuf> {
    if cfg!(target_arch = "wasm32") {
        return None;
    }
    let name = format!("wordcraft-cli{}", std::env::consts::EXE_SUFFIX);
    std::env::current_exe().ok().map(|exe| exe.with_file_name(name)).filter(|p| p.is_file())
}

/// The scripted assistant is offered in debug builds and tests, or with
/// `WORDCRAFT_ASSIST_FAKE=1`; never on the web, which runs no conversation thread.
fn fake_offered() -> bool {
    !cfg!(target_arch = "wasm32") && (cfg!(debug_assertions) || cfg!(test) || std::env::var("WORDCRAFT_ASSIST_FAKE").is_ok_and(|v| v.trim() == "1"))
}

/// Ask what is offered now.
pub(crate) fn refresh_offers(app: &mut WordApp) {
    app.assist.offers = wordcraft_assist::offers::offers_in(&host(app), desktop(app));
    app.assist.offers_at = Some(crate::clock::now_ms());
}

/// The assistant new conversations use: the one chosen in the pane while it is offered, else
/// the first one ready, else the first one offered.
pub(crate) fn chosen(app: &WordApp) -> Option<&Offer> {
    let offers = &app.assist.offers;
    offers.iter().find(|o| o.id == app.ui.assistant).or_else(|| offers.iter().find(|o| o.readiness == Readiness::Ready)).or_else(|| offers.first())
}

/// What the assistant can do now, for the session.
fn status(app: &WordApp) -> AssistantStatus {
    let live = app.conversation().filter(|a| !a.has_ended());
    let scripted = app.assist.next.is_some() && desktop(app);
    let ready = app.has_document() && (scripted || live.is_some() || chosen(app).is_some_and(|o| o.readiness == Readiness::Ready));
    AssistantStatus { window: true, ready, busy: live.is_some_and(Assistant::is_busy), desktop_only: !desktop(app), chatgpt: chatgpt::status(app) }
}

/// Every frame: take what came back, end a conversation whose shared document closed, ask
/// again what is offered now and then, and tell the session what the assistant can do.
pub(crate) fn tick(app: &mut WordApp) {
    sync_unattended(app);
    if let Some(c) = app.assist.conversation.as_mut() {
        c.assistant.poll();
        if c.assistant.room().is_closed() {
            c.assistant.end();
        }
    }
    // A permission request waits for the person: the pane opens to ask them. Closed while it
    // waits (Home › Assistant, the pane's close), the request is answered cancelled, so the
    // turn doesn't wait, hidden, for an answer no one can give.
    if let Some(id) = app.conversation().and_then(Assistant::pending_permission).map(|r| r.id) {
        if app.assist.opened_for != Some(id) {
            app.assist.opened_for = Some(id);
            app.session.view.assistant_pane = true;
            // The question is in the conversation.
            app.assist.view = PaneView::Conversation;
        } else if !app.session.view.assistant_pane
            && let Some(a) = app.conversation_mut()
        {
            a.answer(id, Answer::Cancelled);
        }
    }
    note_output(app);
    take_sources(app);
    // A sign-in or a sign-out that ended changes what ChatGPT's offer says: ask again now.
    let account_changed = chatgpt::tick(app);
    #[cfg(not(target_arch = "wasm32"))]
    models::tick(app, account_changed);
    let stale = account_changed
        || match app.assist.offers_at {
            None => true,
            Some(at) => app.session.view.assistant_pane && crate::clock::now_ms() - at > OFFERS_EVERY * 1000.0,
        };
    if stale {
        refresh_offers(app);
    }
    app.session.assistant = status(app);
}

/// While the pane is open, bring the conversation's record of what it changed up to date, when the
/// document changed: the Review list forgets a change once it is resolved, and this remembers how
/// it ended. It costs a walk of the document's tracked changes, as Accept all's count does.
fn note_output(app: &mut WordApp) {
    let document = app.session.document_id();
    let at = (document, app.session.rev());
    if !app.session.view.assistant_pane || app.assist.output_at == Some(at) {
        return;
    }
    if let Some(c) = app.assist.conversation.as_mut().filter(|c| c.document == document) {
        c.assistant.note_output(&app.session);
        app.assist.output_at = Some(at);
    }
}

/// After command `id` ran in the window with result `r`: for an `assist.*` command, act on the
/// requests it made (the last one's answer is the result), and tell the conversation the
/// setting for requests no one can answer, which holds from a running turn's next request.
pub(crate) fn after_command(app: &mut WordApp, id: &str, r: Result<Value, String>) -> Result<Value, String> {
    if !id.starts_with("assist.") {
        return r;
    }
    let r = if r.is_ok() { take_requests(app).unwrap_or(r) } else { r };
    sync_unattended(app);
    r
}

/// Act on the `assist` requests the command that just ran made, taking them out of the
/// session's: the last one's answer, when there was one.
pub(crate) fn take_requests(app: &mut WordApp) -> Option<Result<Value, String>> {
    if !app.session.ui_requests.iter().any(|r| r.get("assist").is_some()) {
        return None;
    }
    let (mine, rest): (Vec<Value>, Vec<Value>) = std::mem::take(&mut app.session.ui_requests).into_iter().partition(|r| r.get("assist").is_some());
    app.session.ui_requests = rest;
    let mut last = None;
    for req in mine {
        last = Some(handle(app, &req));
    }
    last
}

/// One request of the engine's `assist.*` commands, which checked its params.
pub(crate) fn handle(app: &mut WordApp, req: &Value) -> Result<Value, String> {
    let r = match req.get("assist").and_then(Value::as_str).unwrap_or("") {
        "pane" => {
            if req.get("open").and_then(Value::as_bool) == Some(true) {
                app.assist.focus_prompt = FOCUS_FRAMES;
                refresh_offers(app);
                #[cfg(not(target_arch = "wasm32"))]
                models::retry_failed(app);
            }
            Ok(json!({"value": app.session.view.assistant_pane}))
        }
        "ask" => {
            let prompt = req.get("prompt").and_then(Value::as_str).unwrap_or("");
            let role = req.get("role").and_then(Value::as_str).and_then(Role::named);
            let at_end = req.get("at").and_then(Value::as_str) == Some("end");
            ask(app, prompt, role, at_end)
        }
        "cancel" => {
            let stopping = app.conversation_mut().is_some_and(|a| {
                let busy = a.is_busy();
                a.cancel();
                busy
            });
            Ok(json!({"stopping": stopping}))
        }
        "useDocument" => {
            if !app.has_document() {
                return Err("no document is open".into());
            }
            let switched = app.shared_room().is_some() && !app.shows_shared_document();
            if switched {
                end(app);
                app.stop_sharing();
                app.assist.notice = None;
            }
            Ok(json!({"switched": switched}))
        }
        "backend" => backend(app, req.get("id").and_then(Value::as_str)),
        "details" => details(app),
        "knowledge" => {
            if let Some(n) = req.get("remove").and_then(Value::as_u64) {
                remove_source(app, usize::try_from(n).unwrap_or(usize::MAX))?;
            }
            if let Some(path) = req.get("add").and_then(Value::as_str) {
                add_source_file(app, path);
            }
            Ok(sources_json(app))
        }
        "choice" => choice(app, req),
        "show" => {
            let last = req.get("version").and_then(Value::as_u64).unwrap_or(0);
            show_change(app, req.get("from").and_then(Value::as_u64).unwrap_or(last), last)
        }
        "chatgptSignIn" => chatgpt::sign_in(app),
        "chatgptCancel" => chatgpt::cancel(app),
        "chatgptSignOut" => chatgpt::sign_out(app),
        other => Err(format!("`{}`: not an assistant request", other.chars().take(40).collect::<String>())),
    };
    app.session.assistant = status(app);
    r
}

/// The conversation in summary, as the pane's Details shows it. Empty without a conversation.
fn details(app: &mut WordApp) -> Result<Value, String> {
    serde_json::to_value(details_for(app)).map_err(|e| e.to_string())
}

/// Where `words` are: in the document as it reads, or in an attached source. The document's text is
/// read once a revision, and only when an answer cites anything.
pub(crate) fn cited(app: &mut WordApp, words: &str) -> Cited {
    let at = (app.session.document_id(), app.session.rev(), app.assist.knowledge_rev);
    if app.assist.cited.as_ref().is_none_or(|held| held.at != at) {
        let text = wordcraft_assist::context::final_text(&app.session.doc);
        app.assist.cited = Some(CitedHeld { at, text, found: HashMap::new() });
    }
    if let Some(found) = app.assist.cited.as_ref().and_then(|held| held.found.get(words)) {
        return found.clone();
    }
    let flat = wordcraft_assist::context::flat(words);
    let in_document = app.assist.cited.as_ref().is_some_and(|held| held.text.contains(&flat));
    let found = if in_document {
        Cited::Document
    } else {
        sources(app).iter().find(|s| s.text.contains(words)).map_or(Cited::Nowhere, |s| Cited::Source(s.name.clone()))
    };
    if let Some(held) = app.assist.cited.as_mut() {
        held.found.insert(words.to_string(), found.clone());
    }
    found
}

/// Select `words` in the document, as they read: true when they are there.
pub(crate) fn select_cited(app: &mut WordApp, words: &str) -> bool {
    let Some((anchor, focus)) = wordcraft_assist::context::find_in_final(&app.session.doc, words) else { return false };
    let range = json!({"anchor": wordcraft_engine::cmd::pos_json(&anchor), "focus": wordcraft_engine::cmd::pos_json(&focus)});
    app.run("select.range", range).is_ok()
}

/// The conversation in summary, with its citations looked for: a made-up one is only counted.
pub(crate) fn details_for(app: &mut WordApp) -> wordcraft_assist::Details {
    let mut details = app.conversation().map(Assistant::details).unwrap_or_default();
    for turn in &mut details.turns {
        let citations = std::mem::take(&mut turn.citations);
        for mut citation in citations {
            match cited(app, &citation.words) {
                Cited::Document => turn.citations.push(citation),
                Cited::Source(name) => {
                    citation.source = Some(name);
                    turn.citations.push(citation);
                }
                Cited::Nowhere => turn.unfound = turn.unfound.saturating_add(1),
            }
        }
    }
    details
}

/// What `assist.knowledge` answers.
fn sources_json(app: &WordApp) -> Value {
    let list: Vec<Value> = sources(app).iter().map(|s| json!({"name": s.name, "bytes": s.text.len(), "size": s.size, "cut": s.cut})).collect();
    json!({"sources": list, "pending": app.assist.adding.len(), "error": app.assist.notice})
}

/// Why a prompt waits: it would go without the source the person just chose.
const READING_SOURCE: &str = "a source is still being read: ask again in a moment";

/// Whether a source for this document is still being read.
pub(crate) fn sources_being_read(app: &WordApp) -> bool {
    !app.assist.adding.is_empty() && app.assist.knowledge_for == Some(app.session.document_id())
}

/// The sources attached in the document shown.
pub(crate) fn sources(app: &WordApp) -> &[Knowledge] {
    if app.assist.knowledge_for == Some(app.session.document_id()) { &app.assist.knowledge } else { &[] }
}

/// Whether there is room for one more source. The list is the document shown's from here on.
fn source_room(app: &mut WordApp) -> bool {
    let document = app.session.document_id();
    if app.assist.knowledge_for != Some(document) {
        app.assist.knowledge_rev = app.assist.knowledge_rev.wrapping_add(1);
        app.assist.knowledge.clear();
        app.assist.adding.clear();
        app.assist.knowledge_for = Some(document);
    }
    let room = app.assist.knowledge.len().saturating_add(app.assist.adding.len()) < MAX_SOURCES;
    if !room {
        app.assist.notice = Some(format!("a conversation holds at most {MAX_SOURCES} sources: remove one first"));
    }
    room
}

/// Read the file at `path` as a source, off the frame thread: a large one takes time to import.
/// Whoever asked has had the file rule applied, or chose the file themselves.
pub(crate) fn add_source_file(app: &mut WordApp, path: &str) {
    if !source_room(app) {
        return;
    }
    let path = std::path::PathBuf::from(path);
    let (tx, rx) = channel();
    let wake = app.ctx.clone();
    let reading = std::thread::Builder::new().name("assistant source".into()).spawn(move || {
        let _ = tx.send(read_source(&path));
        if let Some(ctx) = wake {
            ctx.request_repaint();
        }
    });
    match reading {
        Ok(_) => app.assist.adding.push(rx),
        Err(e) => app.assist.notice = Some(format!("the source couldn't be read: {e}")),
    }
}

/// Most bytes of a file read as a source: its text is cut to much less, but only after importing it.
const MAX_SOURCE_FILE: u64 = 16 << 20;

/// Files that are their text already: read as they are, since a text importer makes a paragraph of
/// every line, which a file of line breaks would make millions of.
const PLAIN_TEXT: &[&str] = &["txt", "text", "md", "markdown", "csv", "tsv", "log"];

/// `path` as a source, read as far as the limit from the file as it is now: through the importers
/// documents open with, or as it is when it is plain text.
fn read_source(path: &std::path::Path) -> Result<Knowledge, String> {
    use std::io::Read as _;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let fail = |why: String| format!("{name} couldn't be read as a source: {why}");
    let file = std::fs::File::open(path).map_err(|e| fail(e.to_string()))?;
    let mut bytes = Vec::new();
    file.take(MAX_SOURCE_FILE.saturating_add(1)).read_to_end(&mut bytes).map_err(|e| fail(e.to_string()))?;
    if u64::try_from(bytes.len()).is_ok_and(|n| n > MAX_SOURCE_FILE) {
        return Err(fail(format!("it is larger than {} MB", MAX_SOURCE_FILE >> 20)));
    }
    let extension = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if PLAIN_TEXT.contains(&extension.as_str()) {
        return Ok(Knowledge::new(&name, &wordcraft_engine::io::decode_text(&bytes)));
    }
    let doc = wordcraft_engine::io::open_bytes(&name, &bytes).map_err(fail)?;
    Ok(Knowledge::new(&name, &doc.plain_text(wordcraft_doc::StoryRef::Body)))
}

/// The start of `doc`'s text, past what a source keeps: as much as it takes to say it was cut, read
/// without copying the rest of a long document.
fn text_for_source(doc: &wordcraft_doc::Document) -> String {
    use wordcraft_doc::StoryRef::Body;
    let mut text = String::new();
    for path in doc.para_paths(Body) {
        if text.len() > knowledge::MAX_SOURCE_BYTES {
            break;
        }
        if let Some(p) = doc.para(Body, &path) {
            text.push_str(&p.plain_text());
        }
        text.push('\n');
    }
    text
}

/// Attach open document `id` as a source.
pub(crate) fn add_source_document(app: &mut WordApp, id: u64, name: &str) {
    if source_room(app) {
        match app.session.with_document(id, |s| text_for_source(&s.doc)) {
            Ok(text) => {
                app.assist.knowledge.push(Knowledge::new(name, &text));
                app.assist.knowledge_rev = app.assist.knowledge_rev.wrapping_add(1);
            }
            Err(e) => app.assist.notice = Some(e.to_string()),
        }
    }
}

/// Remove source `n`.
pub(crate) fn remove_source(app: &mut WordApp, n: usize) -> Result<(), String> {
    if n >= sources(app).len() {
        return Err(format!("`remove`: there are {} sources", sources(app).len()));
    }
    app.assist.knowledge.remove(n);
    app.assist.knowledge_rev = app.assist.knowledge_rev.wrapping_add(1);
    Ok(())
}

/// Take the sources that were read.
fn take_sources(app: &mut WordApp) {
    let mut adding = std::mem::take(&mut app.assist.adding);
    adding.retain(|rx| match rx.try_recv() {
        Ok(Ok(source)) => {
            app.assist.knowledge.push(source);
            app.assist.knowledge_rev = app.assist.knowledge_rev.wrapping_add(1);
            false
        }
        Ok(Err(why)) => {
            app.assist.notice = Some(why);
            false
        }
        Err(TryRecvError::Empty) => true,
        Err(TryRecvError::Disconnected) => false,
    });
    app.assist.adding = adding;
}

/// The assistants offered, whether each is ready, and the one chosen; with `id`, choose it.
fn backend(app: &mut WordApp, id: Option<&str>) -> Result<Value, String> {
    refresh_offers(app);
    if let Some(id) = id {
        if !app.assist.offers.iter().any(|o| o.id == id) {
            let ids: Vec<&str> = app.assist.offers.iter().map(|o| o.id.as_str()).collect();
            return Err(if ids.is_empty() {
                format!("`{id}`: no assistant is available here yet")
            } else {
                format!("`{id}`: no such assistant here (there are {})", ids.join(", "))
            });
        }
        app.ui.assistant = id.to_string();
    }
    Ok(json!({"chosen": chosen(app).map(|o| o.id.clone()), "offers": app.assist.offers}))
}

/// The backend the next turn runs on, whose choice of model and effort applies: a scripted one
/// about to start, else the one chosen.
pub(crate) fn backend_in_use(app: &WordApp) -> Option<String> {
    app.assist.next.as_ref().map(|b| b.info().id).or_else(|| chosen(app).map(|o| o.id.clone()))
}

/// `assist.model` and `assist.effort`, which checked their params: with `set`, keep the choice
/// for the backend in use (null: its default), and either way say what is chosen and what ran.
fn choice(app: &mut WordApp, req: &Value) -> Result<Value, String> {
    if app.assist.offers_at.is_none() {
        refresh_offers(app);
    }
    let backend = backend_in_use(app).ok_or(wordcraft_engine::cmd::assist::NOT_SET_UP)?;
    if req.get("set").and_then(Value::as_bool) == Some(true) {
        let value = req.get("value").and_then(Value::as_str).map(str::to_string);
        let is_model = req.get("which").and_then(Value::as_str) == Some("model");
        if let (Some(value), Some(offer)) = (&value, models::menus(app)) {
            let offered = if is_model { offer.offers_model(value) } else { offer.offers_effort(value) };
            if !offered {
                let rows = if is_model { &offer.models } else { &offer.efforts };
                let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
                return Err(format!("`{value}`: {backend} doesn't offer it (it offers {})", ids.join(", ")));
            }
        }
        let models = &mut app.session.prefs.assistant.models;
        let entry = models.entry(backend.clone()).or_default();
        if is_model {
            entry.model = value;
        } else {
            entry.effort = value;
        }
        if entry.model.is_none() && entry.effort.is_none() {
            models.remove(&backend);
        }
    }
    let chosen = app.session.prefs.assistant.models.get(&backend).cloned().unwrap_or_default();
    Ok(json!({"backend": backend, "chosen": chosen, "running": running(app), "offered": models::menus(app)}))
}

/// What the conversation's last turn says ran.
fn running(app: &WordApp) -> Value {
    let entries = app.conversation().map(|a| a.transcript().entries().rev().collect::<Vec<_>>()).unwrap_or_default();
    let line = entries.into_iter().find_map(|e| match e {
        wordcraft_assist::Entry::Model { name, effort, asked } => Some(json!({"model": name, "effort": effort, "asked": asked})),
        _ => None,
    });
    line.unwrap_or(Value::Null)
}

/// Select what the conversation's assistant put in or changed in versions `first` to `last` of
/// the shared document (a tool call's), as the document is now, and open the Review tab, where
/// Accept and Reject act on the selection. Others' changes among those versions are left out.
fn show_change(app: &mut WordApp, first: u64, last: u64) -> Result<Value, String> {
    let r = show_change_inner(app, first, last);
    app.assist.notice = r.as_ref().err().cloned();
    r
}

fn show_change_inner(app: &mut WordApp, first: u64, last: u64) -> Result<Value, String> {
    let room = app.shared_room().filter(|_| app.shows_shared_document()).ok_or("the document the assistant changed isn't the one shown")?;
    // The versions are the conversation's room's: once it closed, they mean nothing in the
    // document shared since.
    let (theirs, name) = app.conversation().map(|a| (a.room().clone(), a.name().to_string())).ok_or("no conversation made that change")?;
    if theirs.is_closed() || !Arc::ptr_eq(&theirs, &room) {
        return Err("that change was made while another sharing of the document was on: look for it in the Review tab".into());
    }
    // What the room took since the last frame, the change among it.
    app.catch_up_share();
    let (anchor, focus) =
        room.where_changed(first..=last, Some(&name)).ok_or("that change isn't in the document any more: look for it in the Review tab")?;
    let doc = &app.session.doc;
    let (Some(anchor), Some(focus)) = (doc.pos_of(anchor, None), doc.pos_of(focus, None)) else {
        return Err("the text that change made isn't in the document any more".into());
    };
    let range = json!({"anchor": wordcraft_engine::cmd::pos_json(&anchor), "focus": wordcraft_engine::cmd::pos_json(&focus)});
    app.execute("select.range", range)?;
    app.execute("ui.tab", json!({"tab": "Review"}))?;
    Ok(json!({"version": last, "from": first, "shown": true}))
}

/// How a turn no one can answer (one a program asked for) answers its agent's permission
/// requests: per the person's setting (`assist.allowUnattended`), refused unless they allowed it.
fn unattended(app: &WordApp) -> PermissionPolicy {
    if app.session.prefs.assistant.allow_unattended { PermissionPolicy::Allow } else { PermissionPolicy::Reject }
}

/// Tell the conversation the person's setting for requests no one can answer: a program's turn
/// reads it for each request, so turning it off holds from a running turn's next one.
pub(crate) fn sync_unattended(app: &WordApp) {
    if let Some(c) = &app.assist.conversation {
        c.assistant.set_unattended(app.session.prefs.assistant.allow_unattended);
    }
}

/// End the conversation there is, if any (its transcript stays for the pane).
fn end(app: &mut WordApp) {
    if let Some(c) = app.assist.conversation.as_mut() {
        c.assistant.end();
    }
}

/// The role a prompt is for: the one it names, else the one chosen in the pane.
fn wanted(app: &WordApp, role: Option<Role>) -> Option<Role> {
    role.or(app.assist.role)
}

/// Ask `prompt` (checked by `assist.ask` or a writing action), as `role` when it names one: in
/// the conversation there is, or in a new one when there is none, it ended, it is about another
/// document or another assistant, or it has another role. The assistant starts with the
/// person's selection, or with the caret at its end (`at_end`). The pane opens, so the person
/// sees the turn and can stop it; the keyboard stays where it is.
pub(crate) fn ask(app: &mut WordApp, prompt: &str, role: Option<Role>, at_end: bool) -> Result<Value, String> {
    let r = ask_inner(app, prompt, role, at_end);
    app.assist.notice = r.as_ref().err().cloned();
    if r.is_ok() {
        app.session.view.assistant_pane = true;
        if role.is_none() {
            app.assist.role = None;
        }
    }
    r
}

fn ask_inner(app: &mut WordApp, prompt: &str, role: Option<Role>, at_end: bool) -> Result<Value, String> {
    if !app.has_document() {
        return Err("no document is open".into());
    }
    if sources_being_read(app) {
        return Err(READING_SOURCE.into());
    }
    let role = wanted(app, role);
    let chosen_id = chosen(app).map(|o| o.id.clone());
    let keep = app.conversation().is_some_and(|a| {
        !a.has_ended() && role.is_none_or(|r| r == a.role()) && (app.assist.next.is_none()) && chosen_id.as_deref().is_none_or(|id| id == a.info().id)
    });
    if !keep {
        start(app, role)?;
    }
    let caps = Caps::default();
    let (context, mut sent) = Context::of(&app.session, &caps).rendered(&caps);
    let (carried, given) = knowledge::for_turn(sources(app));
    sent.knowledge = given;
    // The person's selection, by paragraph id: the assistant's copy finds it there, though the
    // paragraphs moved with edits it has and the window hasn't yet.
    let doc = &app.session.doc;
    let (anchor, focus) = if at_end {
        let (start, mut end) = app.session.sel.ordered();
        // Select Paragraph includes the next sibling's start; insert before the selected paragraph's mark.
        if start.story == end.story
            && start.path != end.path
            && end.off == 0
            && let Some(i) = end.path.last().checked_sub(1)
            && let previous = end.path.with_last(i)
            && let Some(p) = doc.para(end.story, &previous)
        {
            end.off = p.len();
            end.path = previous;
        }
        (end.clone(), end)
    } else {
        (app.session.sel.anchor.clone(), app.session.sel.focus.clone())
    };
    let selection = doc.anchor_at(&anchor).zip(doc.anchor_at(&focus));
    // The person is asked what the agent asks in their own turns; a program's turn is answered
    // per the setting, told to the conversation before the turn is sent: its first request may
    // come before this command returns, and must not find a setting from before.
    sync_unattended(app);
    let policy = if app.assist.by_person { PermissionPolicy::Ask } else { unattended(app) };
    let saved = app.conversation().and_then(|a| app.session.prefs.assistant.models.get(&a.info().id)).cloned().unwrap_or_default();
    let Some(a) = app.conversation_mut() else { return Err("the assistant couldn't start".into()) };
    a.set_knowledge(carried);
    a.choose(saved.model, saved.effort);
    a.ask_with_policy(prompt, Some(context), selection, policy).map_err(|e| e.to_string())?;
    a.gave(sent);
    let (name, role, tracked) = (a.name().to_string(), a.role(), a.tracked());
    Ok(json!({"asked": true, "assistant": name, "role": role.name(), "tracked": tracked}))
}

/// Start a conversation as `role` (`None`: the setting): share the document if it isn't shared,
/// and have the assistant join it. The conversation there was ends.
fn start(app: &mut WordApp, role: Option<Role>) -> Result<(), String> {
    if role == Some(Role::Owner) {
        return Err("an assistant can't be an owner".into());
    }
    if app.shared_room().is_some() && !app.shows_shared_document() {
        return Err("another open document is shared: choose Use this document in the Assistant pane to switch".into());
    }
    // The web runs no assistant (#26), not even a scripted one given before or after.
    let backend = match app.assist.next.take().filter(|_| desktop(app)) {
        Some(b) => b,
        None => {
            if app.assist.offers_at.is_none() {
                refresh_offers(app);
            }
            let Some(id) = chosen(app).map(|o| o.id.clone()) else {
                return Err(wordcraft_engine::cmd::assist::NOT_SET_UP.into());
            };
            wordcraft_assist::offers::make(&id, &host(app))?
        }
    };
    // Sources belong to their conversation: those attached since it, for another document, or before
    // the first, go with the new one.
    if app.assist.conversation.as_ref().is_some_and(|c| Some(c.document) == app.assist.knowledge_for) {
        app.assist.knowledge.clear();
        app.assist.knowledge_rev = app.assist.knowledge_rev.wrapping_add(1);
        // One still being read would land in this conversation.
        app.assist.adding.clear();
    }
    end(app);
    if !app.shows_shared_document() {
        app.share_document(None)?;
    }
    let room = app.shared_room().ok_or("the document couldn't be shared with the assistant")?;
    let wake: Option<wordcraft_assist::Waker> = app.ctx.clone().map(|ctx| Arc::new(move || ctx.request_repaint()) as wordcraft_assist::Waker);
    let options = Options {
        setting: Some(app.session.prefs.assistant.default_role),
        role,
        document: app.title_stem(),
        // Each turn says how its requests are answered; this is for one that doesn't.
        permissions: unattended(app),
        wake,
    };
    let assistant = Assistant::start(&room, backend, options).map_err(|e| e.to_string())?;
    app.assist.conversation = Some(Conversation { assistant, document: app.session.document_id() });
    // Its requests are numbered from 1: the pane opens for each of them.
    app.assist.opened_for = None;
    sync_unattended(app);
    Ok(())
}

/// The tones Rewrite offers in menus: the label shown, and the tone asked for.
pub(crate) const TONES: [(&str, &str); 3] = [("Formal", "formal"), ("Friendly", "friendly"), ("Plain", "plain")];

/// Draft at the caret: the pane opens with the keyboard in its prompt box, which then says what
/// to write instead of asking.
pub(crate) fn start_draft(app: &mut WordApp) {
    if app.run("assist.pane", json!({"value": true})).is_ok() {
        app.assist.draft = true;
    }
}

/// A writing action as a menu item, greyed out with the reason in its tooltip.
fn action_item(ui: &mut egui::Ui, app: &mut WordApp, label: &str, id: &str, params: Value) {
    let on = crate::widgets::enabled(app, id);
    let mut r = ui.add_enabled(on, crate::rtl::menu_item(ui, tl!(label), "", 0.0));
    if let Some(why) = app.session.why_disabled(id) {
        r = r.on_disabled_hover_text(tl!(why));
    }
    if r.clicked() {
        let _ = app.run(id, params);
        ui.close();
    }
}

/// The writing actions (#18) as menu items: the mini toolbar's Assistant menu and the
/// right-click menu's. Draft asks what to write in the pane's prompt box.
pub(crate) fn action_items(ui: &mut egui::Ui, app: &mut WordApp) {
    let on = crate::widgets::enabled(app, "assist.draft");
    let mut r = ui.add_enabled(on, crate::rtl::menu_item(ui, tl!("Draft…"), "", 0.0));
    if let Some(why) = app.session.why_disabled("assist.draft") {
        r = r.on_disabled_hover_text(tl!(why));
    }
    if r.clicked() {
        start_draft(app);
        ui.close();
    }
    action_item(ui, app, "Rewrite", "assist.rewrite", json!({}));
    let tones = crate::widgets::enabled(app, "assist.rewrite");
    ui.add_enabled_ui(tones, |ui| {
        ui.menu_button(tl!("Rewrite in a Tone"), |ui| {
            crate::rtl::root(ui, |ui| {
                for (label, tone) in TONES {
                    action_item(ui, app, label, "assist.rewrite", json!({"tone": tone}));
                }
            })
        })
    });
    action_item(ui, app, "Make Shorter", "assist.shorten", json!({}));
    action_item(ui, app, "Make Longer", "assist.expand", json!({}));
    action_item(ui, app, "Proofread", "assist.proofread", json!({}));
    ui.separator();
    action_item(ui, app, "Summarize in a Comment", "assist.summarize", json!({}));
    action_item(ui, app, "Insert a Summary", "assist.summarize", json!({"insert": true}));
}

/// The roles a conversation may have, with their names in the pane and what each lets the
/// assistant do.
pub(crate) fn role_label(role: Role) -> &'static str {
    match role {
        Role::Viewer => "Viewer",
        Role::Commenter => "Commenter",
        Role::Suggester => "Suggester",
        Role::Editor => "Editor",
        Role::Owner => "Owner",
    }
}

/// What a role lets an assistant do, plainly.
pub(crate) fn role_note(role: Role) -> &'static str {
    match role {
        Role::Viewer => "It reads the document and answers here; it changes nothing.",
        Role::Commenter => "It adds comments; it doesn't change the text.",
        Role::Suggester => "Its edits are tracked changes in its name, for you to accept or reject.",
        Role::Editor | Role::Owner => "Its edits won't be tracked: it changes the document directly.",
    }
}

/// A reasoning level as the pane shows it: its name for the levels ChatGPT's list has, else as
/// the backend gave it (an agent's own names).
pub(crate) fn effort_word(level: &str) -> &str {
    match level {
        "none" => tl!("None"),
        "minimal" => tl!("Minimal"),
        "low" => tl!("Low"),
        "medium" => tl!("Medium"),
        "high" => tl!("High"),
        "xhigh" => tl!("Extra High"),
        "max" => tl!("Max"),
        "ultra" => tl!("Ultra"),
        other => other,
    }
}

/// The texts an entry of the conversation shows as the assistant or its backend wrote them,
/// which the interface audit leaves untranslated.
#[cfg(test)]
pub(crate) fn entry_texts(e: &wordcraft_assist::Entry) -> Vec<String> {
    use wordcraft_assist::Entry;
    match e {
        Entry::Asked { text } | Entry::Thought { text } => vec![text.clone()],
        Entry::Answer { text } => crate::panes::answer::shown_texts(text),
        Entry::Tool(call) => {
            let sources = call.sources.iter().flat_map(|s| [s.title.clone(), s.target.clone().unwrap_or_default()]);
            [call.title.clone(), call.detail.clone()].into_iter().chain(sources).collect()
        }
        Entry::Plan { entries } => entries.iter().map(|p| p.content.clone()).collect(),
        Entry::Permission { request, .. } => std::iter::once(request.title.clone()).chain(request.options.iter().map(|o| o.name.clone())).collect(),
        Entry::Model { name, effort, .. } => std::iter::once(name.clone()).chain(effort.clone()).collect(),
        Entry::Agent { name } => vec![name.clone()],
        Entry::Usage(u) => u.cost.iter().cloned().collect(),
        Entry::Context(sent) => std::iter::once(sent.document.clone()).chain(sent.knowledge.iter().map(|s| s.name.clone())).collect(),
        Entry::Output(row) => vec![row.text.clone()],
        Entry::Note { text } => vec![text.clone()],
        Entry::Stopped { .. } => vec![],
        Entry::Error { message } => vec![message.clone()],
    }
}

#[cfg(test)]
mod tests;

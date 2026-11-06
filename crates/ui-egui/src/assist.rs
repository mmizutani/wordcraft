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

use std::sync::Arc;

use serde_json::{Value, json};
use wordcraft_assist::{Answer, Assistant, Backend, Caps, Context, Host, Offer, Options, PermissionPolicy, Readiness};
use wordcraft_engine::cmd::assist::AssistantStatus;
use wordcraft_engine::room::Role;

use crate::WordApp;

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
    /// The conversation there is ends, so the next prompt starts one.
    pub fn assist_with(&mut self, backend: Box<dyn Backend>) {
        end(self);
        self.assist.next = Some(backend);
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
    Host { settings: app.services.settings_dir.clone(), room: app.lobby.addr(), cli: cli(), fake: fake_offered() }
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
    app.assist.offers = wordcraft_assist::offers::offers(&host(app));
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
    let ready = app.has_document() && (app.assist.next.is_some() || live.is_some() || chosen(app).is_some_and(|o| o.readiness == Readiness::Ready));
    AssistantStatus { window: true, ready, busy: live.is_some_and(Assistant::is_busy) }
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
        } else if !app.session.view.assistant_pane
            && let Some(a) = app.conversation_mut()
        {
            a.answer(id, Answer::Cancelled);
        }
    }
    let stale = match app.assist.offers_at {
        None => true,
        Some(at) => app.session.view.assistant_pane && crate::clock::now_ms() - at > OFFERS_EVERY * 1000.0,
    };
    if stale {
        refresh_offers(app);
    }
    app.session.assistant = status(app);
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
        "show" => {
            let last = req.get("version").and_then(Value::as_u64).unwrap_or(0);
            show_change(app, req.get("from").and_then(Value::as_u64).unwrap_or(last), last)
        }
        other => Err(format!("`{}`: not an assistant request", other.chars().take(40).collect::<String>())),
    };
    app.session.assistant = status(app);
    r
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
    let role = wanted(app, role);
    let chosen_id = chosen(app).map(|o| o.id.clone());
    let keep = app.conversation().is_some_and(|a| {
        !a.has_ended() && role.is_none_or(|r| r == a.role()) && (app.assist.next.is_none()) && chosen_id.as_deref().is_none_or(|id| id == a.info().id)
    });
    if !keep {
        start(app, role)?;
    }
    let caps = Caps::default();
    let context = Context::of(&app.session, &caps).to_text(&caps);
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
    // per the setting.
    let policy = if app.assist.by_person { PermissionPolicy::Ask } else { unattended(app) };
    let Some(a) = app.conversation_mut() else { return Err("the assistant couldn't start".into()) };
    a.ask_with_policy(prompt, Some(context), selection, policy).map_err(|e| e.to_string())?;
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
    let backend = match app.assist.next.take() {
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

/// The texts an entry of the conversation shows as the assistant or its backend wrote them,
/// which the interface audit leaves untranslated.
#[cfg(test)]
pub(crate) fn entry_texts(e: &wordcraft_assist::Entry) -> Vec<String> {
    use wordcraft_assist::Entry;
    match e {
        Entry::Asked { text } | Entry::Answer { text } | Entry::Thought { text } => vec![text.clone()],
        Entry::Tool(call) => vec![call.title.clone(), call.detail.clone()],
        Entry::Plan { entries } => entries.iter().map(|p| p.content.clone()).collect(),
        Entry::Permission { request, .. } => std::iter::once(request.title.clone()).chain(request.options.iter().map(|o| o.name.clone())).collect(),
        Entry::Model { name } => vec![name.clone()],
        Entry::Stopped { .. } => vec![],
        Entry::Error { message } => vec![message.clone()],
    }
}

#[cfg(test)]
mod tests;

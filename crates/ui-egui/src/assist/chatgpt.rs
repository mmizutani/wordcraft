//! Sign in with ChatGPT in the window (#19): File › Options › Account, and the Assistant pane
//! while ChatGPT waits for a sign-in.
//!
//! Continue with ChatGPT (`assist.chatgpt.signIn`) starts a sign-in
//! ([`wordcraft_assist::chatgpt::SignIn`]): it listens on `127.0.0.1` for the browser's answer,
//! and the window opens the browser at its address. Every frame [`tick`] asks whether it is
//! done, never waiting for it, and once it is, ChatGPT is offered ready without a restart.
//! Cancel (`assist.chatgpt.cancelSignIn`) stops it, unless the browser's answer is already being
//! finished: then the sign-in is kept and shows as soon as it is saved. Sign Out
//! (`assist.chatgpt.signOut`) deletes the sign-in and asks ChatGPT to end it, on a thread of its
//! own.
//!
//! Signing in and out are the person's alone, as turning on downloading is: the person runs them
//! from their buttons or Search ([`WordApp::run`]). From a script or an agent
//! (`WordApp::execute`: the control channel, MCP) they are an error, and so is a click or key the
//! control channel made (MCP's `click` and `key`), a step of a macro (someone else may have
//! written it) and a request addressed to another document; and they take no key and go on no
//! ribbon or Quick Access Toolbar ([`wordcraft_engine::cmd::assist::person_only`]). The address the browser opens may hold the
//! last sign-in's id token as a hint, so it reaches neither a command's answer nor the log (the
//! desktop app's logger keeps the browser opener's command lines out), and the tokens stay in the
//! settings folder: nothing here holds one.

use serde_json::{Value, json};
use wordcraft_engine::cmd::assist::ChatGptStatus;

use crate::WordApp;

/// Why a script or an agent can't sign in or out.
pub(crate) const YOURS: &str =
    "Only you can sign in to ChatGPT or out of it: use File › Options › Account or the Assistant pane, and click the button yourself";

/// Why there is no sign-in without a settings folder.
#[cfg(not(target_arch = "wasm32"))]
const NO_FOLDER: &str = "WordCraft has no settings folder here to keep a sign-in in";

/// Why a sign-out, or a new sign-in, waits for a cancelled sign-in.
#[cfg(not(target_arch = "wasm32"))]
const FINISHING: &str = "the ChatGPT sign-in you cancelled is still finishing: try again in a moment";

/// Seconds between two readings of who is signed in (another WordCraft may sign in or out).
#[cfg(not(target_arch = "wasm32"))]
const READ_EVERY: f64 = 10.0;

/// How often the window looks again while a sign-in or a sign-out runs, in milliseconds.
#[cfg(not(target_arch = "wasm32"))]
const LOOK_EVERY_MS: u64 = 200;

/// What the last sign-in or sign-out ended with, for the page and the pane. The web app signs
/// in to nothing, so it has only the refusals.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) enum Notice {
    /// Signed in.
    SignedIn,
    /// Signed out; `revoked`: ChatGPT confirmed it ended the sign-in.
    SignedOut { revoked: bool },
    /// A sign-in didn't work: why (the library's words, with no token).
    SignInFailed(String),
    /// A sign-out didn't work: why.
    SignOutFailed(String),
}

/// The window's ChatGPT sign-in.
pub(crate) struct ChatGptState {
    /// Where the sign-in happens: OpenAI's; tests point it at a fake server on this machine.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) endpoints: wordcraft_assist::chatgpt::Endpoints,
    /// The port the sign-in listens on when it's free (0: any).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) port: u16,
    /// The sign-in waiting for the browser, if one is.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) sign_in: Option<wordcraft_assist::chatgpt::SignIn>,
    /// A sign-in the person cancelled that may still be finishing the browser's answer: how it
    /// ends is still taken, so a sign-in saved anyway shows at once rather than at the next
    /// re-read.
    #[cfg(not(target_arch = "wasm32"))]
    cancelled: Option<wordcraft_assist::chatgpt::SignIn>,
    /// The sign-out under way, if one is.
    #[cfg(not(target_arch = "wasm32"))]
    sign_out: Option<wordcraft_assist::chatgpt::SignOut>,
    /// Who is signed in, as last read from the settings folder (no tokens).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) account: Option<wordcraft_assist::chatgpt::Account>,
    /// When it was read (`clock::now_ms`); `None` before the first time.
    #[cfg(not(target_arch = "wasm32"))]
    read_at: Option<f64>,
    /// The last sign-in ended with the person not allowing plan usage: the next one asks again.
    #[cfg(not(target_arch = "wasm32"))]
    declined: bool,
    /// What the last sign-in or sign-out ended with.
    pub(crate) notice: Option<Notice>,
    /// The models the account may use, as last read (#84).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) list: super::models::ModelList,
}

impl Default for ChatGptState {
    fn default() -> Self {
        ChatGptState {
            #[cfg(not(target_arch = "wasm32"))]
            endpoints: wordcraft_assist::chatgpt::Endpoints::default(),
            #[cfg(not(target_arch = "wasm32"))]
            port: wordcraft_assist::chatgpt::DEFAULT_PORT,
            #[cfg(not(target_arch = "wasm32"))]
            sign_in: None,
            #[cfg(not(target_arch = "wasm32"))]
            cancelled: None,
            #[cfg(not(target_arch = "wasm32"))]
            sign_out: None,
            #[cfg(not(target_arch = "wasm32"))]
            account: None,
            #[cfg(not(target_arch = "wasm32"))]
            read_at: None,
            #[cfg(not(target_arch = "wasm32"))]
            declined: false,
            notice: None,
            #[cfg(not(target_arch = "wasm32"))]
            list: super::models::ModelList::Unread,
        }
    }
}

/// Whether the command running now is the person's own: one they ran ([`WordApp::run`]: a
/// button or Search), not with a click or key the control channel made (`WordApp::run` doesn't
/// count those as theirs). Macro steps, keys, ribbon and toolbar buttons and requests addressed
/// to another document never reach here ([`wordcraft_engine::cmd::assist::person_only`]).
fn the_person_asked(app: &WordApp) -> bool {
    app.assist.by_person
}

/// Who is signed in, as the window last read it.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn account(app: &WordApp) -> Option<&wordcraft_assist::chatgpt::Account> {
    app.assist.chatgpt.account.as_ref()
}

/// Where the sign-in stands, for the session (the commands' greying out).
pub(crate) fn status(app: &WordApp) -> ChatGptStatus {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let s = &app.assist.chatgpt;
        if s.sign_out.is_some() {
            ChatGptStatus::SigningOut
        } else if s.sign_in.is_some() {
            ChatGptStatus::SigningIn
        } else if s.account.is_some() {
            ChatGptStatus::SignedIn
        } else {
            ChatGptStatus::SignedOut
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = app;
        ChatGptStatus::SignedOut
    }
}

/// `assist.chatgpt.signIn`: start a sign-in and open the browser at its address; with one
/// waiting, open the browser there again.
pub(crate) fn sign_in(app: &mut WordApp) -> Result<Value, String> {
    if !crate::assist::desktop(app) {
        return Err(wordcraft_engine::cmd::assist::NEEDS_DESKTOP.into());
    }
    if !the_person_asked(app) {
        return Err(YOURS.into());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use wordcraft_assist::chatgpt::{SIGN_IN_WAIT, SignIn, SignInOptions};
        let settings = app.services.settings_dir.clone().ok_or(NO_FOLDER)?;
        let s = &mut app.assist.chatgpt;
        if s.sign_out.is_some() {
            return Err("WordCraft is signing out of ChatGPT: wait for it to finish".into());
        }
        if let Some(waiting) = &s.sign_in {
            app.canvas.open_url = Some(waiting.url().to_string());
            return Ok(json!({"signingIn": true}));
        }
        // A cancelled sign-in may still save what the browser sent: a new one now would race it,
        // and cancelling that one too would lose track of the first.
        if s.cancelled.is_some() {
            return Err(FINISHING.into());
        }
        // Signed in without plan usage, or it was declined: ChatGPT asks about it again.
        let consent = s.declined || s.account.as_ref().is_some_and(|a| !a.plan_usage);
        let options = SignInOptions { endpoints: s.endpoints.clone(), port: s.port, wait: SIGN_IN_WAIT, consent };
        match SignIn::start(&settings, options) {
            Ok(started) => {
                app.canvas.open_url = Some(started.url().to_string());
                s.sign_in = Some(started);
                s.notice = None;
                Ok(json!({"signingIn": true}))
            }
            Err(e) => {
                let why = e.to_string();
                s.notice = Some(Notice::SignInFailed(why.clone()));
                Err(why)
            }
        }
    }
    #[cfg(target_arch = "wasm32")]
    Err(wordcraft_engine::cmd::assist::NEEDS_DESKTOP.into())
}

/// `assist.chatgpt.cancelSignIn`: stop the sign-in waiting for the browser.
pub(crate) fn cancel(app: &mut WordApp) -> Result<Value, String> {
    if !the_person_asked(app) {
        return Err(YOURS.into());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // It stops, and its port closes, unless it is already finishing the browser's answer:
        // [`tick`] still takes how it ends, so a sign-in kept that way shows at once.
        let s = &mut app.assist.chatgpt;
        let cancelled = match s.sign_in.take() {
            Some(waiting) => {
                waiting.cancel();
                s.cancelled = Some(waiting);
                s.notice = None;
                true
            }
            None => false,
        };
        Ok(json!({"cancelled": cancelled}))
    }
    #[cfg(target_arch = "wasm32")]
    Ok(json!({"cancelled": false}))
}

/// `assist.chatgpt.signOut`: delete the sign-in and revoke it, on a thread of its own.
pub(crate) fn sign_out(app: &mut WordApp) -> Result<Value, String> {
    if !crate::assist::desktop(app) {
        return Err(wordcraft_engine::cmd::assist::NEEDS_DESKTOP.into());
    }
    if !the_person_asked(app) {
        return Err(YOURS.into());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let settings = app.services.settings_dir.clone().ok_or(NO_FOLDER)?;
        let s = &mut app.assist.chatgpt;
        if s.sign_out.is_some() {
            return Ok(json!({"signingOut": true}));
        }
        if s.sign_in.is_some() {
            return Err("a ChatGPT sign-in is waiting for your browser: cancel it first".into());
        }
        // A cancelled sign-in may still save what the browser sent: signing out now could be
        // undone by it a moment later.
        if s.cancelled.is_some() {
            return Err(FINISHING.into());
        }
        match wordcraft_assist::chatgpt::SignOut::start(&settings, s.endpoints.clone()) {
            Ok(started) => {
                s.sign_out = Some(started);
                s.notice = None;
                Ok(json!({"signingOut": true}))
            }
            Err(e) => {
                let why = e.to_string();
                s.notice = Some(Notice::SignOutFailed(why.clone()));
                Err(why)
            }
        }
    }
    #[cfg(target_arch = "wasm32")]
    Err(wordcraft_engine::cmd::assist::NEEDS_DESKTOP.into())
}

/// Every frame: take how a sign-in or a sign-out ended, and read again now and then who is
/// signed in. True when that changed what ChatGPT's offer says.
pub(crate) fn tick(app: &mut WordApp) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use wordcraft_assist::chatgpt::AuthError;
        let settings = app.services.settings_dir.clone();
        let now = crate::clock::now_ms();
        let s = &mut app.assist.chatgpt;
        let mut changed = false;
        if let Some(ended) = s.sign_in.as_mut().and_then(|w| w.poll().cloned()) {
            s.sign_in = None;
            changed = true;
            match ended {
                Ok(account) => {
                    s.account = Some(account);
                    s.read_at = Some(now);
                    s.declined = false;
                    s.notice = Some(Notice::SignedIn);
                }
                Err(AuthError::Cancelled) => s.notice = None,
                Err(e) => {
                    s.declined = e == AuthError::Denied;
                    s.notice = Some(Notice::SignInFailed(e.to_string()));
                }
            }
        }
        // A cancelled sign-in that was finishing the browser's answer: kept if it got that far.
        if let Some(ended) = s.cancelled.as_mut().and_then(|w| w.poll().cloned()) {
            s.cancelled = None;
            if let Ok(account) = ended {
                changed = true;
                s.account = Some(account);
                s.read_at = Some(now);
                s.declined = false;
                s.notice = Some(Notice::SignedIn);
            }
        }
        if let Some(ended) = s.sign_out.as_mut().and_then(|w| w.poll().cloned()) {
            s.sign_out = None;
            changed = true;
            s.notice = Some(match ended {
                Ok(done) => {
                    s.account = None;
                    Notice::SignedOut { revoked: done.revoked }
                }
                Err(e) => Notice::SignOutFailed(e.to_string()),
            });
            // What the file says now: deleted, or kept when it couldn't be.
            s.read_at = None;
        }
        let stale = s.read_at.is_none_or(|at| now - at > READ_EVERY * 1000.0);
        if stale && s.sign_out.is_none() {
            let read = settings.as_deref().and_then(wordcraft_assist::chatgpt::account);
            if read != s.account {
                changed |= s.read_at.is_some();
                // Signed in or out elsewhere (another WordCraft, or ChatGPT ended the sign-in):
                // what the last sign-in or sign-out said no longer describes who is signed in.
                s.notice = None;
            }
            s.account = read;
            s.read_at = Some(now);
        }
        // Frames keep coming while the browser has the sign-in, so its end shows at once.
        if (s.sign_in.is_some() || s.cancelled.is_some() || s.sign_out.is_some())
            && let Some(ctx) = &app.ctx
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(LOOK_EVERY_MS));
        }
        changed
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = app;
        false
    }
}

/// Run one of the account's commands for a button the person clicked; what went wrong shows
/// beside the buttons.
fn press(app: &mut WordApp, id: &str) {
    if let Err(e) = app.run(id, json!({})) {
        let notice = if id == "assist.chatgpt.signOut" { Notice::SignOutFailed(e) } else { Notice::SignInFailed(e) };
        app.assist.chatgpt.notice = Some(notice);
    }
}

/// The sign-in's buttons and what it says: File › Options › Account draws them in full; the
/// Assistant pane (`compact`) only what gets ChatGPT ready.
pub(crate) fn controls(app: &mut WordApp, ui: &mut egui::Ui, compact: bool) {
    use egui::RichText;
    let t = crate::theme::Tokens::get(ui.ctx());
    let small = |text: &str, color: egui::Color32| egui::Label::new(RichText::new(text).small().color(color)).wrap();
    match status(app) {
        ChatGptStatus::SigningOut => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tl!("Signing out of ChatGPT…"));
            });
        }
        ChatGptStatus::SigningIn => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(tl!("Waiting for your browser…")).font(crate::theme::semibold(13.0)));
            });
            ui.add(small(tl!("Finish signing in to ChatGPT in the browser window that opened."), t.text_dim));
            ui.horizontal(|ui| {
                if ui.button(tl!("Open the Browser Again")).clicked() {
                    press(app, "assist.chatgpt.signIn");
                }
                if ui.button(tl!("Cancel")).clicked() {
                    press(app, "assist.chatgpt.cancelSignIn");
                }
            });
        }
        ChatGptStatus::SignedIn => signed_in(app, ui, &t, compact),
        ChatGptStatus::SignedOut => {
            if !compact {
                ui.add(small(
                    tl!(
                        "Use your ChatGPT Plus or Pro plan for the assistant, with no API key: you sign in with your ChatGPT account in your browser."
                    ),
                    t.text_dim,
                ));
            }
            if ui.button(tl!("Continue with ChatGPT")).clicked() {
                press(app, "assist.chatgpt.signIn");
            }
        }
    }
    let line = match &app.assist.chatgpt.notice {
        None => None,
        Some(Notice::SignedIn) => (!compact).then(|| (tl!("You're signed in to ChatGPT.").to_string(), t.text_dim)),
        Some(Notice::SignedOut { revoked: true }) => Some((tl!("You're signed out of ChatGPT.").to_string(), t.text_dim)),
        Some(Notice::SignedOut { revoked: false }) => Some((
            tl!("You're signed out here, but ChatGPT didn't confirm it ended the sign-in: you can disconnect WordCraft in ChatGPT's settings.")
                .to_string(),
            t.orange,
        )),
        Some(Notice::SignInFailed(why)) => Some((crate::i18n::fmt(tl!("Couldn't sign in: {why}"), &[("why", why.as_str())]), t.red)),
        Some(Notice::SignOutFailed(why)) => Some((crate::i18n::fmt(tl!("Couldn't sign out: {why}"), &[("why", why.as_str())]), t.red)),
    };
    if let Some((text, color)) = line {
        ui.add(egui::Label::new(RichText::new(text).small().color(color)).wrap());
    }
}

/// Who is signed in, whether they let WordCraft use their plan, and Sign Out.
fn signed_in(app: &mut WordApp, ui: &mut egui::Ui, t: &crate::theme::Tokens, compact: bool) {
    use egui::RichText;
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(account) = account(app).cloned() else { return };
        let who = match &account.name {
            Some(name) if !name.trim().is_empty() => {
                crate::i18n::fmt(tl!("Signed in as {name} ({email})"), &[("name", name.trim()), ("email", account.email.as_str())])
            }
            _ => crate::i18n::fmt(tl!("Signed in as {email}"), &[("email", account.email.as_str())]),
        };
        if !compact {
            ui.add(egui::Label::new(RichText::new(who).font(crate::theme::semibold(13.0))).wrap());
        }
        if account.plan_usage {
            if !compact {
                ui.add(
                    egui::Label::new(RichText::new(tl!("The assistant uses your ChatGPT plan, not an API key.")).small().color(t.text_dim)).wrap(),
                );
                ui.hyperlink_to(tl!("See your plan's usage"), wordcraft_assist::chatgpt::USAGE_PAGE);
            }
        } else if !compact {
            ui.add(
                egui::Label::new(
                    RichText::new(tl!(
                        "ChatGPT plan usage isn't allowed for WordCraft, so the assistant can't answer with your plan. Sign in again and allow it (ChatGPT Plus and Pro only)."
                    ))
                    .small()
                    .color(t.orange),
                )
                .wrap(),
            );
        }
        if !account.plan_usage && ui.button(tl!("Sign In Again")).clicked() {
            press(app, "assist.chatgpt.signIn");
        }
        if !compact && ui.button(tl!("Sign Out")).clicked() {
            press(app, "assist.chatgpt.signOut");
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (app, ui, t, compact, RichText::new(""));
    }
}

#[cfg(test)]
mod tests;

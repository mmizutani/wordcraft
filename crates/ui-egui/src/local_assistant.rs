//! The optional ChatGPT rewrite action supplied by the local web server.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use egui::Ui;
use serde_json::json;
use wordcraft_doc::para::OBJ;
use wordcraft_doc::styles::PartSet;
use wordcraft_doc::{CharProps, Document, Pos};
use wordcraft_engine::room::Role;
use wordcraft_engine::{Selection, Session};

use crate::WordApp;

const AUTHOR: &str = "ChatGPT";
const MAX_TEXT_BYTES: usize = 8 * 1024;

pub enum Request {
    Status,
    Connect,
    Disconnect,
    Rewrite(String),
}

pub enum Reply {
    Status { connected: bool, connecting: bool },
    AuthorizationUrl(String),
    Disconnected,
    Replacement(String),
}

/// The browser shell supplies this only for pages served by the local Assistant server.
pub type Service = Box<dyn Fn(Request) -> Receiver<Result<Reply, String>>>;

#[derive(Clone, Copy)]
enum Account {
    Disconnected,
    Connecting,
    Connected,
}

enum Job {
    Status,
    Connect,
    Disconnect,
    Rewrite(Rewrite),
    Discard,
}

#[derive(Default)]
pub(crate) struct State {
    account: Option<Account>,
    authorization_url: Option<String>,
    poll_at: f64,
    pending: Option<(Job, Receiver<Result<Reply, String>>)>,
    notice: Option<String>,
    staged: Option<((u64, u64), Selection)>,
}

pub(crate) fn tick(app: &mut WordApp, ctx: &egui::Context) {
    if app.services.local_assistant.is_none() {
        return;
    }
    let window_editable = app.has_document() && !app.ui.backstage && !app.modal_open();
    let state = &mut app.assist.local;
    if let Some((job, reply)) = &mut state.pending {
        if let Job::Rewrite(target) = job
            && (!window_editable || !target.matches(&app.session))
        {
            *job = Job::Discard;
            state.notice = Some(stale_notice());
            ctx.request_repaint();
        }
        let result = match reply.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Disconnected) => Err(tl!("The local Assistant stopped responding.").into()),
            Err(TryRecvError::Empty) => return,
        };
        if let Some((job, _)) = state.pending.take() {
            finish(app, job, result);
            ctx.request_repaint();
        }
    }
    if !app.session.view.assistant_pane || app.ui.backstage {
        return;
    }
    let state = &mut app.assist.local;
    let now = crate::clock::now_ms();
    if state.poll_at <= now
        && let Some(service) = &app.services.local_assistant
    {
        state.poll_at = now + if matches!(state.account, Some(Account::Connecting)) { 1_000.0 } else { 10_000.0 };
        state.pending = Some((Job::Status, service(Request::Status)));
    }
    ctx.request_repaint_after(Duration::from_secs_f64(((state.poll_at - now) / 1000.0).max(0.0)));
}

fn finish(app: &mut WordApp, job: Job, result: Result<Reply, String>) {
    let state = &mut app.assist.local;
    match (job, result) {
        (Job::Discard, _) => {}
        (Job::Status, Ok(Reply::Status { connected, connecting })) => {
            state.account = Some(if connected {
                Account::Connected
            } else if connecting {
                Account::Connecting
            } else {
                Account::Disconnected
            });
            if !connecting || connected {
                state.authorization_url = None;
            }
        }
        (Job::Connect, Ok(Reply::AuthorizationUrl(url))) => {
            state.account = Some(Account::Connecting);
            state.authorization_url = Some(url);
            state.poll_at = 0.0;
        }
        (Job::Disconnect, Ok(Reply::Disconnected)) => {
            state.account = Some(Account::Disconnected);
            state.authorization_url = None;
        }
        (Job::Rewrite(target), Ok(Reply::Replacement(text))) => match target.stage(&mut app.session, &text) {
            Ok(Some(selection)) => {
                state.staged = Some((at(&app.session), selection));
                state.notice = None;
            }
            Ok(None) => state.notice = Some(tl!("ChatGPT returned the same text.").into()),
            Err(message) => state.notice = Some(message),
        },
        (Job::Status, _) => state.account = None,
        (_, result) => {
            state.poll_at = 0.0;
            state.notice = Some(result.err().unwrap_or_else(|| tl!("The local Assistant returned an unexpected response.").into()));
        }
    }
}

pub(crate) fn show(app: &mut WordApp, ui: &mut Ui) {
    let Some(service) = &app.services.local_assistant else { return };
    let state = &mut app.assist.local;
    let waiting = state.pending.is_some();
    let mut paragraph_to_select = None;
    match state.account {
        Some(Account::Connected) => {
            ui.label(tl!("Connected to ChatGPT."));
            if ui.add_enabled(!waiting, egui::Button::new(tl!("Disconnect"))).clicked() {
                state.notice = None;
                state.pending = Some((Job::Disconnect, service(Request::Disconnect)));
            }
            let selection = Rewrite::capture(&app.session);
            ui.label(tl!("Rewrite sends the selected text to ChatGPT using your plan."));
            if ui.add_enabled(!waiting && selection.is_ok(), egui::Button::new(tl!("Rewrite selected text"))).clicked()
                && let Ok(selection) = selection
            {
                state.staged = None;
                state.notice = None;
                let reply = service(Request::Rewrite(selection.text.clone()));
                state.pending = Some((Job::Rewrite(selection), reply));
            } else if let Err(blocked) = selection {
                let caret = app.session.sel.is_collapsed();
                match if caret { current_paragraph(&app.session) } else { Err(blocked) } {
                    Ok(paragraph) => {
                        if ui.add_enabled(!waiting, egui::Button::new(tl!("Select current paragraph"))).clicked() {
                            paragraph_to_select = Some(paragraph);
                        }
                    }
                    Err(Blocked::TrackedChanges) if !caret => {
                        ui.label(tl!("This selection contains tracked changes. Accept or reject them in Review, or select other text."));
                    }
                    Err(Blocked::HiddenText) => {
                        ui.label(tl!("Hidden text cannot be rewritten. Select visible text to rewrite."));
                    }
                    Err(_) => {
                        ui.label(tl!("Select up to 8 KiB of editable text within one paragraph."));
                    }
                }
            }
        }
        Some(Account::Connecting) => {
            ui.label(tl!("Finish signing in with ChatGPT in your browser."));
            if let Some(url) = &state.authorization_url {
                ui.add(egui::Hyperlink::from_label_and_url(tl!("Open ChatGPT sign-in"), url).open_in_new_tab(true));
            }
            if ui.add_enabled(!waiting, egui::Button::new(tl!("Cancel sign-in"))).clicked() {
                state.notice = None;
                state.pending = Some((Job::Disconnect, service(Request::Disconnect)));
            }
        }
        Some(Account::Disconnected) => {
            if ui.add_enabled(!waiting, egui::Button::new(tl!("Continue with ChatGPT"))).clicked() {
                state.notice = None;
                state.pending = Some((Job::Connect, service(Request::Connect)));
            }
        }
        None if waiting || state.poll_at == 0.0 => {
            ui.label(tl!("Checking ChatGPT connection…"));
        }
        None => {
            ui.label(tl!("The local Assistant could not check the connection."));
        }
    }
    match &state.pending {
        Some((Job::Rewrite(_), _)) => {
            ui.label(tl!("Rewriting selected text…"));
        }
        Some((Job::Connect, _)) => {
            ui.label(tl!("Preparing ChatGPT sign-in…"));
        }
        _ => {}
    }
    if let Some(message) = &state.notice {
        ui.label(message);
    }
    if let Some((staged_at, selection)) = &state.staged
        && *staged_at == at(&app.session)
    {
        ui.label(tl!("ChatGPT proposed a tracked rewrite."));
        ui.label(tl!("Use Accept or Reject on the Review tab."));
        if ui.button(tl!("Review rewrite")).clicked() {
            let params = range_params(selection);
            if app.run("select.range", params).is_ok() {
                let _ = app.run("review.markup", json!({"value": "all"}));
                let _ = app.run("ui.tab", json!({"tab": "Review"}));
            }
        }
    }
    if let Some(paragraph) = paragraph_to_select {
        let _ = app.run("select.range", range_params(&paragraph));
    }
}

fn range_params(selection: &Selection) -> serde_json::Value {
    json!({"anchor": selection.anchor, "focus": selection.focus})
}

fn at(session: &Session) -> (u64, u64) {
    (session.document_id(), session.rev())
}

fn stale_notice() -> String {
    tl!("The selection or document changed. Select the text and try Rewrite again.").into()
}

fn text_stream(session: &Session) -> bool {
    session.column_segments().is_none() && session.cell_range().is_none() && session.math.is_none()
}

#[derive(Debug)]
enum Blocked {
    TrackedChanges,
    HiddenText,
    Other,
}

fn selected_text<'a>(session: &'a Session, selection: &Selection) -> Result<&'a str, Blocked> {
    if selection.is_collapsed() || !text_stream(session) || session.role.is_some() || session.why_disabled("text.insert").is_some() {
        return Err(Blocked::Other);
    }
    let (start, end) = selection.ordered();
    if start.story != end.story || start.path != end.path || end.off.saturating_sub(start.off) > MAX_TEXT_BYTES {
        return Err(Blocked::Other);
    }
    let paragraph = session.doc.para_at(&start).ok_or(Blocked::Other)?;
    let text = paragraph.text.get(start.off..end.off).filter(|text| valid_text(text)).ok_or(Blocked::Other)?;
    let style = paragraph.props.style.as_deref();
    let table = table_formatting(&session.doc, &start);
    for (_, props) in paragraph.run_ranges().skip_while(|(range, _)| range.end <= start.off).take_while(|(range, _)| range.start < end.off) {
        if props.ins.is_some() || props.del.is_some() || props.fmt_change.is_some() {
            return Err(Blocked::TrackedChanges);
        }
        if session.doc.styles.resolve_char_in(style, table.as_ref(), props).hidden {
            return Err(Blocked::HiddenText);
        }
    }
    Ok(text)
}

/// The cell's style parts overlay in the same order as table layout.
fn table_formatting(doc: &Document, pos: &Pos) -> Option<CharProps> {
    let (path, row, cell) = pos.path.cell()?;
    let table = doc.table(pos.story, &path)?;
    let style = doc.styles.table_style(table.props.style.as_deref()?)?;
    let span = table.rows.get(row)?.cells.get(cell)?.span();
    let parts = PartSet::of_cell(
        &table.props.look,
        (style.parts.band_size.unwrap_or(0), style.parts.col_band_size.unwrap_or(0)),
        (row, table.rows.len()),
        (table.grid_col(row, cell), span, table.cols()),
    );
    let mut props = CharProps::default();
    for part in parts.iter() {
        props.overlay(&style.part(part).chr);
    }
    Some(props)
}

fn current_paragraph(session: &Session) -> Result<Selection, Blocked> {
    let caret = &session.sel.focus;
    let len = session.doc.para_at(caret).ok_or(Blocked::Other)?.len();
    // Select the text only. The engine's Select Paragraph includes the paragraph mark.
    let selection = Selection { anchor: Pos { off: 0, ..caret.clone() }, focus: Pos { off: len, ..caret.clone() } };
    selected_text(session, &selection)?;
    Ok(selection)
}

struct Rewrite {
    at: (u64, u64),
    selection: Selection,
    text: String,
}

impl Rewrite {
    fn capture(session: &Session) -> Result<Self, Blocked> {
        let text = selected_text(session, &session.sel)?;
        Ok(Self { at: at(session), selection: session.sel.clone(), text: text.into() })
    }

    fn matches(&self, session: &Session) -> bool {
        self.at == at(session) && self.selection == session.sel && text_stream(session) && session.role.is_none()
    }

    fn stage(self, session: &mut Session, replacement: &str) -> Result<Option<Selection>, String> {
        if !self.matches(session) {
            return Err(stale_notice());
        }
        if !valid_text(replacement) {
            return Err(tl!("ChatGPT returned text that cannot replace this selection.").into());
        }
        if self.text == replacement {
            return Ok(None);
        }
        // A suggester's command forces tracking, and the engine rolls back any untracked edit.
        let author = std::mem::replace(&mut session.author, AUTHOR.into());
        session.role = Some(Role::Suggester);
        let result = session.run("text.insert", &json!({"text": replacement, "raw": true}));
        session.role = None;
        session.author = author;
        result.map_err(|_| tl!("WordCraft could not stage the rewrite at this selection.").to_string())?;
        let (anchor, mut focus) = self.selection.ordered();
        // The marked deletion stays in the paragraph after the replacement.
        focus.off = focus.off.saturating_add(replacement.len());
        Ok(Some(Selection { anchor, focus }))
    }
}

fn valid_text(text: &str) -> bool {
    text.len() <= MAX_TEXT_BYTES && !text.trim().is_empty() && !text.chars().any(|ch| ch.is_control() || matches!(ch, OBJ | '\u{2028}' | '\u{2029}'))
}

#[cfg(test)]
mod tests;

//! What the window does with the person's cases (#107): which case each document is bound to, what
//! a turn asks to recall from it, what Details › Recalled shows of what came back, and the
//! `memory.case.*` and `memory.wall.*` writes behind Details › Case ([`page`]).
//!
//! The binding is the window's alone and never in a document. A conversation belongs to the case
//! its document was bound to when it started ([`super::Conversation::case`]); a document bound to
//! another case gets a new conversation. Recall runs only when a case is bound and no one but the
//! person and this conversation's assistant can read the document ([`Alone`]); otherwise the turn
//! runs as it does today, and Details says why.
//!
//! Two writers, two doors. The person's click goes through [`super::memory::write`], the one guard,
//! and the shelf's [`Click`] is its proof: only that door confirms a note, sets facts, replaces the
//! person's plan, binds or deletes. A model's `[[case]]` marker goes through [`tick`], the one
//! route, into the conversation's own case ([`super::Conversation::case`]) and never the document's
//! current binding: a note or a balance lands there unconfirmed, a plan replaces only a model's
//! plan, and facts are always a card. The switch is [`ModelWrites`].

use std::collections::{BTreeMap, VecDeque};

use egui::{RichText, Ui};
use serde_json::{Value, json};
use wordcraft_assist::cases::{
    Alone, Applied, BalanceDraft, Body, Card, Case, CaseError, CaseId, CaseRef, Click, Day, Factor, Facts, Listing, ModelWrite, ModelWrites,
    NoteDraft, NoteId, PersonOp, Place, Plan, Quoted, RecallAsk, RecallSent, Route, Shelf, Stance, Standing, Topics, WallId,
};
use wordcraft_assist::{Context, Details, ProposalAnswer, Sent, Written};

use crate::WordApp;
use crate::i18n::fmt;
use crate::theme::Tokens;

pub(crate) mod page;

/// Why a turn recalled nothing, as Details says it.
const NO_CASE: &str = "no case is bound to this document";
const NO_FOLDER: &str = "WordCraft has no settings folder here";
/// Why a write on the bound case did nothing.
const NOT_BOUND: &str = "no case is bound to this document: bind one in the Assistant pane's Details › Case";
const NO_DOCUMENT: &str = "no document is open";
/// The client a case goes in when none is named.
pub(crate) const MY_CASES: &str = "My cases";

/// Milliseconds between two readings of the bound case's file, and two listings, while the page
/// shows: another WordCraft may have written.
const READ_EVERY: f64 = 2000.0;

/// Most writes waiting on a busy lock; more are dropped and logged.
const MAX_UNWRITTEN: usize = 32;

/// Frames a write waits on a busy lock before it is dropped: another WordCraft holding the lock
/// that long is stuck, and the note is only a model's.
const MAX_TRIES: u32 = 600;

/// What the window holds of it.
#[derive(Default)]
pub(crate) struct CasesState {
    /// By document id ([`wordcraft_engine::Session::document_id`]): a window holds several documents.
    bound: BTreeMap<u64, CaseRef>,
    /// The shown document's case as its file last read, with when (`clock::now_ms`).
    open: Option<(CaseRef, Case)>,
    read_at: Option<f64>,
    /// Every client's cases, read on a worker: a folder scan.
    listing: ListState,
    listed_at: Option<f64>,
    /// What the page is in the middle of.
    pub(crate) drafts: page::Drafts,
    /// A model's writes, each for the case its conversation belongs to, waiting for the file:
    /// a busy lock keeps one for the next frame.
    unwritten: VecDeque<(CaseRef, ModelWrite, u32)>,
    /// [`wordcraft_assist::cases::MODEL_WRITES`], which tests change to try the other setting.
    #[doc(hidden)]
    pub model_writes: ModelWrites,
}

/// The last listing read, kept while a newer one is read: a name lookup during the read must
/// still find the client, or Create would make a second one of that name.
#[derive(Default)]
struct ListState {
    last: Option<Listing>,
    pending: Option<std::sync::mpsc::Receiver<Listing>>,
}

/// A stamp unique to this process for a new wall's or case's id ([`Shelf::new_wall`]): the process
/// id, a sub-second time, and a counter that never repeats in the process.
fn stamp() -> (String, u64) {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    #[cfg(not(target_arch = "wasm32"))]
    let pid = std::process::id();
    #[cfg(target_arch = "wasm32")]
    let pid = 0u32;
    (format!("{pid}-{}-{:.3}", wordcraft_engine::cmd::now_iso(), crate::clock::now_ms()), counter)
}

fn today() -> Result<Day, String> {
    Day::parse(wordcraft_engine::cmd::now_iso().get(..10).unwrap_or_default()).ok_or_else(|| "today's date can't be read".to_string())
}

/// The bound case as last read, read again when stale or `force`d. A case whose file is gone
/// (deleted in another window) drops its binding and says so.
pub(crate) fn open_case(app: &mut WordApp, force: bool) -> Option<(CaseRef, Case)> {
    let at = bound_for(app)?;
    let dir = app.services.settings_dir.clone()?;
    let stale = force || app.assist.cases.read_at.is_none_or(|t| crate::clock::now_ms() - t > READ_EVERY);
    if stale || app.assist.cases.open.as_ref().is_none_or(|(held, _)| *held != at) {
        app.assist.cases.read_at = Some(crate::clock::now_ms());
        match Shelf::at(&dir).open(&at) {
            Ok(case) => app.assist.cases.open = Some((at.clone(), case)),
            Err(_) => {
                app.assist.cases.open = None;
                app.bind_case(None);
                app.assist.notice = Some(tl!("This case was deleted, so the document no longer uses it.").to_string());
                return None;
            }
        }
    }
    app.assist.cases.open.clone()
}

/// The listing as last read; `None` until the first read ends. Asks for a new one when stale, on a
/// worker, since it scans every client's folder.
pub(crate) fn listing(app: &mut WordApp) -> Option<Listing> {
    let state = &mut app.assist.cases.listing;
    if let Some(rx) = &state.pending
        && let Ok(listing) = rx.try_recv()
    {
        state.last = Some(listing);
        state.pending = None;
        app.assist.cases.listed_at = Some(crate::clock::now_ms());
    }
    let stale = app.assist.cases.listed_at.is_none_or(|t| crate::clock::now_ms() - t > READ_EVERY);
    if stale && app.assist.cases.listing.pending.is_none() {
        list_on_worker(app);
    }
    app.assist.cases.listing.last.clone()
}

fn list_on_worker(app: &mut WordApp) {
    let Some(dir) = app.services.settings_dir.clone() else { return };
    let ctx = app.ctx.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("case list".into()).spawn(move || {
        let _ = tx.send(Shelf::at(&dir).list());
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    });
    if spawned.is_ok() {
        app.assist.cases.listing.pending = Some(rx);
        // Set now, so a failed send or a slow worker isn't asked again every frame.
        app.assist.cases.listed_at = Some(crate::clock::now_ms());
    }
}

/// After a write: the case and the listing are read again before the next frame shows them.
fn changed(app: &mut WordApp) {
    app.assist.cases.read_at = None;
    app.assist.cases.listed_at = None;
}

/// Every frame: what the conversation's answers marked for its case goes there, by the one route.
/// A marker with no case bound to the conversation is dropped, as a `[[used:]]` for nothing is.
pub(crate) fn tick(app: &mut WordApp) {
    let assist = &mut app.assist;
    if let Some(c) = assist.conversation.as_mut() {
        let marked = c.assistant.take_case_markers();
        if let Some(at) = c.case.clone() {
            for marker in marked {
                match marker.route(assist.cases.model_writes) {
                    Route::Write(write) if assist.cases.unwritten.len() < MAX_UNWRITTEN => assist.cases.unwritten.push_back((at.clone(), write, 0)),
                    Route::Write(_) => log::warn!("a model's case note was dropped: too many wait for the file"),
                    Route::Card(card) => {
                        c.assistant.case_card(card);
                    }
                }
            }
        }
    }
    drain(app);
}

/// The writes waiting, in order, each into its own case. A busy lock stops the frame's draining;
/// any other refusal drops the write, and nothing is retried.
fn drain(app: &mut WordApp) {
    if app.assist.cases.unwritten.is_empty() {
        return;
    }
    let (Some(dir), Ok(today)) = (app.services.settings_dir.clone(), today()) else {
        app.assist.cases.unwritten.clear();
        return;
    };
    let shelf = Shelf::at(&dir);
    while let Some((at, write, tries)) = app.assist.cases.unwritten.pop_front() {
        let (what, text) = describe(&write);
        match shelf.model(&at, write.clone(), today) {
            Err(CaseError::Busy) => {
                if tries < MAX_TRIES {
                    app.assist.cases.unwritten.push_front((at, write, tries.saturating_add(1)));
                } else {
                    log::warn!("a model's case note was dropped: the case's file stayed busy");
                }
                return;
            }
            Err(e) => log::warn!("a model's case note wasn't kept: {e}"),
            Ok(applied) => {
                // The row and the card belong to the conversation that wrote the marker.
                if let Some(c) = app.assist.conversation.as_mut().filter(|c| c.case.as_ref() == Some(&at)) {
                    match applied {
                        Applied::Added(id) | Applied::Replaced { added: id, .. } => c.assistant.case_written(what, Some(id), &text),
                        Applied::Changed => c.assistant.case_written(what, None, &text),
                        Applied::Card(write) => {
                            c.assistant.case_card(Card::Write(write));
                        }
                        Applied::Known => {}
                    }
                }
                changed(app);
            }
        }
    }
}

/// What a write is, and its main text, for its row.
fn describe(write: &ModelWrite) -> (Written, String) {
    match write {
        ModelWrite::Note(draft) => (Written::Note, draft.body.texts().next().map(|q| q.for_person().to_string()).unwrap_or_default()),
        ModelWrite::Plan(plan) => (Written::Plan, plan.goal.for_person().to_string()),
        ModelWrite::Balance(draft) => (Written::Balance, stance_word(Some(draft.stance)).to_string()),
        ModelWrite::Used(_) => (Written::Note, String::new()),
    }
}

/// One of the `memory.case.*` or `memory.wall.*` writes, whose params the command checked for
/// shape and [`super::memory::write`] for the person's click. The objects passed through are parsed
/// here into the typed record, so junk is an error and never a panic.
pub(crate) fn write(app: &mut WordApp, dir: &std::path::Path, what: &str, req: &Value) -> Result<Value, String> {
    let click = Click::by_person(app.assist.by_person).ok_or(super::memory::YOURS)?;
    let shelf = Shelf::at(dir);
    let r = match what {
        "memory.wall.new" => {
            let (stamp, counter) = stamp();
            let id = shelf.new_wall(click, req.get("name").and_then(Value::as_str).unwrap_or(""), &stamp, counter).map_err(|e| e.to_string())?;
            Ok(json!({"wall": id.as_str()}))
        }
        "memory.wall.delete" => {
            let wall = WallId::parse(req.get("wall").and_then(Value::as_str).unwrap_or("")).ok_or("`wall`: not an id")?;
            if bound_for(app).is_some_and(|at| at.wall == wall) {
                app.bind_case(None);
            }
            shelf.delete_empty_wall(click, &wall).map_err(|e| e.to_string()).map(|()| json!({"deleted": wall.as_str()}))
        }
        "memory.case.new" => new_case(app, &shelf, click, req),
        "memory.case.bind" => {
            if !app.has_document() {
                return Err(NO_DOCUMENT.into());
            }
            let at = CaseRef {
                wall: WallId::parse(req.get("wall").and_then(Value::as_str).unwrap_or("")).ok_or("`wall`: not an id")?,
                case: CaseId::parse(req.get("case").and_then(Value::as_str).unwrap_or("")).ok_or("`case`: not an id")?,
            };
            if !shelf.exists(&at) {
                return Err(CaseError::Gone.to_string());
            }
            app.bind_case(Some(at.clone()));
            Ok(json!({"wall": at.wall.as_str(), "case": at.case.as_str()}))
        }
        "memory.case.unbind" => {
            if !app.has_document() {
                return Err(NO_DOCUMENT.into());
            }
            app.bind_case(None);
            Ok(json!({"unbound": true}))
        }
        "memory.case.delete" => {
            let at = bound_for(app).ok_or(NOT_BOUND)?;
            // The conversation holds the case's notes: it ends first, with its transcript.
            app.bind_case(None);
            super::end(app);
            shelf.delete_case(click, &at).map_err(|e| e.to_string()).map(|()| json!({"deleted": at.case.as_str()}))
        }
        _ => {
            let at = bound_for(app).ok_or(NOT_BOUND)?;
            let op = person_op(what, req)?;
            shelf.person(click, &at, op, today()?).map_err(|e| e.to_string()).map(|_| json!({"case": at.case.as_str()}))
        }
    };
    changed(app);
    r
}

/// A case called `title`, in the wall named, the client named (made if no listed wall has that
/// name), or "My cases" with neither; then bound to the document. Before the first listing has
/// come back the folder is listed here, on the frame: a lookup in no listing would make a second
/// client of the name.
fn new_case(app: &mut WordApp, shelf: &Shelf, click: Click, req: &Value) -> Result<Value, String> {
    if !app.has_document() {
        return Err(NO_DOCUMENT.into());
    }
    let title = req.get("title").and_then(Value::as_str).unwrap_or("");
    let wall = match req.get("wall").and_then(Value::as_str) {
        Some(id) => WallId::parse(id).ok_or("`wall`: not an id")?,
        None => {
            let name = req.get("client").and_then(Value::as_str).filter(|c| !c.trim().is_empty()).unwrap_or(MY_CASES);
            let named = |listing: &Listing| listing.walls.iter().find(|w| w.name.for_person() == name).map(|w| w.id.clone());
            // The cached listing may be older than another window's new client, so a name it lacks
            // is looked up on disk once more before a second client of that name is made.
            let found = app.assist.cases.listing.last.as_ref().and_then(named).or_else(|| named(&shelf.list()));
            match found {
                Some(id) => id,
                None => {
                    let (stamp, counter) = stamp();
                    shelf.new_wall(click, name, &stamp, counter).map_err(|e| e.to_string())?
                }
            }
        }
    };
    let (stamp, counter) = stamp();
    let at = shelf.new_case(click, &wall, title, &stamp, counter).map_err(|e| e.to_string())?;
    app.bind_case(Some(at.clone()));
    Ok(json!({"wall": at.wall.as_str(), "case": at.case.as_str()}))
}

/// The typed operation a request asks for. Serde does the parsing: a field a record doesn't know
/// or a value out of its list is an error.
fn person_op(what: &str, req: &Value) -> Result<PersonOp, String> {
    fn parse<T: serde::de::DeserializeOwned>(key: &str, v: &Value) -> Result<T, String> {
        serde_json::from_value(v.clone()).map_err(|e| format!("`{key}`: {e}"))
    }
    let id = || NoteId::parse(req.get("id").and_then(Value::as_str).unwrap_or("")).ok_or_else(|| "`id`: not a note's".to_string());
    let note_draft = || -> Result<NoteDraft, String> {
        let body: Body = parse("note", req.get("note").unwrap_or(&Value::Null))?;
        let how = req.get("how").and_then(Value::as_str).filter(|h| !h.trim().is_empty()).map(Quoted::parse);
        Ok(NoteDraft { body, how, topics: Topics::default() })
    };
    Ok(match what {
        "memory.case.facts" => PersonOp::Facts(parse::<Facts>("facts", req.get("facts").unwrap_or(&Value::Null))?),
        "memory.case.balance" => {
            let stance: Stance = parse("stance", req.get("stance").unwrap_or(&Value::Null))?;
            let factors: Vec<Factor> = parse("factors", req.get("factors").unwrap_or(&json!([])))?;
            PersonOp::Balance(BalanceDraft { stance, factors })
        }
        "memory.case.plan" if req.get("clear") == Some(&Value::Bool(true)) => PersonOp::Plan(None),
        "memory.case.plan" => {
            let goal = Quoted::parse(req.get("goal").and_then(Value::as_str).unwrap_or(""));
            let next: Vec<String> = parse("next", req.get("next").unwrap_or(&json!([])))?;
            // `standing` is the shelf's to set: the person's write confirms it.
            PersonOp::Plan(Some(Plan {
                goal,
                next: next.iter().map(|s| Quoted::parse(s)).collect(),
                standing: wordcraft_assist::cases::Standing::Unconfirmed,
            }))
        }
        "memory.case.note" => PersonOp::AddNote(note_draft()?),
        "memory.case.noteEdit" => PersonOp::EditNote(id()?, note_draft()?),
        "memory.case.noteConfirm" => PersonOp::Confirm(id()?),
        "memory.case.noteRemove" => PersonOp::RemoveNote(id()?),
        other => return Err(format!("`{}`: not a case request", other.chars().take(40).collect::<String>())),
    })
}

/// The case the shown document is bound to.
pub(crate) fn bound_for(app: &WordApp) -> Option<CaseRef> {
    app.assist.cases.bound.get(&app.session.document_id()).cloned()
}

impl WordApp {
    /// Bind the shown document to `at`, or unbind it with `None`. A live conversation of the
    /// document bound to another case ends, stopping its turn: it holds that case's notes, and
    /// the next ask would only replace it. The `assist.memory.case.*` commands come here through
    /// [`write`]; tests bind directly.
    #[doc(hidden)]
    pub fn bind_case(&mut self, at: Option<CaseRef>) {
        self.assist.cases.open = None;
        self.assist.cases.read_at = None;
        let document = self.session.document_id();
        if self.assist.cases.bound.get(&document) != at.as_ref() {
            // The page's forms hold the old case's text.
            self.assist.cases.drafts.close_forms();
        }
        if let Some(c) = self.assist.conversation.as_mut()
            && c.document == document
            && c.case != at
        {
            c.assistant.end();
        }
        match at {
            Some(at) => {
                self.assist.cases.bound.insert(document, at);
            }
            None => {
                self.assist.cases.bound.remove(&document);
            }
        }
    }
}

/// Who can read the shown document besides the person, while its conversation has read case notes
/// and goes on. `None` otherwise. The model still holds what it read, and a document could ask it
/// to write that into a comment the newcomer reads, so stopping recall alone wouldn't do. Cheap:
/// the participant list is in memory, and nothing is looked at until a turn has recalled.
fn others_while_recalled(app: &WordApp) -> Option<String> {
    let c = app.assist.conversation.as_ref().filter(|c| c.assistant.recalled() && !c.assistant.has_ended())?;
    // The conversation's own room, whichever document is shown: someone can join it while the
    // person looks at another one.
    let room = c.assistant.room();
    let me = app.shared_room().filter(|shared| std::sync::Arc::ptr_eq(shared, room)).and_then(|_| app.own_participant());
    let Some(me) = me else {
        // The window no longer shares that room, so it can't say who reads it: fail closed.
        return Some(tl!("someone else").to_string());
    };
    Alone::check(room, me, c.assistant.name()).err().map(|others| others.names.join(", "))
}

/// Why the shown document's conversation can't take a turn: it has read case notes, and someone
/// else can read the document now.
pub(crate) fn refuses(app: &WordApp) -> Option<String> {
    let names = others_while_recalled(app)?;
    Some(format!("This conversation has read your case notes and {names} can now read this document. Start a new conversation."))
}

/// Every frame: someone who can read the document now, while a conversation that read case notes
/// goes on (a turn may be running), ends it, stopping its turn, and the pane says why.
pub(crate) fn guard(app: &mut WordApp) {
    let Some(names) = others_while_recalled(app) else { return };
    if let Some(c) = app.assist.conversation.as_mut() {
        c.assistant.end();
    }
    app.assist.notice = Some(fmt(
        tl!("{names} can now read this document, and this conversation had read your case notes, so it ended. Start a new one."),
        &[("names", &names)],
    ));
}

/// What the next turn asks to recall: nothing without a bound case, a settings folder, a
/// conversation, or the proof that no one else reads the document, and `sent` then says which.
pub(crate) fn for_turn(app: &WordApp, prompt: &str, ctx: &Context, sent: &mut Sent) -> Option<RecallAsk> {
    let Some(at) = bound_for(app) else {
        sent.recall_withheld = Some(NO_CASE.into());
        return None;
    };
    let Some(dir) = app.services.settings_dir.as_ref() else {
        sent.recall_withheld = Some(NO_FOLDER.into());
        return None;
    };
    let (Some(room), Some(me), Some(seat)) = (app.shared_room(), app.own_participant(), app.conversation().map(|a| a.name().to_string())) else {
        sent.recall_withheld = Some("the assistant hasn't joined the document".into());
        return None;
    };
    let alone = match Alone::check(&room, me, &seat) {
        Ok(alone) => alone,
        Err(others) => {
            sent.recall_withheld = Some(format!("{} can read this document", others.names.join(", ")));
            return None;
        }
    };
    let today = Day::parse(wordcraft_engine::cmd::now_iso().get(..10).unwrap_or_default())?;
    // The prompt and the text around the caret, which is what the turn is about. The outline is
    // left out: it names every topic.
    let about = [prompt, &ctx.selection, &ctx.lead, &ctx.trail].join("\n");
    Some(RecallAsk::new(Shelf::at(dir), at, &about, today, alone))
}

/// Details › Recalled: per turn, newest first, what was recalled, from where, under which balance,
/// how many didn't fit, and why nothing when nothing. Interface text is translated; recalled text
/// isn't, since it is the person's or a model's.
pub(crate) fn section(ui: &mut Ui, t: &Tokens, details: &Details) {
    let mut shown = false;
    for (n, turn) in details.turns.iter().enumerate().rev() {
        let withheld = turn.context.as_ref().and_then(|s| s.recall_withheld.as_deref());
        if withheld.is_none() && turn.recalled.is_none() && turn.markers_dropped == 0 {
            continue;
        }
        shown = true;
        if let Some(asked) = &turn.asked {
            ui.add(egui::Label::new(RichText::new(asked).small().color(t.text_dim)).wrap());
        }
        if turn.markers_dropped > 0 {
            let says =
                fmt(tl!("{count} case notes the answer carried couldn't be read and were dropped."), &[("count", &turn.markers_dropped.to_string())]);
            ui.add(egui::Label::new(RichText::new(says).small().color(t.orange)).wrap());
        }
        if let Some(why) = withheld {
            ui.add(egui::Label::new(RichText::new(fmt(tl!("Recall was off: {why}."), &[("why", why)])).small().color(t.orange)).wrap());
        }
        if let Some(recalled) = &turn.recalled {
            ui.push_id(("recalled turn", n), |ui| turn_rows(ui, t, recalled));
        }
        ui.add_space(4.0);
    }
    if !shown {
        ui.label(RichText::new(tl!("Nothing recalled yet.")).small().color(t.text_dim));
    }
}

fn turn_rows(ui: &mut Ui, t: &Tokens, recalled: &RecallSent) {
    for item in &recalled.items {
        let from = match (&item.place, &item.title) {
            (Place::SameClient, Some(title)) => fmt(tl!("{title}, the same client's case"), &[("title", title)]),
            (Place::SameClient, None) => tl!("the same client's case").to_string(),
            (Place::ThisCase, _) => tl!("this case").to_string(),
        };
        ui.scope(|ui| {
            // Rows of text, not of controls: a wrapped row takes its height from this as it starts.
            ui.spacing_mut().interact_size.y = 0.0;
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(item.id.to_string()).small().color(t.accent));
                ui.label(RichText::new(from).small().color(t.text_dim));
                if !item.confirmed {
                    ui.label(RichText::new(tl!("unconfirmed")).small().color(t.orange));
                }
                if item.again {
                    ui.label(RichText::new(tl!("sent earlier")).small().color(t.text_dim));
                }
            });
        });
        ui.add(egui::Label::new(RichText::new(&item.text).small().color(t.text)).wrap());
        if let Some(gap) = &item.balance {
            let line = fmt(tl!("Balance: then {then}, now {now}"), &[("then", stance_word(gap.then)), ("now", stance_word(gap.now))]);
            ui.add(egui::Label::new(RichText::new(line).small().color(t.text_dim)).wrap());
        }
    }
    if recalled.left_out > 0 {
        let says = fmt(tl!("{count} more scored but didn't fit and weren't sent."), &[("count", &recalled.left_out.to_string())]);
        ui.add(egui::Label::new(RichText::new(says).small().color(t.orange)).wrap());
    }
    if let Some(problem) = &recalled.problem {
        ui.add(egui::Label::new(RichText::new(problem).small().color(t.orange)).wrap());
    }
}

/// Whether the conversation's case is the one the page's commands write: the shown document's.
/// Rebinding ends the conversation, so this is false only for an ended one's rows and cards.
fn rows_act(app: &WordApp) -> bool {
    let conversation = app.assist.conversation.as_ref().filter(|c| c.document == app.session.document_id()).and_then(|c| c.case.clone());
    conversation.is_some() && conversation == bound_for(app)
}

/// The quiet row under an answer for what a marker wrote ([`wordcraft_assist::Entry::CaseWritten`]).
/// A note's standing is read from the case as it is now, so the row says when it was confirmed or
/// removed, in Details › Case or here. Confirm, Edit and Remove are the person's commands.
pub(crate) fn written_row(app: &mut WordApp, ui: &mut Ui, t: &Tokens, what: Written, note: Option<NoteId>, text: &str) {
    let live = rows_act(app).then(|| open_case(app, false)).flatten().map(|(_, case)| case);
    let lead = match what {
        Written::Note => tl!("Noted for this case:"),
        Written::Plan => tl!("Plan for this case, unconfirmed:"),
        Written::Balance => tl!("Balance for this case, unconfirmed:"),
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(RichText::new(lead).small().color(t.text_dim));
        ui.label(RichText::new(text).small().color(t.text));
        let (Written::Note, Some(id)) = (what, note) else { return };
        let Some(case) = &live else { return };
        match case.notes.iter().find(|n| n.id == id) {
            None => {
                ui.label(RichText::new(tl!("Removed")).small().color(t.text_dim));
            }
            Some(n) if matches!(n.standing, Standing::Confirmed { .. }) => {
                ui.label(RichText::new(tl!("Confirmed")).small().color(t.text_dim));
            }
            Some(n) => {
                let n = n.clone();
                ui.label(RichText::new(tl!("unconfirmed")).small().color(t.orange));
                if ui.small_button(tl!("Confirm")).on_hover_text(tl!("This client's other cases can use it.")).clicked() {
                    run(app, "assist.memory.case.noteConfirm", json!({"id": id.to_string()}));
                }
                if ui.small_button(tl!("Edit")).clicked() {
                    page::edit_note(app, &n);
                }
                if ui.small_button(tl!("Remove")).clicked() {
                    run(app, "assist.memory.case.noteRemove", json!({"id": id.to_string()}));
                }
            }
        }
    });
}

/// A card under an answer ([`wordcraft_assist::Entry::CaseCard`]): what the model proposes for the
/// case and, while it waits, the buttons. Nothing is stored until Use or Keep, which are the
/// person's commands; Not this stores nothing either.
pub(crate) fn card(app: &mut WordApp, ui: &mut Ui, t: &Tokens, id: u64, card: &Card, answer: Option<ProposalAnswer>) {
    let waiting = answer.is_none();
    let stroke = if waiting { egui::Stroke::new(1.5, t.accent) } else { egui::Stroke::new(1.0, t.border_strong) };
    egui::Frame::NONE.stroke(stroke).corner_radius(6).inner_margin(6).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let (question, lines, keep) = card_words(card);
        let said = match answer {
            None => question,
            Some(ProposalAnswer::Kept) => tl!("Used for this case"),
            Some(ProposalAnswer::Dismissed) => tl!("Not used"),
        };
        ui.label(RichText::new(said).small().color(t.text_dim));
        for line in &lines {
            ui.add(egui::Label::new(RichText::new(line).font(crate::theme::semibold(12.0)).color(if waiting { t.text } else { t.text_dim })).wrap());
        }
        if !waiting || !rows_act(app) {
            return;
        }
        ui.horizontal_wrapped(|ui| {
            if ui.button(keep).clicked()
                && use_card(app, card)
                && let Some(a) = app.conversation_mut()
            {
                a.answer_case_card(id, ProposalAnswer::Kept);
            }
            if ui.button(tl!("Edit")).clicked() {
                match card {
                    Card::Facts(_) => app.assist.view = super::PaneView::Details,
                    Card::Write(ModelWrite::Note(draft)) => page::draft_note(app, draft),
                    Card::Write(ModelWrite::Plan(plan)) => page::draft_plan(app, plan),
                    Card::Write(ModelWrite::Balance(draft)) => page::draft_balance(app, draft),
                    Card::Write(ModelWrite::Used(_)) => {}
                }
            }
            if ui.button(tl!("Not this")).clicked()
                && let Some(a) = app.conversation_mut()
            {
                a.answer_case_card(id, ProposalAnswer::Dismissed);
            }
        });
    });
}

/// A card's question, the lines it shows, and what its first button says.
fn card_words(card: &Card) -> (&'static str, Vec<String>, &'static str) {
    match card {
        Card::Facts(facts) => {
            let mut lines = Vec::new();
            let mut say = |name: &str, v: Option<String>| {
                if let Some(v) = v {
                    lines.push(format!("{name}: {v}"));
                }
            };
            say(tl!("Contract"), facts.kind.map(word));
            say(tl!("Our side"), facts.side.map(word));
            say(tl!("Paper"), facts.paper.map(word));
            say(tl!("Counterparty"), facts.theirs.map(word));
            say(tl!("Law"), facts.law.map(String::from));
            say(tl!("Size"), facts.size.map(word));
            say(tl!("Language"), facts.language.map(word));
            (tl!("The assistant suggests these facts for this case."), lines, tl!("Use these"))
        }
        Card::Write(ModelWrite::Note(draft)) => {
            let mut lines: Vec<String> = draft.body.texts().map(|q| q.for_person().to_string()).collect();
            if let Some(how) = &draft.how {
                lines.push(how.for_person().to_string());
            }
            (tl!("Keep this note for this case?"), lines, tl!("Keep"))
        }
        Card::Write(ModelWrite::Plan(plan)) => {
            let mut lines = vec![plan.goal.for_person().to_string()];
            lines.extend(plan.next.iter().map(|s| s.for_person().to_string()));
            (tl!("The assistant suggests this plan. Yours stays until you use it."), lines, tl!("Use this plan"))
        }
        Card::Write(ModelWrite::Balance(draft)) => {
            let mut lines = vec![stance_word(Some(draft.stance)).to_string()];
            lines.extend(draft.factors.iter().filter_map(|f| f.why.as_ref()).map(|w| w.for_person().to_string()));
            (tl!("The assistant reads the balance as this. Reassess?"), lines, tl!("Reassess"))
        }
        Card::Write(ModelWrite::Used(_)) => ("", Vec::new(), ""),
    }
}

/// A typed value's name, as its JSON reads.
fn word<T: serde::Serialize>(v: T) -> String {
    serde_json::to_value(v).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

/// The person's Use or Keep on a card: the command it runs, on the bound case. Facts proposed fill
/// only what they name; the rest, names and stage included, stays as it is.
fn use_card(app: &mut WordApp, card: &Card) -> bool {
    match card {
        Card::Facts(draft) => {
            let Some((_, case)) = open_case(app, true) else { return false };
            let mut facts = case.facts;
            facts.kind = draft.kind.or(facts.kind);
            facts.side = draft.side.or(facts.side);
            facts.paper = draft.paper.or(facts.paper);
            facts.theirs = draft.theirs.or(facts.theirs);
            facts.law = draft.law.or(facts.law);
            facts.size = draft.size.or(facts.size);
            facts.language = draft.language.or(facts.language);
            run(app, "assist.memory.case.facts", json!({"facts": facts}))
        }
        Card::Write(ModelWrite::Note(draft)) => run(app, "assist.memory.case.note", json!({"note": draft.body, "how": draft.how})),
        Card::Write(ModelWrite::Plan(plan)) => run(app, "assist.memory.case.plan", json!({"goal": plan.goal, "next": plan.next})),
        Card::Write(ModelWrite::Balance(draft)) => run(app, "assist.memory.case.balance", json!({"stance": draft.stance, "factors": draft.factors})),
        Card::Write(ModelWrite::Used(_)) => false,
    }
}

fn run(app: &mut WordApp, id: &str, params: Value) -> bool {
    match app.run(id, params) {
        Ok(_) => true,
        Err(e) => {
            app.assist.notice = Some(e);
            false
        }
    }
}

fn stance_word(stance: Option<Stance>) -> &'static str {
    match stance {
        Some(Stance::Stronger) => tl!("stronger"),
        Some(Stance::Even) => tl!("even"),
        Some(Stance::Weaker) => tl!("weaker"),
        None => tl!("unknown"),
    }
}

#[cfg(test)]
mod tests;

//! What the window does with the person's cases (#107): which case each document is bound to, what
//! a turn asks to recall from it, what Details › Recalled shows of what came back, and the
//! `memory.case.*` and `memory.wall.*` writes behind Details › Case ([`page`]).
//!
//! The binding is the window's alone and never in a document. A conversation belongs to the case
//! its document was bound to when it started ([`super::Conversation::case`]); a document bound to
//! another case gets a new conversation. Recall runs only when a case is bound and no one but the
//! person and this conversation's assistant can read the document ([`Alone`]); otherwise the turn
//! runs as it does today, and Details says why. Only the person's own click writes a case:
//! [`super::memory::write`] is the one guard, and the shelf's [`Click`] is its proof.

use std::collections::BTreeMap;

use egui::{RichText, Ui};
use serde_json::{Value, json};
use wordcraft_assist::cases::{
    Alone, BalanceDraft, Body, Case, CaseError, CaseId, CaseRef, Click, Day, Factor, Facts, Listing, NoteDraft, NoteId, PersonOp, Place, Plan,
    Quoted, RecallAsk, RecallSent, Shelf, Stance, Topics, WallId,
};
use wordcraft_assist::{Context, Details, Sent};

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
            let listing = match &app.assist.cases.listing.last {
                Some(listing) => listing.clone(),
                None => shelf.list(),
            };
            match listing.walls.iter().find(|w| w.name.for_person() == name).map(|w| w.id.clone()) {
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
        if withheld.is_none() && turn.recalled.is_none() {
            continue;
        }
        shown = true;
        if let Some(asked) = &turn.asked {
            ui.add(egui::Label::new(RichText::new(asked).small().color(t.text_dim)).wrap());
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

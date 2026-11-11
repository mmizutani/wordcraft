//! The case's record in the window (#107): a conversation on a case whose record is on owns a
//! [`CaseLog`] for its life, and every line goes to that case, as a marker is routed, never to
//! the document's current binding. What is written: each turn as it ended, the person's Accept
//! or Reject of anyone's change, the person's edit of the assistant's text, and a snapshot of the
//! other side's redlines when the conversation takes the draft up. Nothing in a turn reads it.
//!
//! Details › Case › Record holds the switch (off for every case until the person ticks it), the
//! record's size, a search box whose search runs on a worker, and **Delete the record**. Deleting
//! the record or the case runs on a worker too: the conversation's log closes its file there,
//! before the folder is renamed, so a frame never waits for the disk.

use std::sync::mpsc::Receiver;

use egui::{RichText, Ui};
use serde_json::{Value, json};
use wordcraft_assist::Decision;
use wordcraft_assist::cases::{Case, CaseError, CaseLog, CaseRef, Click, CommentLine, LogSearch, PersonOp, Quoted, Record, RedlineLine, Shelf};
use wordcraft_engine::cmd::review::{AuthoredChange, authors, changes_of, comments_except, redlines};

use crate::WordApp;
use crate::i18n::fmt;
use crate::options::note;
use crate::theme::Tokens;

/// Most hits a search shows.
const MAX_HITS: usize = 50;

/// Most of the other side's changes and comments a snapshot keeps.
const MAX_REDLINES: usize = 256;
const MAX_COMMENTS: usize = 64;

/// Most characters of a hit's text shown.
const SHOWN: usize = 160;

/// What the page holds of the record, and the workers it waits on.
#[derive(Default)]
pub(crate) struct RecordState {
    /// The record's size as last read, with when (`clock::now_ms`).
    size: Option<(f64, u64)>,
    pub(crate) query: String,
    /// A search running on a worker, and whose record it searches.
    search: Option<(CaseRef, Receiver<Result<LogSearch, CaseError>>)>,
    /// What the last search found, and whose record: shown on that case's page alone.
    found: Option<(CaseRef, Result<LogSearch, CaseError>)>,
    /// A delete running on a worker, and what to do when it ends.
    pub(crate) deleting: Option<(Receiver<Result<(), CaseError>>, After)>,
}

impl RecordState {
    /// What the last search found, when it searched `at`'s record.
    pub(crate) fn results(&self, at: &CaseRef) -> Option<&Result<LogSearch, CaseError>> {
        self.found.as_ref().filter(|(of, _)| of == at).map(|(_, found)| found)
    }
}

/// What follows a delete that ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum After {
    /// The live conversation on this case opens a new file, if its record is on: whether the
    /// delete succeeded or not, since its file was closed for it either way.
    ReopenLog(CaseRef),
    Nothing,
}

/// The conversation's changes, and anyone's, as the person's Accept or Reject starts.
pub(crate) struct Reviewing {
    /// `None` when there are more than the window can list: unknown, not none.
    pub(crate) mine: Option<Vec<AuthoredChange>>,
    /// By author, only while the conversation keeps a record: the walk is per author. An author
    /// whose changes are unknown isn't here.
    pub(crate) anyone: Vec<(String, Vec<AuthoredChange>)>,
}

fn shelf(app: &WordApp) -> Option<Shelf> {
    app.services.settings_dir.as_ref().map(|dir| Shelf::at(dir))
}

/// The conversation just started, or its case's record was just turned on: open its file when
/// the case keeps a record, and put the other side's redlines in it first.
pub(crate) fn opened(app: &mut WordApp) {
    let Some(shelf) = shelf(app) else { return };
    let Some(c) = app.assist.conversation.as_mut() else { return };
    let Some(at) = c.case.clone() else { return };
    if c.log.is_some() || !shelf.open(&at).is_ok_and(|case| case.record) {
        return;
    }
    let (stamp, counter) = super::stamp();
    match CaseLog::open(&shelf, &at, &format!("{stamp}-{counter}")) {
        Ok(log) => c.log = Some(log),
        Err(e) => {
            log::warn!("the case's record can't be opened: {e}");
            return;
        }
    }
    // The window may show another document by now (a delete ended after the person moved on):
    // its redlines are another case's, maybe another client's, and go in no snapshot.
    if !shows_own_document(app) {
        return;
    }
    let snapshot = snapshot(app);
    if let Some(log) = app.assist.conversation.as_mut().and_then(|c| c.log.as_mut()) {
        log.write(snapshot);
    }
}

/// Whether the document shown is the conversation's: only then does the session say anything
/// about it.
fn shows_own_document(app: &WordApp) -> bool {
    app.assist.conversation.as_ref().is_some_and(|c| c.document == app.session.document_id())
}

/// The conversation's log for one more line, if its case still keeps a record. Another window or
/// process may have turned the record off since the log opened, and nothing here hears that: the
/// case is read again before each write, which come at the person's or a turn's pace. Off, the
/// log is let go, and the writer finishes what it holds.
fn live_log(app: &mut WordApp) -> Option<&mut CaseLog> {
    let shelf = shelf(app)?;
    let c = app.assist.conversation.as_mut()?;
    c.log.as_ref()?;
    let on = c.case.as_ref().is_some_and(|at| shelf.open(at).is_ok_and(|case| case.record));
    if !on {
        c.log = None;
    }
    c.log.as_mut()
}

/// The other side's tracked changes and comments as the document holds them now: everyone's but
/// the person's and this conversation's assistant's.
fn snapshot(app: &WordApp) -> Record {
    let me = app.session.author.clone();
    let seat = app.conversation().map(|a| a.name().to_string()).unwrap_or_default();
    let except = [me.as_str(), seat.as_str()];
    let (changes, more_changes) = redlines(&app.session, &except, MAX_REDLINES);
    let (comments, more_comments) = comments_except(&app.session, &except, MAX_COMMENTS);
    Record::Redlines {
        at: wordcraft_engine::cmd::now_iso(),
        draft: Quoted::parse(&app.title_stem()),
        changes: changes
            .into_iter()
            .map(|c| RedlineLine {
                kind: c.kind.to_string(),
                author: Quoted::parse(&c.author),
                date: c.date,
                text: Quoted::parse(&c.text),
                at: Some(wordcraft_engine::cmd::pos_json(&c.at)),
            })
            .collect(),
        comments: comments
            .into_iter()
            .map(|c| CommentLine {
                author: Quoted::parse(&c.author),
                date: c.date,
                text: Quoted::parse(&c.text),
                at: c.at.as_ref().map(wordcraft_engine::cmd::pos_json),
                resolved: c.resolved,
            })
            .collect(),
        more: u32::try_from(more_changes.saturating_add(more_comments)).unwrap_or(u32::MAX),
    }
}

/// The turn that just ended, if any, goes to the conversation's record. Taken whether or not
/// there is a log, so a turn never waits for the next one.
fn write_finished_turn(app: &mut WordApp) {
    let finished = app.assist.conversation.as_mut().and_then(|c| c.assistant.take_finished_turn());
    if let Some(record) = finished
        && let Some(log) = live_log(app)
    {
        log.write(record);
    }
}

/// End the conversation there is, if any, and write the turn it cut off, as cancelled, to its
/// record. Every place a conversation ends or is replaced comes here, while the conversation and
/// its log are still held: dropped first, the cut turn would be lost.
pub(crate) fn end(app: &mut WordApp) {
    if let Some(c) = app.assist.conversation.as_mut() {
        c.assistant.end();
    }
    write_finished_turn(app);
}

/// Every frame: the turn that just ended goes to the conversation's record, and a worker that
/// ended is heard.
pub(crate) fn tick(app: &mut WordApp) {
    write_finished_turn(app);
    let state = &mut app.assist.cases.record;
    if let Some((of, rx)) = &state.search
        && let Ok(found) = rx.try_recv()
    {
        state.found = Some((of.clone(), found));
        state.search = None;
    }
    let done = state.deleting.as_ref().and_then(|(rx, _)| rx.try_recv().ok());
    if let Some(r) = done {
        let after = state.deleting.take().map_or(After::Nothing, |(_, after)| after);
        state.size = None;
        super::changed(app);
        if let Err(e) = r {
            app.assist.notice = Some(e.to_string());
        }
        if let After::ReopenLog(at) = after
            && app.assist.conversation.as_ref().is_some_and(|c| c.case.as_ref() == Some(&at))
        {
            opened(app);
        }
    }
}

/// The changes to compare around the person's Accept or Reject: the conversation's, and anyone's
/// when the conversation keeps a record.
pub(crate) fn reviewing(app: &WordApp) -> Option<Reviewing> {
    let c = app.assist.conversation.as_ref().filter(|c| c.document == app.session.document_id())?;
    let mine = changes_of(&app.session, c.assistant.name());
    let anyone = if c.log.is_some() { by_author(app).into_iter().filter_map(|(a, changes)| Some((a, changes?))).collect() } else { Vec::new() };
    Some(Reviewing { mine, anyone })
}

/// Each author's changes, `None` where there are more than the window can list.
fn by_author(app: &WordApp) -> Vec<(String, Option<Vec<AuthoredChange>>)> {
    authors(&app.session)
        .into_iter()
        .map(|a| {
            let changes = changes_of(&app.session, &a);
            (a, changes)
        })
        .collect()
}

/// The person's Accept or Reject ran: anyone's changes `before` that are gone now are lines of the
/// conversation's record, each naming its author. An author whose changes can't be listed now is
/// skipped: unknown isn't none, and a decision is never invented.
pub(crate) fn decided(app: &mut WordApp, decision: Decision, before: Vec<(String, Vec<AuthoredChange>)>) {
    if before.is_empty() || !shows_own_document(app) {
        return;
    }
    let after = by_author(app);
    let at = wordcraft_engine::cmd::now_iso();
    let Some(log) = live_log(app) else { return };
    for (author, changes) in before {
        let now = match after.iter().find(|(a, _)| *a == author) {
            // No revision of theirs is left: every change went.
            None => Vec::new(),
            Some((_, None)) => continue,
            Some((_, Some(now))) => now.clone(),
        };
        for change in super::super::resolved(changes, now) {
            log.write(Record::Decided {
                at: at.clone(),
                decision,
                kind: change.kind.to_string(),
                author: Quoted::parse(&author),
                text: Quoted::parse(&change.text),
            });
        }
    }
}

/// The person changed the assistant's text: a line of the conversation's record.
pub(crate) fn edited(app: &mut WordApp, was: &str, now: &str) {
    let at = wordcraft_engine::cmd::now_iso();
    if let Some(log) = live_log(app) {
        log.write(Record::Edited { at, was: Quoted::parse(was), now: Quoted::parse(now) });
    }
}

/// `memory.case.record` and `memory.case.recordDelete`, on the bound case, with the person's click.
pub(crate) fn write(app: &mut WordApp, shelf: &Shelf, click: Click, what: &str, req: &Value) -> Result<Value, String> {
    let at = super::bound_for(app).ok_or(super::NOT_BOUND)?;
    match what {
        "memory.case.record" => {
            let on = req.get("value").and_then(Value::as_bool).ok_or("`value`: true or false")?;
            shelf.person(click, &at, PersonOp::Record(on), super::today()?).map_err(|e| e.to_string())?;
            let on_this = app.assist.conversation.as_ref().is_some_and(|c| c.case.as_ref() == Some(&at));
            if on_this && on {
                opened(app);
            } else if on_this && let Some(c) = app.assist.conversation.as_mut() {
                // Dropped, not closed: the writer finishes what is queued on its own.
                c.log = None;
            }
            Ok(json!({"record": on}))
        }
        _ => {
            if app.assist.cases.record.deleting.is_some() {
                return Err(tl!("A delete is still running: wait for it.").to_string());
            }
            let log = app.assist.conversation.as_mut().filter(|c| c.case.as_ref() == Some(&at)).and_then(|c| c.log.take());
            let shelf = shelf.clone();
            let for_worker = at.clone();
            spawn_delete(app, log, After::ReopenLog(at.clone()), move || wordcraft_assist::cases::delete_log(&shelf, &for_worker))?;
            app.assist.cases.record.found = None;
            Ok(json!({"deleting": at.case.as_str()}))
        }
    }
}

/// Delete the case on a worker: the conversation's log closes its file there first, then the
/// folder goes. A refusal (another window's lock, a file in use) comes back as a notice and
/// loses nothing.
pub(crate) fn delete_case(
    app: &mut WordApp,
    shelf: &Shelf,
    click: Click,
    at: &wordcraft_assist::cases::CaseRef,
    keep_lessons: bool,
) -> Result<Value, String> {
    if app.assist.cases.record.deleting.is_some() {
        return Err(tl!("A delete is still running: wait for it.").to_string());
    }
    // The conversation held the case's notes: it ends first, with its transcript (a cut turn
    // goes to its log), then its log goes to the worker to be closed.
    app.bind_case(None);
    super::super::end(app);
    let log = app.assist.conversation.as_mut().filter(|c| c.case.as_ref() == Some(at)).and_then(|c| c.log.take());
    let shelf = shelf.clone();
    let for_worker = at.clone();
    spawn_delete(app, log, After::Nothing, move || shelf.delete_case(click, &for_worker, keep_lessons))?;
    Ok(json!({"deleting": at.case.as_str()}))
}

fn spawn_delete(
    app: &mut WordApp,
    log: Option<CaseLog>,
    after: After,
    delete: impl FnOnce() -> Result<(), CaseError> + Send + 'static,
) -> Result<(), String> {
    let ctx = app.ctx.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("case delete".into())
        .spawn(move || {
            if let Some(log) = log {
                log.close();
            }
            let _ = tx.send(delete());
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        })
        .map_err(|e| format!("the delete can't start: {e}"))?;
    app.assist.cases.record.deleting = Some((rx, after));
    Ok(())
}

fn search_on_worker(app: &mut WordApp, at: &CaseRef) {
    let Some(shelf) = shelf(app) else { return };
    let ctx = app.ctx.clone();
    let query = app.assist.cases.record.query.clone();
    let of = at.clone();
    let at = at.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("case record search".into()).spawn(move || {
        let _ = tx.send(wordcraft_assist::cases::search_log(&shelf, &at, &query, MAX_HITS));
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    });
    if spawned.is_ok() {
        app.assist.cases.record.search = Some((of, rx));
    }
}

/// Details › Case › Record.
pub(crate) fn section(app: &mut WordApp, ui: &mut Ui, t: &Tokens, at: &CaseRef, case: &Case) {
    let mut on = case.record;
    if ui.checkbox(&mut on, tl!("Keep a record of this negotiation")).changed() {
        super::run(app, "assist.memory.case.record", json!({"value": on}));
    }
    note(
        ui,
        "Off for every case until you turn it on. On, each conversation on this case is a file in the case's own folder, log/ in WordCraft's settings folder, that only you can read: every turn as it ended, your Accept and Reject of anyone's tracked change, your edits of what the assistant wrote, and the other side's tracked changes and comments as the conversation found them. No turn reads it. Delete the record removes it; deleting the case does too.",
    );
    let stale = app.assist.cases.record.size.is_none_or(|(t, _)| crate::clock::now_ms() - t > super::READ_EVERY);
    if stale && let Some(shelf) = shelf(app) {
        app.assist.cases.record.size = Some((crate::clock::now_ms(), wordcraft_assist::cases::log_size(&shelf, at)));
    }
    let size = app.assist.cases.record.size.map_or(0, |(_, n)| n);
    let deleting = app.assist.cases.record.deleting.is_some();
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(fmt(tl!("Size: {size}"), &[("size", &size_words(size))])).small().color(t.text_dim));
        if size > 0 && !deleting && ui.small_button(tl!("Delete the record")).clicked() {
            super::run(app, "assist.memory.case.recordDelete", json!({}));
        }
        if deleting {
            ui.label(RichText::new(tl!("Deleting…")).small().color(t.text_dim));
        }
    });
    if size == 0 {
        return;
    }
    ui.horizontal(|ui| {
        let edit = egui::TextEdit::singleline(&mut app.assist.cases.record.query).hint_text(tl!("Search the record")).desired_width(180.0);
        let r = ui.add(edit);
        let go = ui.small_button(tl!("Search")).clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
        if go && app.assist.cases.record.search.is_none() {
            search_on_worker(app, at);
        }
        if app.assist.cases.record.search.as_ref().is_some_and(|(of, _)| of == at) {
            ui.label(RichText::new(tl!("Searching…")).small().color(t.text_dim));
        }
    });
    match app.assist.cases.record.results(at) {
        None => {}
        Some(Err(e)) => {
            ui.label(RichText::new(e.to_string()).small().color(t.orange));
        }
        Some(Ok(found)) => {
            if found.hits.is_empty() {
                ui.label(RichText::new(tl!("Nothing found.")).small().color(t.text_dim));
            }
            for hit in &found.hits {
                ui.add(egui::Label::new(RichText::new(hit_words(&hit.record)).small().color(t.text)).wrap());
            }
            if found.skipped > 0 {
                let says = fmt(tl!("{count} lines of the record couldn't be read and were skipped."), &[("count", &found.skipped.to_string())]);
                ui.add(egui::Label::new(RichText::new(says).small().color(t.orange)).wrap());
            }
        }
    }
}

fn size_words(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1 << 20 {
        format!("{} KiB", bytes >> 10)
    } else {
        format!("{} MiB", bytes >> 20)
    }
}

fn short(q: &Quoted) -> String {
    let line: String = q.for_person().chars().take(SHOWN).collect();
    line.replace(['\n', '\r'], " ")
}

/// One hit as a line: interface words translated, the record's own text as it is.
fn hit_words(record: &Record) -> String {
    match record {
        Record::Turn { at, asked, answer, .. } => {
            let answer = answer.as_ref().map(short).unwrap_or_default();
            format!("{at} · {}: {} · {}: {answer}", tl!("Asked"), short(asked), tl!("Answer"))
        }
        Record::Decided { at, decision, kind, author, text } => {
            let what = match decision {
                Decision::Accepted => tl!("Accepted"),
                Decision::Rejected => tl!("Rejected"),
            };
            format!("{at} · {what} · {kind} · {}: {}", short(author), short(text))
        }
        Record::Edited { at, was, now } => format!("{at} · {}: {} → {}", tl!("Edited"), short(was), short(now)),
        Record::Redlines { at, draft, changes, comments, .. } => fmt(
            tl!("{at} · Redlines of {draft}: {changes} changes, {comments} comments"),
            &[("at", at), ("draft", &short(draft)), ("changes", &changes.len().to_string()), ("comments", &comments.len().to_string())],
        ),
        Record::Dropped { n } => fmt(tl!("{count} records didn't fit and were dropped."), &[("count", &n.to_string())]),
    }
}

#[cfg(test)]
mod tests;

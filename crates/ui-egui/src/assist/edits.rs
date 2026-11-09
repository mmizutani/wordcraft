//! What the person changes of the text the assistant wrote, for the next turn's digest (#86): the
//! clearest evidence of how they want something written, after they accepted it or inside its
//! tracked insertion.
//!
//! The hub keeps its latest document alone, so what a tool call's text said has to be read when its
//! turn ends. The digest then quotes both what it said and what it says now as the assistant's and
//! the person's, so a text is quoted only if no one else could have put words in it: when it reads
//! differently and anyone but the person changed the document since it was last read, it says
//! nothing and stops being watched. Feedback to a model had better miss an edit than invent one,
//! and a participant's text must not reach it as the person's correction. This errs quiet in a
//! room with others typing, which only costs a hint.

use std::collections::VecDeque;

use wordcraft_engine::room::Role;

use crate::WordApp;

/// Most texts watched: the newest stay.
const MAX_WATCHED: usize = 32;

/// Most bytes of a text compared. An edit past this isn't seen, which errs towards saying nothing.
const MAX_TEXT: usize = 2 << 10;

/// One tool call's text, as it last read.
struct Written {
    first: u64,
    last: u64,
    text: String,
    /// The shared document's version when it read this.
    seen: u64,
}

#[derive(Default)]
pub(super) struct Edits {
    watched: VecDeque<Written>,
    /// A turn has started whose tool calls haven't been read yet.
    running: bool,
    /// Since the last look, a command of a script's or an agent's changed the document in the
    /// window. The room can't tell it from the person's own: both go through the person's copy, so
    /// nothing is said.
    script_changed: bool,
    /// The document's revision as the command running now started.
    rev_before: u64,
}

impl Edits {
    pub(super) fn turn_started(&mut self) {
        self.running = true;
    }
}

/// Every frame: when a turn has ended, read what its tool calls wrote.
pub(super) fn tick(app: &mut WordApp) {
    if !app.assist.edits.running || app.conversation().is_none_or(|a| a.is_busy()) {
        return;
    }
    app.assist.edits.running = false;
    record(app);
    let Some(a) = app.conversation() else { return };
    let (room, name) = (a.room().clone(), a.name().to_string());
    let calls: Vec<_> = a
        .transcript()
        .entries()
        .rev()
        .take_while(|e| !matches!(e, wordcraft_assist::Entry::Asked { .. }))
        .filter_map(|e| if let wordcraft_assist::Entry::Tool(call) = e { call.changes } else { None })
        .take(MAX_WATCHED)
        .collect();
    let seen = room.version();
    let edits = &mut app.assist.edits;
    for call in calls.into_iter().rev() {
        // Its text is what the assistant wrote only if no one else has changed anything since the
        // call began: others' changes may be among its versions.
        if !room.changed_since(call.first.saturating_sub(1)).is_some_and(|names| names.iter().all(|who| *who == name)) {
            continue;
        }
        let Some(text) = room.text_where_changed(call.first..=call.last, Some(&name), MAX_TEXT) else { continue };
        edits.watched.push_back(Written { first: call.first, last: call.last, text, seen });
        if edits.watched.len() > MAX_WATCHED {
            edits.watched.pop_front();
        }
    }
}

/// Before a prompt, or an Accept or Reject: what the person changed of each text goes in the digest.
pub(super) fn record(app: &mut WordApp) {
    read_again(app, true);
    app.assist.edits.script_changed = false;
}

/// Accept or Reject ran: what it did to the text isn't an edit.
pub(super) fn review_ended(app: &mut WordApp) {
    read_again(app, false);
}

/// A command of the window's is about to run.
pub(super) fn command_starts(app: &mut WordApp) {
    app.assist.edits.rev_before = app.session.rev();
}

/// Command `id` ended. One that changed the document counts as a script's unless the person ran it
/// themselves: a macro the person ran counts too, since anyone may have written it.
pub(super) fn command_ended(app: &mut WordApp, id: &str) {
    if (!app.assist.by_person || id == "tools.macros") && app.session.rev() != app.assist.edits.rev_before {
        app.assist.edits.script_changed = true;
    }
}

/// Read each watched text as it is now, and with `say`, put what the person changed in the digest.
/// A text the document no longer has, or that someone else may have changed, stops being watched:
/// the assistant's own later changes to it too, whose new call is watched instead.
fn read_again(app: &mut WordApp, say: bool) {
    // A script's change may be anywhere, past where a text is compared too.
    if app.assist.edits.script_changed {
        app.assist.edits.watched.clear();
    }
    if app.assist.edits.watched.is_empty() {
        return;
    }
    app.catch_up_share();
    let Some(a) = app.conversation() else { return };
    let (room, name) = (a.room().clone(), a.name().to_string());
    let Some(person) = room.participants().into_iter().find(|p| p.role == Role::Owner).map(|p| p.name) else { return };
    let now = room.version();
    let watched = std::mem::take(&mut app.assist.edits.watched);
    for mut w in watched {
        let Some(text) = room.text_where_changed(w.first..=w.last, Some(&name), MAX_TEXT) else { continue };
        // The hub no longer keeping those changes means it can't be said whose they were.
        let names = room.changed_since(w.seen);
        if text != w.text {
            if !names.is_some_and(|names| names.iter().all(|who| *who == person)) {
                continue;
            }
            if say {
                app.assist.feedback.record_edit(&w.text, &text);
            }
        } else if !names.is_some_and(|names| names.iter().all(|who| *who == person || *who == name)) {
            // It reads the same as far as it is compared, but someone else changed the document,
            // maybe past that: `seen` stays before their change, so the text's next difference
            // counts it.
            app.assist.edits.watched.push_back(w);
            continue;
        }
        w.text = text;
        w.seen = now;
        app.assist.edits.watched.push_back(w);
    }
}

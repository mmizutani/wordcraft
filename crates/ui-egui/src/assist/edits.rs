//! What the person changes of the text the assistant wrote, for the next turn's digest (#86): the
//! clearest evidence of how they want something written, after they accepted it or inside its
//! tracked insertion.
//!
//! The hub keeps its latest document alone, so what a tool call's text said has to be read when its
//! turn ends. At the next prompt, a text that reads differently now and that the person's own
//! changes touched, and no one else's, is an edit: someone else's edit of it doesn't count, nor
//! does the person typing elsewhere.

use std::collections::VecDeque;

use wordcraft_engine::room::{Role, Room};

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
    /// The person's own command changed the document since the last look: the room can't tell it
    /// from a script's, which goes through the same copy.
    person_changed: bool,
}

impl Edits {
    pub(super) fn turn_started(&mut self) {
        self.running = true;
    }

    /// The person's own command changed the document, not a script's or an agent's in the window.
    pub(super) fn person_changed_it(&mut self) {
        self.person_changed = true;
    }
}

/// Every frame: when a turn has ended, read what its tool calls wrote.
pub(super) fn tick(app: &mut WordApp) {
    if !app.assist.edits.running || app.conversation().is_none_or(|a| a.is_busy()) {
        return;
    }
    app.assist.edits.running = false;
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
        let Some(text) = room.text_where_changed(call.first..=call.last, Some(&name), MAX_TEXT) else { continue };
        edits.watched.push_back(Written { first: call.first, last: call.last, text, seen });
        if edits.watched.len() > MAX_WATCHED {
            edits.watched.pop_front();
        }
    }
}

/// Whether the person's changes since `w` was read touched it and no one else's did, the
/// assistant's included. The digest quotes what the text now says as the person's, so with anyone
/// else's words possibly in it, it says nothing: a participant's text must not reach the model as
/// the person's correction. Better to miss an edit than to invent one.
fn only_the_person_touched(room: &Room, w: &Written, person: &str, name: &str) -> bool {
    let meets = |by: &str| room.changes_by(w.seen, by).is_some_and(|(f, l)| room.changes_meet(f..=l, by, w.first..=w.last, name));
    room.changed_since(w.seen).is_some_and(|names| meets(person) && names.iter().all(|other| other == person || !meets(other)))
}

/// Before a prompt, or an Accept or Reject: what the person changed of each text goes in the digest.
/// A text the document no longer has stops being watched.
pub(super) fn record(app: &mut WordApp) {
    if app.assist.edits.watched.is_empty() {
        return;
    }
    app.catch_up_share();
    read_again(app, true);
    app.assist.edits.person_changed = false;
}

/// Accept or Reject ran: what it did to the text isn't an edit, so the text is read again as it is.
pub(super) fn review_ended(app: &mut WordApp) {
    if !app.assist.edits.watched.is_empty() {
        app.catch_up_share();
        read_again(app, false);
    }
}

fn read_again(app: &mut WordApp, say: bool) {
    let Some(a) = app.conversation() else { return };
    let (room, name) = (a.room().clone(), a.name().to_string());
    let Some(person) = room.participants().into_iter().find(|p| p.role == Role::Owner).map(|p| p.name) else { return };
    let say = say && app.assist.edits.person_changed;
    let now = room.version();
    let watched = std::mem::take(&mut app.assist.edits.watched);
    for mut w in watched {
        let Some(text) = room.text_where_changed(w.first..=w.last, Some(&name), MAX_TEXT) else { continue };
        if say && text != w.text && only_the_person_touched(&room, &w, &person, &name) {
            app.assist.feedback.record_edit(&w.text, &text);
        }
        w.text = text;
        w.seen = now;
        app.assist.edits.watched.push_back(w);
    }
}

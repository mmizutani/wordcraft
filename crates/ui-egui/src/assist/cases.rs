//! What the window does with the person's cases (#107): which case each document is bound to, what
//! a turn asks to recall from it, and what Details › Recalled shows of what came back.
//!
//! The binding is the window's alone and never in a document. A conversation belongs to the case
//! its document was bound to when it started ([`super::Conversation::case`]); a document bound to
//! another case gets a new conversation. Recall runs only when a case is bound and no one but the
//! person and this conversation's assistant can read the document ([`Alone`]); otherwise the turn
//! runs as it does today, and Details says why. The commands that bind a case come later; until
//! then only a test binds one ([`WordApp::bind_case`]).

use std::collections::BTreeMap;

use egui::{RichText, Ui};
use wordcraft_assist::cases::{Alone, CaseRef, Day, Place, RecallAsk, RecallSent, Shelf, Stance};
use wordcraft_assist::{Context, Details, Sent};

use crate::WordApp;
use crate::i18n::fmt;
use crate::theme::Tokens;

/// Why a turn recalled nothing, as Details says it.
const NO_CASE: &str = "no case is bound to this document";
const NO_FOLDER: &str = "WordCraft has no settings folder here";

/// What the window holds of it.
#[derive(Default)]
pub(crate) struct CasesState {
    /// By document id ([`wordcraft_engine::Session::document_id`]): a window holds several documents.
    bound: BTreeMap<u64, CaseRef>,
}

/// The case the shown document is bound to.
pub(crate) fn bound_for(app: &WordApp) -> Option<CaseRef> {
    app.assist.cases.bound.get(&app.session.document_id()).cloned()
}

impl WordApp {
    /// Bind the shown document to `at`, or unbind it with `None`. A live conversation of the
    /// document bound to another case ends, stopping its turn: it holds that case's notes, and
    /// the next ask would only replace it. For tests, until the commands that do this for the
    /// person land; nothing else binds a case.
    #[doc(hidden)]
    pub fn bind_case(&mut self, at: Option<CaseRef>) {
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

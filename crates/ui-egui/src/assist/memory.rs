//! What the assistant remembers across conversations (#86), from the window's side: the file is
//! [`wordcraft_assist::memory`]'s, and this holds what was last read of it, carries out the
//! `assist.memory*` commands, and draws what the Assistant pane shows of it: the list in Details ›
//! Memory, the card a proposal shows, and the question a preference raises when the changes made
//! under it keep being rejected.
//!
//! Only the person changes what it says. A model's marker raises `used` and the person's Accept and
//! Reject raise `accepted` and `rejected`, and those counts only lead to a question. The window refuses a write from a script or an agent, a click or
//! key the control channel made and a macro's step: a document can tell a model to keep something,
//! and a kept preference acts on later documents.

use std::collections::BTreeMap;

use egui::{RichText, Stroke, Ui};
use serde_json::{Value, json};
use wordcraft_assist::memory::{ForModel, Instructions, Item, MAX_TEXT, Memory, SentPreference};
use wordcraft_assist::{Decision, ProposalAnswer, Sent};
use wordcraft_engine::cmd::review::AuthoredChange;

use crate::WordApp;
use crate::i18n::fmt;
use crate::options::{note, section};
use crate::theme::Tokens;

/// Why a script or an agent can't change what the assistant remembers.
pub(crate) const YOURS: &str =
    "Only you can change what the assistant remembers: use Details › Memory in the Assistant pane, and click the button yourself";

/// Why there is nowhere to keep it.
const NO_FOLDER: &str = "WordCraft has no settings folder here to keep what the assistant remembers";

/// Milliseconds between two readings of the file while the page shows it: another WordCraft may
/// have written it.
const READ_EVERY: f64 = 2000.0;

/// What the window holds of it.
#[derive(Default)]
pub(crate) struct MemoryState {
    /// The file, as last read.
    pub(crate) memory: Memory,
    /// When it was (`clock::now_ms`); `None` before the first time.
    read_at: Option<f64>,
    /// What the page's Add box says.
    new: String,
    /// The item being edited on the page, and its text so far.
    editing: Option<(String, String)>,
    /// The proposal being edited in the pane, and its text so far.
    editing_proposal: Option<(u64, String)>,
    /// A chip was clicked: Details scrolls to Memory.
    pub(crate) show_list: bool,
}

/// Read the file again, if there is a settings folder.
fn reload(app: &mut WordApp) {
    if let Some(dir) = &app.services.settings_dir {
        app.assist.memory.memory = Memory::load(dir);
        app.assist.memory.read_at = Some(crate::clock::now_ms());
    }
}

/// A new conversation numbers its proposals from 1 again: a draft of the last one's isn't for it.
pub(crate) fn conversation_started(app: &mut WordApp) {
    app.assist.memory.editing_proposal = None;
}

/// What the next turn carries, read fresh: another WordCraft's keep counts. Nothing without a
/// folder to keep it in, so the model isn't asked to propose what no click could keep. `sent` gets
/// what the pane says about it.
pub(crate) fn for_turn(app: &mut WordApp, sent: &mut Sent) -> Option<Instructions> {
    sent.at = wordcraft_engine::cmd::now_iso();
    app.services.settings_dir.as_ref()?;
    reload(app);
    let memory = &app.assist.memory.memory;
    let ForModel { text, sent: ids, left_out } = memory.for_model();
    sent.memory = memory.preferences(&ids);
    sent.memory_left_out = u32::try_from(left_out.len()).unwrap_or(u32::MAX);
    text
}

/// Every frame: what the conversation's answers just marked counts as used. A file another
/// WordCraft holds for too long misses a count, which only orders what is sent.
pub(crate) fn tick(app: &mut WordApp) {
    let Some(c) = app.assist.conversation.as_mut() else { return };
    let ids = c.assistant.take_used();
    if ids.is_empty() {
        return;
    }
    let Some(dir) = app.services.settings_dir.clone() else { return };
    let now = wordcraft_engine::cmd::now_iso();
    if let Err(e) = Memory::count(&dir, |m| m.note_used(&ids, &now)) {
        log::warn!("the preferences used weren't counted: {e}");
    }
    reload(app);
}

/// The person's Accept or Reject resolved `changes`: each counts for the preferences marked in the
/// turn it was made in ([`wordcraft_assist::Transcript::marked_at`]). A change has one revision and
/// a turn's changes share few, so the turns are looked for once a date.
pub(super) fn decided(app: &mut WordApp, decision: Decision, changes: &[AuthoredChange]) {
    let (Some(dir), Some(a)) = (app.services.settings_dir.clone(), app.conversation()) else { return };
    let mut by_date: BTreeMap<&str, u64> = BTreeMap::new();
    for change in changes {
        *by_date.entry(change.date.as_str()).or_default() += 1;
    }
    let mut by_item: BTreeMap<SentPreference, u64> = BTreeMap::new();
    for (date, count) in by_date {
        for sent in a.transcript().marked_at(date) {
            let total = by_item.entry(sent).or_default();
            *total = total.saturating_add(count);
        }
    }
    if by_item.is_empty() {
        return;
    }
    let counted = Memory::count(&dir, |m| {
        for (sent, count) in &by_item {
            m.note_decided(sent, decision, *count);
        }
    });
    if let Err(e) = counted {
        log::warn!("the changes decided weren't counted: {e}");
    }
    reload(app);
}

/// `assist.memory`.
pub(crate) fn read(app: &mut WordApp) -> Result<Value, String> {
    if app.services.settings_dir.is_none() {
        return Err(NO_FOLDER.into());
    }
    reload(app);
    let m = &app.assist.memory.memory;
    let ForModel { sent, left_out, .. } = m.for_model();
    let asking: Vec<&str> = m.items.iter().filter(|i| i.asks()).map(|i| i.id.as_str()).collect();
    Ok(json!({"enabled": m.enabled, "items": m.items, "sent": sent, "leftOut": left_out, "asking": asking, "damaged": m.damaged}))
}

/// One of the `assist.memory.*` writes, whose params the command checked. Only the person's: the
/// one guard every write passes, the person's cases (`memory.case.*`, `memory.wall.*`) included.
pub(crate) fn write(app: &mut WordApp, what: &str, req: &Value) -> Result<Value, String> {
    if !app.assist.by_person {
        return Err(YOURS.into());
    }
    let dir = app.services.settings_dir.clone().ok_or(NO_FOLDER)?;
    if what.starts_with("memory.case.") || what.starts_with("memory.wall.") {
        return super::cases::write(app, &dir, what, req);
    }
    let text = req.get("text").and_then(Value::as_str);
    let id = req.get("id").and_then(Value::as_str).unwrap_or("");
    let keeps_item = what == "memory.keep" && req.get("id").is_some();
    if (what == "memory.keep" && !keeps_item) || what == "memory.dismiss" {
        return answer(app, &dir, what == "memory.keep", req);
    }
    let r = Memory::update(&dir, |m| match what {
        "memory.add" => {
            let now = wordcraft_engine::cmd::now_iso();
            m.add(text.unwrap_or(""), &now).map(|item| json!({"id": item.id}))
        }
        "memory.edit" => m.edit(id, text, req.get("off").and_then(Value::as_bool)).map(|()| json!({"id": id})),
        "memory.remove" => m.remove(id).map(|()| json!({"removed": id})),
        "memory.keep" => m.keep_asked(id).map(|()| json!({"id": id})),
        "memory.clear" => {
            m.clear();
            Ok(json!({"cleared": true}))
        }
        "memory.enabled" => {
            m.enabled = req.get("value").and_then(Value::as_bool).unwrap_or(m.enabled);
            Ok(json!({"value": m.enabled}))
        }
        other => Err(format!("`{}`: not a memory request", other.chars().take(40).collect::<String>())),
    });
    reload(app);
    r
}

/// The person's Keep or Not this on proposal `req["proposal"]` of the conversation: what it says,
/// or what they edited it to, is kept, or its text is said no to. A proposal that isn't waiting
/// changes nothing. It is marked answered only once the file has the answer.
fn answer(app: &mut WordApp, dir: &std::path::Path, keep: bool, req: &Value) -> Result<Value, String> {
    let id = req.get("proposal").and_then(Value::as_u64).unwrap_or(0);
    let proposed = app.conversation().and_then(|a| a.transcript().pending_proposal(id)).map(str::to_string);
    let proposed = proposed.ok_or("that proposal isn't waiting for an answer")?;
    let kept = if keep {
        let text = req.get("text").and_then(Value::as_str).unwrap_or(&proposed);
        let now = wordcraft_engine::cmd::now_iso();
        Memory::update(dir, |m| m.add(text, &now)).map(Some)
    } else {
        Memory::update(dir, |m| {
            m.dismiss(&proposed);
            Ok(None)
        })
    };
    reload(app);
    let kept = kept?;
    if let Some(a) = app.conversation_mut() {
        let said = if keep { ProposalAnswer::Kept } else { ProposalAnswer::Dismissed };
        a.answer_proposal(id, said, kept.as_ref().map(|item| item.text.as_str()));
    }
    Ok(kept.map_or(json!({"dismissed": id}), |item| json!({"id": item.id})))
}

/// A proposal in the Assistant pane: what it says and, while it waits, Keep, Edit and Not this.
/// None for one the person already kept or said no to, or while memory is off. The text is the
/// model's, which a document may have prompted, so it is shown whole before anything is stored.
pub(crate) fn proposal_card(app: &mut WordApp, ui: &mut Ui, t: &Tokens, id: u64, text: &str, answer: Option<ProposalAnswer>) {
    let memory = &app.assist.memory.memory;
    let shows = app.services.settings_dir.is_some() && memory.enabled && (answer.is_some() || !memory.known(text));
    if !shows {
        return;
    }
    let waiting = answer.is_none();
    let stroke = if waiting { Stroke::new(1.5, t.accent) } else { Stroke::new(1.0, t.border_strong) };
    egui::Frame::NONE.stroke(stroke).corner_radius(6).inner_margin(6).show(ui, |ui| {
        ui.set_width(ui.available_width());
        match answer {
            None => {
                ui.label(RichText::new(tl!("Keep this for every conversation?")).small().color(t.text_dim));
                waiting_card(app, ui, t, id, text);
            }
            Some(answer) => {
                let said = if answer == ProposalAnswer::Kept { tl!("Kept") } else { tl!("Not kept") };
                ui.label(RichText::new(said).small().color(t.text_dim));
                ui.add(egui::Label::new(RichText::new(text).small().color(t.text_dim)).wrap());
            }
        }
    });
}

/// A waiting proposal's text, or the box it is being edited in, and its buttons.
fn waiting_card(app: &mut WordApp, ui: &mut Ui, t: &Tokens, id: u64, text: &str) {
    let mine = app.assist.memory.editing_proposal.as_ref().is_some_and(|(i, _)| *i == id);
    let editing = if mine { app.assist.memory.editing_proposal.take() } else { None };
    let Some((_, mut edited)) = editing else {
        ui.add(egui::Label::new(RichText::new(text).font(crate::theme::semibold(12.0)).color(t.text)).wrap());
        ui.horizontal_wrapped(|ui| {
            if ui.button(tl!("Keep")).clicked() {
                let _ = app.run("assist.memory.keep", json!({"proposal": id}));
            }
            if ui.button(tl!("Edit")).clicked() {
                app.assist.memory.editing_proposal = Some((id, text.to_string()));
            }
            if ui.button(tl!("Not this")).clicked() {
                let _ = app.run("assist.memory.dismiss", json!({"proposal": id}));
            }
        });
        return;
    };
    ui.add(egui::TextEdit::multiline(&mut edited).char_limit(MAX_TEXT).desired_rows(2).desired_width(f32::INFINITY).hint_text(tl!("What to keep")));
    let mut open = true;
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(!edited.trim().is_empty(), egui::Button::new(tl!("Keep"))).clicked()
            && app.run("assist.memory.keep", json!({"proposal": id, "text": edited})).is_ok()
        {
            open = false;
        }
        if ui.button(tl!("Cancel")).clicked() {
            open = false;
        }
    });
    if open {
        app.assist.memory.editing_proposal = Some((id, edited));
    }
}

/// Read the file again if it was last read a while ago: another WordCraft may have written it, and
/// a preference waiting for an answer waits across restarts.
fn read_if_stale(app: &mut WordApp) {
    if app.assist.memory.read_at.is_none_or(|at| crate::clock::now_ms() - at > READ_EVERY) {
        reload(app);
    }
}

/// What File › Options › Assistant says about it: where the list is, or the list itself where the
/// pane can't show it, as turning this off and clearing it mustn't take opening a document.
pub(crate) fn options_section(app: &mut WordApp, ui: &mut Ui) {
    section(ui, "Memory");
    if app.has_document() && super::desktop(app) {
        note(ui, "What the assistant remembers is in the Assistant pane, under Details › Memory.");
    } else {
        let t = Tokens::get(ui.ctx());
        page(app, ui, &t);
    }
}

/// The questions the pane asks now, for the end of the conversation: a closed Details asks nothing.
pub(crate) fn questions(app: &mut WordApp, ui: &mut Ui, t: &Tokens) {
    read_if_stale(app);
    if !app.assist.memory.memory.enabled {
        return;
    }
    for item in app.assist.memory.memory.items.iter().filter(|i| i.asks()).cloned().collect::<Vec<_>>() {
        ui.push_id(("memory question", &item.id), |ui| question(app, ui, t, &item));
        ui.add_space(6.0);
    }
}

/// The one rule's question: nothing changes until the person answers.
fn question(app: &mut WordApp, ui: &mut Ui, t: &Tokens, item: &Item) {
    egui::Frame::NONE.stroke(Stroke::new(1.5, t.accent)).corner_radius(6).inner_margin(6).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let (rejected, accepted) = (item.rejected.to_string(), item.accepted.to_string());
        let says = fmt(
            tl!("You rejected {rejected} changes made with this preference and accepted {accepted}. Keep it?"),
            &[("rejected", &rejected), ("accepted", &accepted)],
        );
        ui.add(egui::Label::new(RichText::new(says).small().color(t.text_dim)).wrap());
        ui.add(egui::Label::new(RichText::new(&item.text).font(crate::theme::semibold(12.0)).color(t.text)).wrap());
        ui.horizontal_wrapped(|ui| {
            if ui.button(tl!("Keep")).clicked() {
                let _ = app.run("assist.memory.keep", json!({"id": item.id}));
            }
            if ui.button(tl!("Remove")).clicked() {
                let _ = app.run("assist.memory.remove", json!({"id": item.id}));
            }
        });
    });
}

/// How much an item has been used, as Details › Memory says it: nothing until it has been.
fn counts(item: &Item) -> Option<String> {
    (item.used > 0 || item.accepted > 0 || item.rejected > 0).then(|| {
        let (used, accepted, rejected) = (item.used.to_string(), item.accepted.to_string(), item.rejected.to_string());
        fmt(
            tl!("Used {used} times. Changes: {accepted} accepted, {rejected} rejected."),
            &[("used", &used), ("accepted", &accepted), ("rejected", &rejected)],
        )
    })
}

/// What Details › Memory says about a turn: the preferences a chip marked, each with its chip's
/// number, and the rest of what it sent, which apply to the whole answer. `open`: the rest shows
/// without a click.
pub(crate) fn turn(ui: &mut Ui, t: &Tokens, sent: &Sent, used: &[String], open: bool) {
    let says = |id: &str| sent.memory.iter().find(|p| p.id == id).map(|p| p.text.as_str());
    for (n, id) in used.iter().enumerate() {
        let Some(text) = says(id) else { continue };
        ui.horizontal_top(|ui| {
            ui.label(RichText::new(chip_label(n.saturating_add(1))).small().color(t.accent));
            ui.add(egui::Label::new(RichText::new(text).small().color(t.text)).wrap());
        });
    }
    let others: Vec<&str> = sent.memory.iter().filter(|p| !used.contains(&p.id)).map(|p| p.text.as_str()).collect();
    if !others.is_empty() {
        let header = fmt(tl!("The whole answer ({count})"), &[("count", &others.len().to_string())]);
        egui::CollapsingHeader::new(RichText::new(header).small().color(t.text_dim)).default_open(open).show(ui, |ui| {
            for text in others {
                ui.add(egui::Label::new(RichText::new(text).small().color(t.text)).wrap());
            }
        });
    }
    if sent.memory_left_out > 0 {
        let says = fmt(tl!("{count} more didn't fit and weren't sent."), &[("count", &sent.memory_left_out.to_string())]);
        ui.add(egui::Label::new(RichText::new(says).small().color(t.orange)).wrap());
    }
}

/// What a preference's chip says, in an answer and in Details › Memory.
pub(crate) fn chip_label(number: usize) -> String {
    fmt(tl!("M{number}"), &[("number", &number.to_string())])
}

/// The list in Details › Memory: the setting, what is kept, and the way to add to it.
pub(crate) fn page(app: &mut WordApp, ui: &mut Ui, t: &Tokens) {
    if app.services.settings_dir.is_none() {
        note(ui, NO_FOLDER);
        return;
    }
    read_if_stale(app);
    let mut enabled = app.assist.memory.memory.enabled;
    if ui.checkbox(&mut enabled, tl!("Give the assistant what you kept")).changed() {
        let _ = app.run("assist.memory.enabled", json!({"value": enabled}));
    }
    note(
        ui,
        "What you keep here goes to every assistant in every later conversation, sent to whichever provider runs the turn. It stays in a file in WordCraft's settings folder that only you can read, never in a document. Off, nothing is sent, and what you kept stays until you clear it.",
    );
    if app.assist.memory.memory.damaged {
        note(ui, "The file holding this can't be read whole. The next change moves it aside as assistant-memory.json.damaged, so nothing is lost.");
    }
    ui.add_space(4.0);
    list(app, ui, t);
    ui.add_space(4.0);
    add_row(app, ui);
}

fn list(app: &mut WordApp, ui: &mut Ui, t: &Tokens) {
    let items = app.assist.memory.memory.items.clone();
    let ForModel { left_out, .. } = app.assist.memory.memory.for_model();
    for item in &items {
        if app.assist.memory.editing.as_ref().is_some_and(|(id, _)| *id == item.id) {
            edit_row(app, ui);
            continue;
        }
        ui.horizontal_wrapped(|ui| {
            let color = if item.off { ui.visuals().weak_text_color() } else { ui.visuals().text_color() };
            ui.label(RichText::new(&item.text).color(color));
        });
        if let Some(counts) = counts(item) {
            ui.add(egui::Label::new(RichText::new(counts).small().color(t.text_dim)).wrap());
        }
        ui.horizontal(|ui| {
            let mut on = !item.off;
            if ui.checkbox(&mut on, tl!("On")).changed() {
                let _ = app.run("assist.memory.edit", json!({"id": item.id, "off": !on}));
            }
            if ui.small_button(tl!("Edit")).clicked() {
                app.assist.memory.editing = Some((item.id.clone(), item.text.clone()));
            }
            if ui.small_button(tl!("Remove")).clicked() {
                let _ = app.run("assist.memory.remove", json!({"id": item.id}));
            }
        });
        if left_out.contains(&item.id) {
            note(ui, "Not sent: what you kept is more than a turn takes, and this was used least recently.");
        }
        if app.assist.memory.memory.enabled && item.asks() {
            ui.push_id(("memory list question", &item.id), |ui| question(app, ui, t, item));
        }
    }
    if !items.is_empty() {
        let count = fmt(tl!("{count} kept"), &[("count", &items.len().to_string())]);
        ui.horizontal(|ui| {
            ui.label(RichText::new(count).small().weak());
            if ui.small_button(tl!("Clear all")).clicked() {
                let _ = app.run("assist.memory.clear", json!({}));
            }
        });
    }
}

/// The row of the item being edited: its text in a box, with Save and Cancel.
fn edit_row(app: &mut WordApp, ui: &mut Ui) {
    let Some((id, mut text)) = app.assist.memory.editing.take() else { return };
    ui.add(egui::TextEdit::multiline(&mut text).char_limit(MAX_TEXT).desired_rows(2).desired_width(f32::INFINITY).hint_text(tl!("What to keep")));
    let mut open = true;
    ui.horizontal(|ui| {
        if ui.small_button(tl!("Save")).clicked() && app.run("assist.memory.edit", json!({"id": id, "text": text})).is_ok() {
            open = false;
        }
        if ui.small_button(tl!("Cancel")).clicked() {
            open = false;
        }
    });
    if open {
        app.assist.memory.editing = Some((id, text));
    }
}

/// The Add box.
fn add_row(app: &mut WordApp, ui: &mut Ui) {
    let mut text = std::mem::take(&mut app.assist.memory.new);
    ui.add(
        egui::TextEdit::multiline(&mut text)
            .char_limit(MAX_TEXT)
            .desired_rows(2)
            .desired_width(f32::INFINITY)
            .hint_text(tl!("Something the assistant should always know, such as a list style you prefer")),
    );
    if ui.add_enabled(!text.trim().is_empty(), egui::Button::new(tl!("Add"))).clicked() && app.run("assist.memory.add", json!({"text": text})).is_ok()
    {
        text.clear();
    }
    app.assist.memory.new = text;
}

#[cfg(test)]
pub(crate) mod tests;

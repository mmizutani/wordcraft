//! What the assistant remembers across conversations (#86), from the window's side: the file is
//! [`wordcraft_assist::memory`]'s, and this holds what was last read of it, carries out the
//! `assist.memory*` commands, and draws the list in File › Options › Assistant and the card a
//! proposal shows in the Assistant pane.
//!
//! Only the person changes it. The window refuses a write from a script or an agent, a click or
//! key the control channel made and a macro's step: a document can tell a model to keep something,
//! and a kept preference acts on later documents.

use egui::{RichText, Stroke, Ui};
use serde_json::{Value, json};
use wordcraft_assist::ProposalAnswer;
use wordcraft_assist::memory::{ForModel, MAX_TEXT, Memory};

use crate::WordApp;
use crate::i18n::fmt;
use crate::options::{note, section};
use crate::theme::Tokens;

/// Why a script or an agent can't change what the assistant remembers.
pub(crate) const YOURS: &str = "Only you can change what the assistant remembers: use File › Options › Assistant, and click the button yourself";

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
}

/// Read the file again, if there is a settings folder.
fn reload(app: &mut WordApp) {
    if let Some(dir) = &app.services.settings_dir {
        app.assist.memory.memory = Memory::load(dir);
        app.assist.memory.read_at = Some(crate::clock::now_ms());
    }
}

/// What the next turn carries, read fresh: another WordCraft's keep counts. Nothing without a
/// folder to keep it in, so the model isn't asked to propose what no click could keep.
pub(crate) fn for_turn(app: &mut WordApp) -> Option<String> {
    app.services.settings_dir.as_ref()?;
    reload(app);
    app.assist.memory.memory.for_model().text
}

/// `assist.memory`.
pub(crate) fn read(app: &mut WordApp) -> Result<Value, String> {
    if app.services.settings_dir.is_none() {
        return Err(NO_FOLDER.into());
    }
    reload(app);
    let m = &app.assist.memory.memory;
    let ForModel { sent, left_out, .. } = m.for_model();
    Ok(json!({"enabled": m.enabled, "items": m.items, "sent": sent, "leftOut": left_out, "damaged": m.damaged}))
}

/// One of the `assist.memory.*` writes, whose params the command checked. Only the person's: the
/// one guard every write passes.
pub(crate) fn write(app: &mut WordApp, what: &str, req: &Value) -> Result<Value, String> {
    if !app.assist.by_person {
        return Err(YOURS.into());
    }
    let dir = app.services.settings_dir.clone().ok_or(NO_FOLDER)?;
    let text = req.get("text").and_then(Value::as_str);
    let id = req.get("id").and_then(Value::as_str).unwrap_or("");
    if what == "memory.keep" || what == "memory.dismiss" {
        return answer(app, &dir, what == "memory.keep", req);
    }
    let r = Memory::update(&dir, |m| match what {
        "memory.add" => {
            let now = wordcraft_engine::cmd::now_iso();
            m.add(text.unwrap_or(""), &now).map(|item| json!({"id": item.id}))
        }
        "memory.edit" => m.edit(id, text, req.get("off").and_then(Value::as_bool)).map(|()| json!({"id": id})),
        "memory.remove" => m.remove(id).map(|()| json!({"removed": id})),
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
    let r = if keep {
        let text = req.get("text").and_then(Value::as_str).unwrap_or(&proposed);
        let now = wordcraft_engine::cmd::now_iso();
        Memory::update(dir, |m| m.add(text, &now).map(|item| json!({"id": item.id})))
    } else {
        Memory::update(dir, |m| {
            m.dismiss(&proposed);
            Ok(json!({"dismissed": id}))
        })
    };
    reload(app);
    if r.is_ok()
        && let Some(a) = app.conversation_mut()
    {
        a.answer_proposal(id, if keep { ProposalAnswer::Kept } else { ProposalAnswer::Dismissed });
    }
    r
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
    let editing = app.assist.memory.editing_proposal.take().filter(|(i, _)| *i == id);
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
    ui.add(egui::TextEdit::multiline(&mut edited).char_limit(MAX_TEXT).desired_rows(2).desired_width(f32::INFINITY));
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

/// The section of File › Options › Assistant.
pub(crate) fn page(app: &mut WordApp, ui: &mut Ui) {
    section(ui, "Memory");
    if app.services.settings_dir.is_none() {
        note(ui, NO_FOLDER);
        return;
    }
    if app.assist.memory.read_at.is_none_or(|at| crate::clock::now_ms() - at > READ_EVERY) {
        reload(app);
    }
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
    list(app, ui);
    ui.add_space(4.0);
    add_row(app, ui);
}

fn list(app: &mut WordApp, ui: &mut Ui) {
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
    ui.add(egui::TextEdit::multiline(&mut text).char_limit(MAX_TEXT).desired_rows(2).desired_width(f32::INFINITY));
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
mod tests;

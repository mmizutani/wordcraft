//! What the assistant remembers across conversations (#86), from the window's side: the file is
//! [`wordcraft_assist::memory`]'s, and this holds what was last read of it, carries out the
//! `assist.memory*` commands, and draws the list in File › Options › Assistant.
//!
//! Only the person changes it. The window refuses a write from a script or an agent, a click or
//! key the control channel made and a macro's step: a document can tell a model to keep something,
//! and a kept preference acts on later documents.

use egui::{RichText, Ui};
use serde_json::{Value, json};
use wordcraft_assist::memory::{ForModel, MAX_TEXT, Memory};

use crate::WordApp;
use crate::i18n::fmt;
use crate::options::{note, section};

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
}

/// Read the file again, if there is a settings folder.
fn reload(app: &mut WordApp) {
    if let Some(dir) = &app.services.settings_dir {
        app.assist.memory.memory = Memory::load(dir);
        app.assist.memory.read_at = Some(crate::clock::now_ms());
    }
}

/// What the next turn carries, read fresh: another WordCraft's keep counts.
pub(crate) fn for_turn(app: &mut WordApp) -> Option<String> {
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

use super::{CLOUD_CREATE_FORMAT, CloudFailureKind, SavePhase};
use crate::{WordApp, panes};
use serde_json::json;

pub(crate) fn show_save(app: &mut WordApp, ui: &mut egui::Ui) {
    if !app.cloud_saves_current() {
        if !app.cloud_can_create() {
            ui.label(tl!(CLOUD_CREATE_FORMAT));
            return;
        }
        let label = ui.label(tl!("Document name"));
        ui.add(egui::TextEdit::singleline(&mut app.cloud.draft_name).desired_width(280.0)).labelled_by(label.id);
        if ui.button(tl!("Save to Cloud")).clicked() {
            let name = app.cloud.draft_name.clone();
            if app.run("ui.cloudSave", json!({"name": name})).is_ok() {
                app.ui.backstage = false;
                app.canvas.want_focus = true;
            }
        }
        if let Some((message, _)) = &app.status_msg {
            ui.label(message);
        }
        if ui.button(tl!("Download DOCX")).clicked() {
            let _ = app.run("ui.cloudDownload", json!({}));
        }
    } else {
        show_status(app, ui);
    }
}

pub(crate) fn show_status(app: &mut WordApp, ui: &mut egui::Ui) {
    if app.show_hosted_room_status(ui) {
        return;
    }
    let document = app.session.document_id();
    let sending = app.cloud.sending(document);
    let failure = app.cloud.saves.get(&document).and_then(|save| match &save.phase {
        SavePhase::Failed(error) => Some((error.kind, error.message.clone())),
        SavePhase::Sending(_) => None,
    });
    let retry = failure.as_ref().is_some_and(|(kind, _)| !matches!(kind, CloudFailureKind::Stale | CloudFailureKind::Missing));
    let stale = failure.as_ref().is_some_and(|(kind, _)| *kind == CloudFailureKind::Stale);
    ui.horizontal_wrapped(|ui| {
        if sending {
            ui.spinner();
            ui.label(tl!("Saving to Cloud…"));
        } else if let Some((_, message)) = failure {
            ui.label(message);
        } else if app.document_unsaved() || panes::has_comment_drafts(app) {
            ui.label(tl!("Unsaved changes"));
        } else {
            ui.label(tl!("Saved to Cloud"));
        }
        if retry && ui.button(tl!("Retry")).clicked() {
            let _ = app.run("ui.cloudSave", json!({}));
        }
        if !sending && !retry && !stale && !app.cloud.unsaved(document) && app.document_unsaved() && ui.button(tl!("Save to Cloud")).clicked() {
            let _ = app.run("file.save", json!({}));
        }
        if ui.button(if stale { tl!("Download Your Edits") } else { tl!("Download DOCX") }).clicked() {
            let _ = app.run("ui.cloudDownload", json!({}));
        }
        if stale && ui.button(tl!("Reload Latest")).clicked() {
            let _ = app.run("ui.cloudOpen", json!({"reload": true}));
        }
    });
}

pub(crate) fn show_waiting_navigation(app: &mut WordApp, ui: &mut egui::Ui) {
    if app.cloud.held() || app.cloud.fetching() {
        ui.horizontal(|ui| {
            if app.cloud.held() {
                ui.label(tl!("Waiting for the save to finish…"));
            } else {
                ui.spinner();
                ui.label(tl!("Opening cloud document…"));
            }
            if ui.button(tl!("Cancel Navigation")).clicked() {
                let _ = app.run("ui.cloudCancelNavigation", json!({}));
            }
        });
    }
}

pub(crate) fn show_library(app: &mut WordApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.heading(tl!("Cloud documents"));
        let busy = app.cloud.library.busy.is_some();
        if ui.add_enabled(!busy, egui::Button::new(tl!("Refresh"))).clicked() {
            app.cloud.library.message.clear();
            let _ = app.run("ui.cloudRefresh", json!({}));
        }
        if busy {
            ui.spinner();
        }
    });
    if app.cloud.library.busy.is_none() && (app.cloud.library.refresh_needed || (!app.cloud.library.loaded && app.cloud.library.message.is_empty())) {
        let _ = app.run("ui.cloudRefresh", json!({}));
    }
    if !app.cloud.library.message.is_empty() {
        ui.label(&app.cloud.library.message);
    }
    if let Some(entry) = app.cloud.library.confirm_delete.clone() {
        ui.label(crate::i18n::fmt(tl!("Permanently delete {name} from the cloud?"), &[("name", &entry.name)]));
        ui.horizontal(|ui| {
            if ui.button(tl!("Cancel")).clicked() {
                app.cloud.library.confirm_delete = None;
            }
            if ui.add_enabled(app.cloud.library.busy.is_none(), egui::Button::new(tl!("Delete Cloud Document"))).clicked() {
                app.cloud.library.confirm_delete = None;
                let _ = app.run("ui.cloudDelete", json!({"id":entry.id, "revision":entry.revision}));
            }
        });
        return;
    }
    if app.cloud.library.entries.is_empty() && app.cloud.library.loaded {
        ui.label(tl!("No cloud documents yet."));
    }
    let busy = app.cloud.library.busy.is_some();
    for entry in app.cloud.library.entries.clone() {
        ui.push_id(entry.id.clone(), |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button(&entry.name).clicked() {
                    let _ = app.run("ui.cloudOpen", json!({"id":entry.id}));
                }
                if ui.add_enabled(!busy, egui::Button::new(tl!("Download"))).clicked() {
                    let _ = app.run("ui.cloudLibraryDownload", json!({"id":entry.id}));
                }
                if ui.add_enabled(!busy, egui::Button::new(tl!("Delete…"))).clicked() {
                    app.cloud.library.confirm_delete = Some(entry);
                }
            });
        });
    }
    if app.cloud.library.next_cursor.is_some() && ui.add_enabled(!busy, egui::Button::new(tl!("Load More"))).clicked() {
        let _ = app.run("ui.cloudMore", json!({}));
    }
}

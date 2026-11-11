use super::*;
use crate::{Services, WordApp, dialogs::Dialog};
use serde_json::json;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::mpsc,
};
use wordcraft_doc::{Document, StoryRef};
use wordcraft_engine::{Session, io};

type Respond = mpsc::Sender<Result<CloudResponse, CloudFailure>>;
type Requests = mpsc::Receiver<(CloudRequest, Respond)>;
type Downloads = Rc<RefCell<Vec<(String, Vec<u8>)>>>;

fn app() -> (WordApp, Requests, Downloads, Rc<Cell<bool>>) {
    let (requests, received) = mpsc::channel();
    let operation = Cell::new(0);
    let cloud = CloudService {
        new_operation_id: Box::new(move || {
            operation.set(operation.get() + 1);
            Ok(format!("operation-{}", operation.get()))
        }),
        request: Box::new(move |request| {
            let (respond, reply) = mpsc::channel();
            requests.send((request, respond)).unwrap();
            reply
        }),
    };
    let downloads = Downloads::default();
    let dirty = Rc::new(Cell::new(false));
    let app = WordApp::new(
        Session::new(Document::new()),
        Services {
            cloud: Some(cloud),
            download: Some(Box::new({
                let downloads = Rc::clone(&downloads);
                move |name, bytes| {
                    downloads.borrow_mut().push((name.into(), bytes.to_vec()));
                    Ok(())
                }
            })),
            on_dirty: Some(Box::new({
                let dirty = Rc::clone(&dirty);
                move |value| dirty.set(value)
            })),
            ..Default::default()
        },
    );
    (app, received, downloads, dirty)
}

fn next(requests: &Requests) -> (CloudRequest, Respond) {
    requests.try_recv().unwrap()
}

fn docx(text: &str) -> Vec<u8> {
    io::save_bytes("test.docx", &Document::from_text(text)).unwrap()
}

fn test_passphrase(scenario: &str) -> String {
    format!("WordCraft test fixture {scenario}")
}

fn edit(app: &mut WordApp, text: &str) {
    app.run("text.insert", json!({"text": text})).unwrap();
}

fn entry(id: &str, revision: u64) -> CloudEntry {
    CloudEntry { id: id.into(), name: "Cloud document.docx".into(), revision, byte_size: 10, saved_at: 1 }
}

fn saved(request: CloudRequest, respond: Respond) {
    let CloudRequest::Save { id, base_revision, name, bytes, .. } = request else {
        panic!("expected a save");
    };
    respond
        .send(Ok(CloudResponse::Save(CloudEntry {
            name,
            byte_size: bytes.len() as u64,
            ..entry(id.as_deref().unwrap_or("cloud-1"), base_revision.unwrap_or_default() + 1)
        })))
        .unwrap();
}

fn fail(respond: Respond, kind: CloudFailureKind) {
    respond.send(Err(CloudFailure { kind, message: format!("{kind:?}") })).unwrap();
}

fn start_save(app: &mut WordApp, requests: &Requests) -> (CloudRequest, Respond) {
    app.run("ui.cloudSave", json!({"name":"Cloud document.docx"})).unwrap();
    next(requests)
}

fn list(app: &mut WordApp, requests: &Requests, entry: CloudEntry) {
    app.run("ui.cloudRefresh", json!({})).unwrap();
    let (request, reply) = next(requests);
    assert_eq!(request, CloudRequest::List { cursor: None });
    reply.send(Ok(CloudResponse::List { documents: vec![entry], next_cursor: None })).unwrap();
    app.settle_cloud();
}

#[test]
fn first_cloud_save_keeps_edits_unsaved_until_acknowledged() {
    let (mut app, requests, _, leave_guard) = app();
    let bytes = docx("Imported local document.");
    app.run("file.open", json!({"path":"local.docx","data":wordcraft_engine::cmd::insert::base64_encode(&bytes)})).unwrap();
    edit(&mut app, "unsaved changes");
    let (request, reply) = start_save(&mut app, &requests);
    let CloudRequest::Save { id: None, base_revision: None, bytes, .. } = &request else { panic!("expected first save") };
    assert!(io::open_bytes("result.docx", bytes).unwrap().plain_text(StoryRef::Body).contains("unsaved changes"));
    assert!(app.session.dirty);
    assert!(leave_guard.get());
    assert!(!app.cloud.bound(app.session.document_id()));
    saved(request, reply);
    app.settle_cloud();
    app.report_dirty();
    assert!(!app.session.dirty);
    assert!(!leave_guard.get());
    assert!(app.cloud.bound(app.session.document_id()));
    app.run("file.save", json!({})).unwrap();
    assert!(matches!(next(&requests).0, CloudRequest::Save { id: Some(id), base_revision: Some(1), .. } if id == "cloud-1"));
}

#[test]
fn retry_keeps_the_snapshot_and_authentication_keeps_the_operation() {
    let (mut app, requests, _, _) = app();
    edit(&mut app, "first snapshot");
    let (request, reply) = start_save(&mut app, &requests);
    edit(&mut app, "newer edit");
    fail(reply, CloudFailureKind::Authentication);
    app.settle_cloud();
    assert!(app.session.dirty);
    app.run("ui.cloudSave", json!({})).unwrap();
    let (retry, reply) = next(&requests);
    assert_eq!(retry, request);
    fail(reply, CloudFailureKind::Abandoned);
    app.settle_cloud();
    app.run("ui.cloudSave", json!({})).unwrap();
    let (mut expected, reply) = next(&requests);
    saved(expected.clone(), reply);
    if let CloudRequest::Save { operation_id, .. } = &mut expected {
        assert_eq!(operation_id, "operation-2");
        *operation_id = "operation-1".into();
    }
    assert_eq!(expected, request);
    app.settle_cloud();
    assert!(app.session.dirty, "the newer edit remains unsaved");
    app.run("file.save", json!({})).unwrap();
    let (next_save, _) = next(&requests);
    let CloudRequest::Save { base_revision: Some(1), bytes, .. } = next_save else { panic!("expected the next revision") };
    assert!(io::open_bytes("new.docx", &bytes).unwrap().plain_text(StoryRef::Body).contains("newer edit"));
}

#[test]
fn a_held_close_waits_for_the_first_save_and_cancel_keeps_the_document_open() {
    let (mut app, requests, _, _) = app();
    let original = app.session.document_id();
    let (save, reply) = start_save(&mut app, &requests);
    assert!(!app.session.dirty, "even an unedited document has an unresolved save");
    assert_eq!(app.run("file.close", json!({})).unwrap()["pending"], "saveChanges");
    app.run("ui.saveChanges", json!({"answer":"save"})).unwrap();
    assert!(app.cloud.held());
    assert_eq!(app.session.document_id(), original);
    app.run("ui.cloudCancelNavigation", json!({})).unwrap();
    saved(save, reply);
    app.settle_cloud();
    assert_eq!(app.session.document_id(), original);
    edit(&mut app, "close after saving");
    assert_eq!(app.run("file.close", json!({})).unwrap()["pending"], "saveChanges");
    app.run("ui.saveChanges", json!({"answer":"save"})).unwrap();
    let (save, reply) = next(&requests);
    saved(save, reply);
    app.settle_cloud();
    assert!(!app.has_document());
}

#[test]
fn a_late_cloud_open_confirms_new_edits_and_a_new_navigation_cancels_the_fetch() {
    let (mut app, requests, _, _) = app();
    list(&mut app, &requests, entry("target", 1));
    let original = app.session.document_id();
    app.run("ui.cloudOpen", json!({"id":"target"})).unwrap();
    let (_, reply) = next(&requests);
    edit(&mut app, "new edit during fetch");
    reply.send(Ok(CloudResponse::Open { revision: 3, bytes: docx("remote") })).unwrap();
    app.settle_cloud();
    assert_eq!(app.session.document_id(), original);
    assert!(matches!(app.dialog, Some(Dialog::SaveChanges { .. })));
    app.run("ui.saveChanges", json!({"answer":"cancel"})).unwrap();
    assert!(app.session.doc.plain_text(StoryRef::Body).contains("new edit"));

    app.run("ui.cloudOpen", json!({"id":"target"})).unwrap();
    app.run("ui.saveChanges", json!({"answer":"dontSave"})).unwrap();
    let (_, reply) = next(&requests);
    app.run("file.new", json!({})).unwrap();
    assert!(reply.send(Ok(CloudResponse::Open { revision: 4, bytes: docx("canceled") })).is_err());
    app.settle_cloud();
    assert!(!app.cloud.bound(app.session.document_id()));
}

#[test]
fn cloud_open_from_the_start_screen_uses_the_read_revision() {
    let (mut app, requests, _, _) = app();
    assert!(app.start_screen());
    assert!(!app.has_document());
    list(&mut app, &requests, entry("from-library", 2));
    app.run("ui.cloudOpen", json!({"id":"from-library"})).unwrap();
    let (request, reply) = next(&requests);
    assert_eq!(request, CloudRequest::Open { id: "from-library".into() });
    reply.send(Ok(CloudResponse::Open { revision: 5, bytes: docx("read revision") })).unwrap();
    app.settle_cloud();
    assert!(app.has_document());
    assert!(!app.session.dirty);
    assert_eq!(app.session.doc.plain_text(StoryRef::Body), "read revision");
    app.run("file.save", json!({})).unwrap();
    assert!(matches!(next(&requests).0, CloudRequest::Save { id: Some(id), base_revision: Some(5), .. } if id == "from-library"));
}

#[test]
fn stale_save_downloads_current_edits_and_confirms_reload() {
    let (mut app, requests, downloads, _) = app();
    let (request, reply) = start_save(&mut app, &requests);
    saved(request, reply);
    app.settle_cloud();
    edit(&mut app, "edits made here");
    app.run("file.save", json!({})).unwrap();
    let (_, reply) = next(&requests);
    fail(reply, CloudFailureKind::Stale);
    app.settle_cloud();
    app.run("ui.cloudDownload", json!({})).unwrap();
    app.run("file.saveAs", json!({"path":"another.docx"})).unwrap();
    assert!(app.session.dirty);
    assert!(app.cloud.bound(app.session.document_id()));
    assert_eq!(downloads.borrow().len(), 2);
    for (name, bytes) in downloads.borrow().iter() {
        assert_eq!(io::open_bytes(name, bytes).unwrap().plain_text(StoryRef::Body), "edits made here");
    }
    app.run("ui.cloudOpen", json!({"reload":true})).unwrap();
    assert!(matches!(app.dialog, Some(Dialog::SaveChanges { .. })));
    assert!(requests.try_recv().is_err());
    app.run("ui.saveChanges", json!({"answer":"dontSave"})).unwrap();
    let (_, reply) = next(&requests);
    reply.send(Ok(CloudResponse::Open { revision: 2, bytes: docx("committed elsewhere") })).unwrap();
    app.settle_cloud();
    assert_eq!(app.session.doc.plain_text(StoryRef::Body), "committed elsewhere");
    assert!(!app.session.dirty);
}

#[test]
fn library_download_uses_committed_bytes_and_stale_delete_requires_the_new_revision() {
    let (mut app, requests, downloads, _) = app();
    list(&mut app, &requests, entry("listed", 1));
    let committed = docx("library copy");
    edit(&mut app, "keep the local buffer");
    app.run("ui.cloudLibraryDownload", json!({"id":"listed"})).unwrap();
    let (request, reply) = next(&requests);
    assert_eq!(request, CloudRequest::Open { id: "listed".into() });
    reply.send(Ok(CloudResponse::Open { revision: 1, bytes: committed.clone() })).unwrap();
    app.settle_cloud();
    assert_eq!(downloads.borrow().as_slice(), [("Cloud document.docx".into(), committed)]);
    assert!(app.session.dirty);

    app.run("ui.cloudDelete", json!({"id":"listed","revision":1})).unwrap();
    let (request, reply) = next(&requests);
    assert_eq!(request, CloudRequest::Delete { id: "listed".into(), revision: 1 });
    fail(reply, CloudFailureKind::Stale);
    app.settle_cloud();
    let (request, reply) = next(&requests);
    assert_eq!(request, CloudRequest::List { cursor: None });
    reply.send(Ok(CloudResponse::List { documents: vec![entry("listed", 2)], next_cursor: None })).unwrap();
    app.settle_cloud();
    assert!(app.run("ui.cloudDelete", json!({"id":"listed","revision":1})).is_err());
    assert!(requests.try_recv().is_err());
    app.run("ui.cloudDelete", json!({"id":"listed","revision":2})).unwrap();
    let (request, reply) = next(&requests);
    assert_eq!(request, CloudRequest::Delete { id: "listed".into(), revision: 2 });
    reply.send(Ok(CloudResponse::Delete { cleanup_pending: true })).unwrap();
    app.settle_cloud();
    assert!(app.execute("ui.cloudOpen", json!({"id":"listed","force":true})).is_err());
    assert!(app.session.doc.plain_text(StoryRef::Body).contains("keep the local buffer"));
}

#[test]
fn an_unexpected_save_receipt_keeps_the_original_operation_and_dirty_state() {
    let (mut app, requests, _, _) = app();
    let (request, reply) = start_save(&mut app, &requests);
    saved(request, reply);
    app.settle_cloud();
    edit(&mut app, "must remain unsaved");
    app.run("file.save", json!({})).unwrap();
    let (request, reply) = next(&requests);
    let CloudRequest::Save { bytes, .. } = &request else { panic!("expected a save") };
    reply.send(Ok(CloudResponse::Save(CloudEntry { byte_size: bytes.len() as u64, ..entry("wrong-document", 2) }))).unwrap();
    app.settle_cloud();
    assert!(app.session.dirty);
    app.run("file.save", json!({})).unwrap();
    assert_eq!(next(&requests).0, request);
}

#[test]
fn a_save_as_copy_can_use_its_own_password_without_changing_the_cloud_document() {
    let (mut app, requests, downloads, _) = app();
    let cloud = test_passphrase("cloud");
    let copy = test_passphrase("copy");
    app.run("ui.cloudSave", json!({"name":"Cloud document.docx","password":cloud})).unwrap();
    let (request, reply) = next(&requests);
    saved(request, reply);
    app.settle_cloud();
    edit(&mut app, "encrypted copy");
    app.run("file.saveAs", json!({"path":"existing-password.docx"})).unwrap();
    app.run("file.saveAs", json!({"path":"different-password.docx","password":copy})).unwrap();
    assert_eq!(app.session.password.as_ref().unwrap().as_str(), cloud);
    assert!(app.session.dirty);
    let copies = downloads.borrow();
    for ((name, bytes), password) in copies.iter().zip([cloud.as_str(), copy.as_str()]) {
        let (document, encrypted) = io::open_bytes_with(name, bytes, Some(password)).unwrap();
        assert!(encrypted);
        assert_eq!(document.plain_text(StoryRef::Body), "encrypted copy");
    }
}

#[test]
fn changing_the_password_during_save_keeps_the_document_open_and_unsaved() {
    let (mut app, requests, _, _) = app();
    let (save, reply) = start_save(&mut app, &requests);
    assert_eq!(app.run("file.close", json!({})).unwrap()["pending"], "saveChanges");
    app.run("ui.saveChanges", json!({"answer":"save"})).unwrap();
    let edit_revision = app.session.rev();
    app.run("file.encrypt", json!({"password":test_passphrase("changed during save")})).unwrap();
    assert_eq!(app.session.rev(), edit_revision, "encryption changes are outside the edit revision");
    saved(save, reply);
    app.settle_cloud();
    assert!(app.has_document());
    assert!(app.session.dirty);
    assert!(matches!(app.dialog, Some(Dialog::SaveChanges { .. })));
}

#[test]
fn changing_the_password_during_open_requires_new_discard_consent() {
    let (mut app, requests, _, _) = app();
    list(&mut app, &requests, entry("other", 1));
    let document = app.session.document_id();
    let edit_revision = app.session.rev();
    app.run("ui.cloudOpen", json!({"id":"other"})).unwrap();
    let (_, reply) = next(&requests);
    app.run("file.encrypt", json!({"password":test_passphrase("changed during open")})).unwrap();
    assert_eq!(app.session.rev(), edit_revision);
    reply.send(Ok(CloudResponse::Open { revision: 1, bytes: docx("other") })).unwrap();
    app.settle_cloud();
    assert_eq!(app.session.document_id(), document);
    assert!(app.session.dirty);
    assert!(matches!(app.dialog, Some(Dialog::SaveChanges { .. })));
}

#[test]
fn ordinary_save_stays_local_while_the_first_cloud_save_is_pending() {
    let (mut app, requests, downloads, _) = app();
    edit(&mut app, "local download");
    let (save, reply) = start_save(&mut app, &requests);
    app.run("file.save", json!({})).unwrap();
    assert_eq!(downloads.borrow().len(), 1);
    assert!(requests.try_recv().is_err());
    assert!(app.document_unsaved());
    saved(save, reply);
    app.settle_cloud();
    assert!(app.cloud.bound(app.session.document_id()));
}

#[test]
fn a_password_on_a_duplicate_bound_save_is_reported_instead_of_ignored() {
    let (mut app, requests, _, _) = app();
    let (save, reply) = start_save(&mut app, &requests);
    saved(save, reply);
    app.settle_cloud();
    edit(&mut app, "pending");
    app.run("file.save", json!({})).unwrap();
    let (save, reply) = next(&requests);
    assert!(app.run("file.save", json!({"password":test_passphrase("duplicate save")})).unwrap_err().contains("pending"));
    assert!(requests.try_recv().is_err());
    assert!(app.session.password.is_none());
    saved(save, reply);
    app.settle_cloud();
    assert!(!app.session.dirty);
}

#[test]
fn saving_before_apply_cannot_reopen_an_older_revision_of_the_same_cloud_document() {
    let (mut app, requests, _, _) = app();
    let (save, reply) = start_save(&mut app, &requests);
    saved(save, reply);
    app.settle_cloud();
    list(&mut app, &requests, entry("cloud-1", 1));
    let local = app.session.document_id();
    app.run("ui.cloudOpen", json!({"id":"cloud-1"})).unwrap();
    let (_, reply) = next(&requests);
    edit(&mut app, "must survive the older response");
    reply.send(Ok(CloudResponse::Open { revision: 1, bytes: docx("old bytes") })).unwrap();
    app.settle_cloud();
    assert!(matches!(app.dialog, Some(Dialog::SaveChanges { .. })));
    app.run("ui.saveChanges", json!({"answer":"save"})).unwrap();
    let (save, reply) = next(&requests);
    assert!(matches!(save, CloudRequest::Save { base_revision: Some(1), .. }));
    saved(save, reply);
    app.settle_cloud();
    assert_eq!(app.session.document_id(), local);
    assert_eq!(app.session.doc.plain_text(StoryRef::Body), "must survive the older response");
    assert!(!app.session.dirty);
    app.run("file.save", json!({})).unwrap();
    assert!(matches!(next(&requests).0, CloudRequest::Save { base_revision: Some(2), .. }));
}

#[test]
fn late_cloud_open_resumes_after_the_save_changes_dialog_uses_a_local_picker() {
    use crate::async_saves::{AsyncSaves, SaveOutcome};
    use egui_kittest::kittest::Queryable;

    let (mut app, requests, _, _) = app();
    let saves = AsyncSaves::default();
    let ticket = Rc::new(Cell::new(None));
    app.services.async_saves = Some(saves.clone());
    app.services.download = Some(Box::new({
        let saves = saves.clone();
        let ticket = Rc::clone(&ticket);
        move |_, _| {
            ticket.set(Some(saves.start()));
            Ok(())
        }
    }));
    list(&mut app, &requests, entry("other", 1));
    app.run("ui.cloudOpen", json!({"id":"other"})).unwrap();
    let (_, reply) = next(&requests);
    edit(&mut app, "edited while opening");
    reply.send(Ok(CloudResponse::Open { revision: 1, bytes: docx("cloud copy") })).unwrap();
    app.settle_cloud();
    assert!(matches!(app.dialog, Some(Dialog::SaveChanges { .. })));
    let mut installed_fonts = false;
    let mut ui = egui_kittest::Harness::builder().build_ui_state(
        move |ui, app: &mut WordApp| {
            if installed_fonts {
                crate::dialogs::show(app, ui.ctx());
            } else {
                crate::theme::install_fonts_for(ui.ctx(), false);
                installed_fonts = true;
            }
        },
        app,
    );
    for _ in 0..3 {
        ui.step();
    }
    ui.get_by_label("Save").click();
    ui.step();
    saves.finish(ticket.get().unwrap(), SaveOutcome::Saved);
    ui.state_mut().settle_async_saves();
    assert_eq!(ui.state().session.doc.plain_text(StoryRef::Body), "cloud copy");
    assert!(ui.state().cloud.bound(ui.state().session.document_id()));
}

#[test]
fn first_cloud_save_refuses_an_imported_macro_document_and_keeps_it_dirty() {
    let (mut app, requests, _, _) = app();
    app.run("file.open", json!({"path":"macro.docm","data":wordcraft_engine::cmd::insert::base64_encode(&docx("macro file"))})).unwrap();
    edit(&mut app, "local edit");
    let error = app.run("ui.cloudSave", json!({"name":"Cloud document.docx"})).unwrap_err();
    assert!(error.contains("DOCX"));
    assert!(app.session.dirty);
    assert_eq!(app.session.path.as_deref(), Some(std::path::Path::new("macro.docm")));
    assert!(requests.try_recv().is_err());
}

#[test]
fn cloud_open_rejects_non_docx_bytes_without_replacing_the_editor() {
    let (mut app, requests, _, _) = app();
    let original = app.session.document_id();
    list(&mut app, &requests, entry("invalid", 1));
    app.run("ui.cloudOpen", json!({"id":"invalid"})).unwrap();
    let (_, reply) = next(&requests);
    reply.send(Ok(CloudResponse::Open { revision: 1, bytes: br"{\rtf1\ansi This is RTF.}".to_vec() })).unwrap();
    app.settle_cloud();
    assert_eq!(app.session.document_id(), original);
    assert!(!app.cloud.bound(original));
    assert!(app.status_msg.is_some());
}

fn answer_open(requests: &Requests, password: &str) {
    let (_, reply) = next(requests);
    let bytes = io::save_bytes_with("encrypted.docx", &Document::from_text("encrypted cloud document"), Some(password), &Default::default()).unwrap();
    reply.send(Ok(CloudResponse::Open { revision: 2, bytes })).unwrap();
}

#[test]
fn a_scripts_open_of_an_encrypted_document_opens_no_dialog_and_takes_the_password_by_command() {
    let (mut app, requests, _, _) = app();
    list(&mut app, &requests, entry("encrypted", 1));
    app.execute("ui.cloudOpen", json!({"id":"encrypted"})).unwrap();
    let correct = test_passphrase("encrypted document");
    answer_open(&requests, &correct);
    app.settle_cloud();
    assert!(app.dialog.is_none());
    let error = app.execute("ui.cloudApply", json!({})).unwrap_err();
    assert!(io::needs_password(&error), "{error}");
    assert!(app.dialog.is_none());
    app.execute("ui.cloudApply", json!({"password":correct})).unwrap();
    assert_eq!(app.session.doc.plain_text(StoryRef::Body), "encrypted cloud document");
}

#[test]
fn a_scripts_open_overtaken_by_an_edit_opens_no_dialog_and_keeps_the_edit() {
    let (mut app, requests, _, _) = app();
    list(&mut app, &requests, entry("other", 1));
    app.execute("ui.cloudOpen", json!({"id":"other"})).unwrap();
    let (_, reply) = next(&requests);
    edit(&mut app, "typed while it loaded");
    reply.send(Ok(CloudResponse::Open { revision: 1, bytes: docx("other") })).unwrap();
    app.settle_cloud();
    assert!(app.dialog.is_none());
    assert!(app.session.doc.plain_text(StoryRef::Body).contains("typed while it loaded"));
}

#[test]
fn a_persons_open_still_asks_for_the_password_after_answering_save_changes() {
    let (mut app, requests, _, _) = app();
    list(&mut app, &requests, entry("encrypted", 1));
    edit(&mut app, "unsaved");
    assert_eq!(app.run("ui.cloudOpen", json!({"id":"encrypted"})).unwrap()["pending"], "saveChanges");
    app.run("ui.saveChanges", json!({"answer":"dontSave"})).unwrap();
    answer_open(&requests, &test_passphrase("encrypted document"));
    app.settle_cloud();
    assert!(matches!(app.dialog, Some(Dialog::Password { .. })));
}

#[test]
fn cloud_open_prompts_for_the_docx_password_and_the_next_save_keeps_it() {
    let (mut app, requests, _, _) = app();
    let original = app.session.document_id();
    list(&mut app, &requests, entry("encrypted", 1));
    app.run("ui.cloudOpen", json!({"id":"encrypted"})).unwrap();
    let (_, reply) = next(&requests);
    let correct = test_passphrase("encrypted document");
    let bytes = io::save_bytes_with("encrypted.docx", &Document::from_text("encrypted cloud document"), Some(&correct), &Default::default()).unwrap();
    reply.send(Ok(CloudResponse::Open { revision: 2, bytes })).unwrap();
    app.settle_cloud();
    assert_eq!(app.session.document_id(), original);
    assert!(matches!(app.dialog, Some(Dialog::Password { .. })));
    assert!(app.execute("ui.cloudApply", json!({"password":test_passphrase("incorrect")})).is_err());
    assert_eq!(app.session.document_id(), original);
    app.execute("ui.cloudApply", json!({"password":correct})).unwrap();
    assert_eq!(app.session.doc.plain_text(StoryRef::Body), "encrypted cloud document");
    app.run("file.save", json!({})).unwrap();
    let (save, _) = next(&requests);
    let CloudRequest::Save { id: Some(id), base_revision: Some(2), bytes, .. } = save else { panic!("expected a bound save") };
    assert_eq!(id, "encrypted");
    let (_, encrypted) = io::open_bytes_with("encrypted.docx", &bytes, Some(&correct)).unwrap();
    assert!(encrypted);
}

#[test]
fn an_old_picker_failure_keeps_the_newer_cloud_open_and_the_unsaved_buffer() {
    use crate::async_saves::{AsyncSaves, SaveOutcome};

    for late_prompt in [false, true] {
        let (mut app, requests, _, _) = app();
        let saves = AsyncSaves::default();
        let ticket = Rc::new(Cell::new(None));
        app.services.async_saves = Some(saves.clone());
        app.services.download = Some(Box::new({
            let saves = saves.clone();
            let ticket = Rc::clone(&ticket);
            move |_, _| {
                ticket.set(Some(saves.start()));
                Ok(())
            }
        }));
        let (request, reply) = {
            app.run("ui.cloudRefresh", json!({})).unwrap();
            next(&requests)
        };
        assert_eq!(request, CloudRequest::List { cursor: None });
        reply.send(Ok(CloudResponse::List { documents: vec![entry("old", 1), entry("new", 1)], next_cursor: None })).unwrap();
        app.settle_cloud();

        if !late_prompt {
            edit(&mut app, "unsaved work");
        }
        app.run("ui.cloudOpen", json!({"id":"old"})).unwrap();
        if late_prompt {
            let (_, reply) = next(&requests);
            edit(&mut app, "unsaved work");
            reply.send(Ok(CloudResponse::Open { revision: 1, bytes: docx("older open") })).unwrap();
            app.settle_cloud();
        }
        app.run("ui.saveChanges", json!({"answer":"save"})).unwrap();
        assert!(!app.session.dirty, "the local picker provisionally marks its snapshot saved");

        app.run("ui.cloudOpen", json!({"id":"new"})).unwrap();
        app.run("ui.saveChanges", json!({"answer":"dontSave"})).unwrap();
        let (request, reply) = next(&requests);
        assert_eq!(request, CloudRequest::Open { id: "new".into() });

        saves.finish(ticket.get().unwrap(), SaveOutcome::Failed("test write error".into()));
        app.settle_async_saves();
        assert!(app.session.dirty, "the original picker failure still restores dirty state");
        assert!(app.dialog.is_none(), "the old navigation cannot prompt again");
        reply.send(Ok(CloudResponse::Open { revision: 1, bytes: docx("newer open") })).unwrap();
        app.settle_cloud();
        assert_eq!(app.session.doc.plain_text(StoryRef::Body), "newer open");
    }
}

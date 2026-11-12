use super::*;
use crate::cloud::{
    RoomChange,
    tests::{Requests, app, edit, entry, list, next},
};
use egui_kittest::kittest::Queryable;
use wordcraft_doc::{ChangeSet, Document, Pos, StoryRef};

const ID: &str = "abcdef0123456789abcdef0123456789";

fn share_ui(app: WordApp) -> egui_kittest::Harness<'static, WordApp> {
    let mut fonts_installed = false;
    let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(1100.0, 780.0)).build_ui_state(
        move |ui, app: &mut WordApp| {
            if fonts_installed {
                crate::backstage::show(app, ui);
            } else {
                crate::theme::install_fonts_for(ui.ctx(), false);
                fonts_installed = true;
            }
        },
        app,
    );
    harness.step();
    harness
}

fn metadata(checkpoint_revision: u64, head: u64, role: RoomRole) -> CloudResponse {
    CloudResponse::RoomMetadata(RoomMetadata { id: ID.into(), file_revision: 1, published_room_revision: 0, checkpoint_revision, head, role })
}

fn checkpoint(text: &str) -> (Vec<u8>, Document) {
    let mut doc = Document::from_text(text);
    doc.ensure_block_ids(None);
    let bytes = wordcraft_engine::io::save_bytes("fixture.docx", &doc).unwrap();
    let canonical = wordcraft_engine::io::open_room_checkpoint(&bytes).unwrap();
    assert_eq!(canonical.block_ids(), doc.block_ids());
    (bytes, canonical)
}

fn insert(doc: &mut Document, offset: usize, text: &str) -> ChangeSet {
    let before = doc.clone();
    doc.insert_text(&Pos::body(0, offset), text, &Default::default()).unwrap();
    ChangeSet::between(&before, doc)
}

fn page(head: u64, next_after: Option<u64>, role: RoomRole, changes: Vec<RoomChange>) -> CloudResponse {
    CloudResponse::RoomChanges(RoomChanges { head, next_after, role, changes })
}

fn entry_at(revision: u64, operation_id: &str, change: ChangeSet) -> RoomChange {
    RoomChange { revision, operation_id: operation_id.into(), change }
}

fn checkpoint_reply(app: &mut WordApp, requests: &Requests, revision: u64, bytes: Vec<u8>) {
    let (request, reply) = next(requests);
    assert_eq!(request, CloudRequest::RoomCheckpoint { id: ID.into() });
    reply.send(Ok(CloudResponse::RoomCheckpoint { revision, bytes })).unwrap();
    app.settle_cloud();
}

#[test]
fn cloud_open_replays_a_captured_head_then_retries_its_local_edit_without_echo() {
    let (mut app, requests, _, _) = app();
    list(&mut app, &requests, entry(ID, 1));
    app.run("ui.cloudOpen", json!({"id": ID})).unwrap();
    let (request, reply) = next(&requests);
    assert_eq!(request, CloudRequest::OpenEditor { id: ID.into() });
    reply.send(Err(CloudFailure { kind: CloudFailureKind::RoomRequired, message: "room required".into() })).unwrap();
    app.settle_cloud();
    let (request, reply) = next(&requests);
    assert_eq!(request, CloudRequest::RoomMetadata { id: ID.into() });
    reply.send(Ok(metadata(2, 4, RoomRole::Editor))).unwrap();
    app.settle_cloud();

    let (bytes, mut doc) = checkpoint("abcd");
    let first = insert(&mut doc, 0, "A");
    let second = insert(&mut doc, 5, "Z");
    checkpoint_reply(&mut app, &requests, 2, bytes);
    let (request, reply) = next(&requests);
    assert_eq!(request, CloudRequest::RoomChanges { id: ID.into(), after: 2, through: Some(4) });
    reply.send(Ok(page(4, Some(3), RoomRole::Editor, vec![entry_at(3, "other-1", first)]))).unwrap();
    app.settle_cloud();
    let (request, reply) = next(&requests);
    assert_eq!(request, CloudRequest::RoomChanges { id: ID.into(), after: 3, through: Some(4) });
    reply.send(Ok(page(4, None, RoomRole::Editor, vec![entry_at(4, "other-2", second)]))).unwrap();
    app.settle_cloud();

    assert_eq!(app.session.doc.plain_text(StoryRef::Body), "AabcdZ");
    assert!(app.hosted_room_open());
    assert!(!app.session.dirty);
    assert!(app.run("file.save", json!({})).is_err());
    edit(&mut app, "local");
    assert_eq!(app.session.doc.plain_text(StoryRef::Body), "localAabcdZ");

    app.settle_cloud_room(1000.0);
    let (original, reply) = next(&requests);
    let CloudRequest::RoomSubmit { id, proposal } = &original else { panic!("expected room submit") };
    assert_eq!(id, ID);
    assert_eq!(proposal.base_revision, 4);
    app.settle_cloud_room(1200.0);
    assert!(requests.try_recv().is_err());
    drop(reply);
    app.settle_cloud_room(1300.0);
    app.settle_cloud_room(1650.0);
    let (retry, reply) = next(&requests);
    assert_eq!(retry, original);
    reply.send(Ok(CloudResponse::RoomSubmitted { revision: 5, head: 6 })).unwrap();
    app.settle_cloud_room(1700.0);
    let (request, reply) = next(&requests);
    assert_eq!(request, CloudRequest::RoomChanges { id: ID.into(), after: 4, through: Some(6) });
    let own: ChangeSet = serde_json::from_slice(&proposal.change_json).unwrap();
    doc.apply(&own).unwrap();
    let other = insert(&mut doc, 11, "!");
    reply.send(Ok(page(6, None, RoomRole::Editor, vec![entry_at(5, &proposal.operation_id, own), entry_at(6, "other-3", other)]))).unwrap();
    app.settle_cloud_room(1750.0);
    assert_eq!(app.session.doc.plain_text(StoryRef::Body), "localAabcdZ!");
    assert!(!app.session.dirty);
    assert!(requests.try_recv().is_err());

    app.settle_cloud_room(2100.0);
    let (request, _) = next(&requests);
    assert_eq!(request, CloudRequest::RoomChanges { id: ID.into(), after: 6, through: None });
}

#[test]
fn share_activates_only_the_saved_cloud_revision() {
    let (mut app, requests, _, _) = app();
    assert!(app.run("ui.cloudActivateRoom", json!({})).is_err());
    app.run("ui.cloudSave", json!({"name":"Test.docx"})).unwrap();
    let (request, reply) = next(&requests);
    let CloudRequest::Save { bytes, .. } = request else { panic!("expected first cloud save") };
    reply.send(Ok(CloudResponse::Save(CloudEntry { name: "Test.docx".into(), byte_size: bytes.len() as u64, ..entry(ID, 1) }))).unwrap();
    app.settle_cloud();

    app.run("ui.backstage", json!({"value": true, "page": "share"})).unwrap();
    let mut ui = share_ui(app);
    ui.get_by_label("Enable Sharing").click();
    ui.step();
    let app = ui.state_mut();
    let (request, reply) = next(&requests);
    assert_eq!(request, CloudRequest::RoomActivate { id: ID.into(), file_revision: 1 });
    reply.send(Ok(metadata(0, 0, RoomRole::Owner))).unwrap();
    app.settle_cloud();
    let (bytes, _) = checkpoint("saved content");
    checkpoint_reply(app, &requests, 0, bytes);
    assert_eq!(app.session.doc.plain_text(StoryRef::Body), "saved content");
    assert!(app.hosted_room_open());
}

#[test]
fn downgrade_and_revocation_stop_writes_and_keep_a_downloadable_copy() {
    let (mut app, requests, downloads, _) = app();
    app.run("ui.cloudOpenRoom", json!({"id": ID})).unwrap();
    let (request, reply) = next(&requests);
    assert_eq!(request, CloudRequest::RoomMetadata { id: ID.into() });
    reply.send(Ok(metadata(0, 0, RoomRole::Editor))).unwrap();
    app.settle_cloud();
    let (bytes, _) = checkpoint("preserved text");
    checkpoint_reply(&mut app, &requests, 0, bytes);

    app.settle_cloud_room(1000.0);
    let (request, reply) = next(&requests);
    assert_eq!(request, CloudRequest::RoomChanges { id: ID.into(), after: 0, through: None });
    reply.send(Ok(page(0, None, RoomRole::Viewer, vec![]))).unwrap();
    app.settle_cloud_room(1100.0);
    assert!(app.run("text.insert", json!({"text": "must not edit"})).is_err());

    app.settle_cloud_room(1450.0);
    let (_, reply) = next(&requests);
    reply.send(Err(CloudFailure { kind: CloudFailureKind::Missing, message: "room missing".into() })).unwrap();
    app.settle_cloud_room(1500.0);
    assert!(!app.cloud_room_polling());
    assert!(app.run("text.insert", json!({"text": "must not edit"})).is_err());
    app.run("ui.cloudDownload", json!({})).unwrap();
    let downloaded = downloads.borrow();
    assert_eq!(wordcraft_engine::io::open_docx_bytes(&downloaded[0].1, None).unwrap().0.plain_text(StoryRef::Body), "preserved text");
}

#[test]
fn an_unapproved_link_only_requests_access_after_the_user_asks() {
    let (mut app, requests, _, _) = app();
    app.start_screen();
    app.run("ui.cloudOpenRoom", json!({"id": ID})).unwrap();
    let (_, reply) = next(&requests);
    reply.send(Err(CloudFailure { kind: CloudFailureKind::Missing, message: "room missing".into() })).unwrap();
    app.settle_cloud();
    let (request, reply) = next(&requests);
    assert_eq!(request, CloudRequest::RoomAccess { id: ID.into() });
    reply.send(Ok(CloudResponse::RoomAccess(RoomAccess { state: RoomAccessState::Unrequested, request_code: None }))).unwrap();
    app.settle_cloud();
    assert!(requests.try_recv().is_err());
    assert_eq!(app.ui.backstage_page, "share");

    let mut ui = share_ui(app);
    ui.get_by_label("Request Access").click();
    ui.step();
    let app = ui.state_mut();
    let (request, reply) = next(&requests);
    assert_eq!(request, CloudRequest::RoomRequestAccess { id: ID.into() });
    reply.send(Ok(CloudResponse::RoomAccess(RoomAccess { state: RoomAccessState::Pending, request_code: Some(ID.into()) }))).unwrap();
    app.settle_cloud();
    assert!(requests.try_recv().is_err());
    assert!(!app.hosted_room_open());
}

#[test]
fn owner_save_waits_for_the_journal_then_publishes_that_exact_revision() {
    let (mut app, requests, _, _) = app();
    app.run("ui.cloudOpenRoom", json!({"id": ID})).unwrap();
    let (_, reply) = next(&requests);
    reply.send(Ok(metadata(0, 0, RoomRole::Owner))).unwrap();
    app.settle_cloud();
    let (bytes, _) = checkpoint("saved content");
    checkpoint_reply(&mut app, &requests, 0, bytes);

    edit(&mut app, "local edit ");
    assert_eq!(app.run("file.save", json!({})).unwrap()["pending"], "cloudSave");
    app.settle_cloud_room(1000.0);
    let (request, reply) = next(&requests);
    let CloudRequest::RoomSubmit { proposal, .. } = request else { panic!("expected the pending edit first") };
    assert!(requests.try_recv().is_err());
    reply.send(Ok(CloudResponse::RoomSubmitted { revision: 1, head: 1 })).unwrap();
    app.settle_cloud_room(1050.0);
    let (request, reply) = next(&requests);
    assert_eq!(request, CloudRequest::RoomChanges { id: ID.into(), after: 0, through: Some(1) });
    let own = serde_json::from_slice(&proposal.change_json).unwrap();
    reply.send(Ok(page(1, None, RoomRole::Owner, vec![entry_at(1, &proposal.operation_id, own)]))).unwrap();
    app.settle_cloud_room(1100.0);

    let (publish, reply) = next(&requests);
    assert!(matches!(&publish, CloudRequest::RoomSave { id, room_revision: 1, .. } if id == ID));
    assert!(requests.try_recv().is_err());
    drop(reply);
    app.settle_cloud_room(1150.0);
    app.run("file.save", json!({})).unwrap();
    app.settle_cloud_room(1200.0);
    let (retry, reply) = next(&requests);
    assert_eq!(retry, publish);
    edit(&mut app, "later ");
    reply.send(Ok(CloudResponse::RoomSaved { file_revision: 2, room_revision: 1 })).unwrap();
    app.settle_cloud_room(1250.0);

    assert_eq!(app.session.doc.plain_text(StoryRef::Body), "local edit later saved content");
    assert!(app.session.dirty);
    assert_eq!(app.cloud.documents[&app.session.document_id()].revision, 2);
    assert_eq!(app.cloud.rooms[&app.session.document_id()].published_revision, 1);
}

#[test]
fn a_clash_keeps_the_authors_pending_work_in_the_editor_and_download() {
    let (mut app, requests, downloads, _) = app();
    app.run("ui.cloudOpenRoom", json!({"id": ID})).unwrap();
    let (_, reply) = next(&requests);
    reply.send(Ok(metadata(0, 0, RoomRole::Editor))).unwrap();
    app.settle_cloud();
    let (bytes, mut doc) = checkpoint("abcd");
    checkpoint_reply(&mut app, &requests, 0, bytes);

    app.settle_cloud_room(1000.0);
    let (_, reply) = next(&requests);
    app.run("select.all", json!({})).unwrap();
    edit(&mut app, "Keep my edit");
    let other = insert(&mut doc, 2, "other");
    reply.send(Ok(page(1, None, RoomRole::Editor, vec![entry_at(1, "other-1", other)]))).unwrap();
    app.settle_cloud_room(1050.0);

    assert!(!app.cloud_room_polling());
    assert!(app.session.dirty);
    assert!(app.run("text.insert", json!({"text": "not now"})).is_err());
    assert_eq!(app.session.doc.plain_text(StoryRef::Body), "Keep my edit");
    assert!(requests.try_recv().is_err());
    app.run("ui.cloudDownload", json!({})).unwrap();
    let downloaded = downloads.borrow();
    assert_eq!(wordcraft_engine::io::open_docx_bytes(&downloaded[0].1, None).unwrap().0.plain_text(StoryRef::Body), "Keep my edit");
}

#[test]
fn confirmed_presence_moves_through_local_pending_edits() {
    use crate::cloud::{RoomParticipant, RoomPosition};

    let (mut app, requests, _, _) = app();
    app.run("ui.cloudOpenRoom", json!({"id": ID})).unwrap();
    let (_, reply) = next(&requests);
    reply.send(Ok(metadata(0, 0, RoomRole::Editor))).unwrap();
    app.settle_cloud();
    let (bytes, doc) = checkpoint("abcd");
    checkpoint_reply(&mut app, &requests, 0, bytes);

    app.settle_cloud_room(10_000.0);
    let (request, replay) = next(&requests);
    assert!(matches!(request, CloudRequest::RoomChanges { .. }));
    let (request, heartbeat) = next(&requests);
    let CloudRequest::RoomHeartbeat { session_id, revision: 0, position, .. } = request else { panic!("expected heartbeat") };
    assert_eq!(position.focus, doc.anchor_at(&Pos::body(0, 0)));
    replay.send(Ok(page(0, None, RoomRole::Editor, vec![]))).unwrap();
    heartbeat.send(Ok(CloudResponse::RoomHeartbeat)).unwrap();
    app.settle_cloud_room(10_050.0);

    let (request, presence) = next(&requests);
    assert_eq!(request, CloudRequest::RoomPresence { id: ID.into() });
    let at = doc.anchor_at(&Pos::body(0, 2));
    presence
        .send(Ok(CloudResponse::RoomPresence(vec![
            RoomParticipant { session_id, revision: 0, role: RoomRole::Editor, display_name: None, position },
            RoomParticipant {
                session_id: "peer".into(),
                revision: 0,
                role: RoomRole::Owner,
                display_name: Some("Peer".into()),
                position: RoomPosition { anchor: at, focus: at },
            },
        ])))
        .unwrap();
    app.settle_cloud_room(10_100.0);
    let peers = app.hosted_others().unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].0.at.as_ref().unwrap().focus.off, 2);

    edit(&mut app, "A");
    app.settle_cloud_room(10_150.0);
    assert_eq!(app.hosted_others().unwrap()[0].0.at.as_ref().unwrap().focus.off, 3);
}

#[test]
fn a_viewers_parked_document_stays_read_only_to_an_addressed_command() {
    use crate::control::{ControlRequest, Outcome, handle};

    let (mut app, requests, _, _) = app();
    app.run("ui.cloudOpenRoom", json!({"id": ID})).unwrap();
    let (_, reply) = next(&requests);
    reply.send(Ok(metadata(0, 0, RoomRole::Viewer))).unwrap();
    app.settle_cloud();
    let (bytes, _) = checkpoint("shared text");
    checkpoint_reply(&mut app, &requests, 0, bytes);
    assert_eq!(app.session.why_disabled("text.insert"), Some("a viewer changes nothing"));
    assert!(app.session.why_disabled("caret.docEnd").is_none());
    let shared = app.session.document_id();
    app.run("file.new", json!({})).unwrap();
    assert_ne!(app.session.document_id(), shared);
    edit(&mut app, "local document");

    let (request, _) = ControlRequest::new("engine.execute", json!({"doc": shared, "command": "text.insert", "params": {"text": "not allowed"}}));
    let Outcome::Done(reply) = handle(&mut app, &egui::Context::default(), &request) else { panic!("expected a command reply") };
    assert_eq!(reply["ok"], false, "{reply}");
    app.show_document(shared).unwrap();
    assert_eq!(app.session.doc.plain_text(StoryRef::Body), "shared text");
}

fn room_revision_expired() -> CloudFailure {
    CloudFailure { kind: CloudFailureKind::RoomRevisionExpired, message: "room revision expired".into() }
}

fn open_at_zero(app: &mut WordApp, requests: &Requests, role: RoomRole) {
    app.run("ui.cloudOpenRoom", json!({"id": ID})).unwrap();
    let (request, reply) = next(requests);
    assert_eq!(request, CloudRequest::RoomMetadata { id: ID.into() });
    reply.send(Ok(metadata(0, 0, role))).unwrap();
    app.settle_cloud();
    let (bytes, _) = checkpoint("original");
    checkpoint_reply(app, requests, 0, bytes);
}

#[test]
fn a_clean_room_recovers_in_place_after_compaction() {
    let (mut app, requests, _, _) = app();
    open_at_zero(&mut app, &requests, RoomRole::Editor);
    let document = app.session.document_id();
    app.settle_cloud_room(1000.0);
    let (request, expired) = next(&requests);
    assert_eq!(request, CloudRequest::RoomChanges { id: ID.into(), after: 0, through: None });
    expired.send(Err(room_revision_expired())).unwrap();
    app.settle_cloud_room(1050.0);

    let (request, reply) = requests.try_recv().expect("a clean replica should request current room metadata");
    assert_eq!(request, CloudRequest::RoomMetadata { id: ID.into() });
    assert!(app.run("text.insert", json!({"text": "blocked during reopen"})).is_err());
    reply.send(Ok(metadata(4, 5, RoomRole::Editor))).unwrap();
    app.settle_cloud_room(1100.0);
    let (request, reply) = next(&requests);
    assert_eq!(request, CloudRequest::RoomCheckpoint { id: ID.into() });
    let (bytes, mut doc) = checkpoint("compacted");
    reply.send(Ok(CloudResponse::RoomCheckpoint { revision: 4, bytes })).unwrap();
    app.settle_cloud_room(1150.0);

    let (request, reply) = next(&requests);
    assert_eq!(request, CloudRequest::RoomChanges { id: ID.into(), after: 4, through: Some(5) });
    let change = insert(&mut doc, 9, "!");
    reply.send(Ok(page(5, None, RoomRole::Viewer, vec![entry_at(5, "peer", change)]))).unwrap();
    app.settle_cloud_room(1200.0);

    assert_eq!(app.session.document_id(), document);
    assert_eq!(app.session.doc.plain_text(StoryRef::Body), "compacted!");
    assert!(app.cloud_room_polling());
    assert!(!app.session.dirty);
    assert!(app.run("text.insert", json!({"text": "viewer cannot edit"})).is_err());

    app.settle_cloud_room(1550.0);
    let (request, _) = next(&requests);
    assert_eq!(request, CloudRequest::RoomChanges { id: ID.into(), after: 5, through: None });
}

#[test]
fn a_compacted_cursor_preserves_edits_entered_while_the_poll_was_pending() {
    let (mut app, requests, downloads, _) = app();
    open_at_zero(&mut app, &requests, RoomRole::Editor);
    app.settle_cloud_room(1000.0);
    let (_, expired) = next(&requests);
    edit(&mut app, "my edits ");
    expired.send(Err(room_revision_expired())).unwrap();
    app.settle_cloud_room(1050.0);

    assert!(!app.cloud_room_polling());
    assert!(app.session.dirty);
    assert!(requests.try_recv().is_err());
    app.run("ui.cloudDownload", json!({})).unwrap();
    {
        let downloaded = downloads.borrow();
        let doc = wordcraft_engine::io::open_docx_bytes(&downloaded[0].1, None).unwrap().0;
        assert_eq!(doc.plain_text(StoryRef::Body), "my edits original");
    }

    assert_eq!(app.run("ui.cloudOpenRoom", json!({"id": ID})).unwrap()["pending"], "saveChanges");
    app.run("ui.saveChanges", json!({"answer": "dontSave"})).unwrap();
    let (request, _) = requests.try_recv().expect("explicit discard should reopen the room");
    assert_eq!(request, CloudRequest::RoomMetadata { id: ID.into() });
}

#[test]
fn recovery_restarts_when_a_new_checkpoint_overtakes_metadata_or_replay() {
    let (mut app, requests, _, _) = app();
    open_at_zero(&mut app, &requests, RoomRole::Editor);
    app.settle_cloud_room(1000.0);
    let (_, expired) = next(&requests);
    expired.send(Err(room_revision_expired())).unwrap();
    app.settle_cloud_room(1050.0);

    for (metadata_revision, checkpoint_revision) in [(4, 5), (5, 5), (6, 6)] {
        let (request, reply) = next(&requests);
        assert_eq!(request, CloudRequest::RoomMetadata { id: ID.into() });
        reply.send(Ok(metadata(metadata_revision, metadata_revision, RoomRole::Editor))).unwrap();
        app.settle_cloud_room(1100.0);
        let (request, reply) = next(&requests);
        assert_eq!(request, CloudRequest::RoomCheckpoint { id: ID.into() });
        let (bytes, _) = checkpoint("new checkpoint");
        reply.send(Ok(CloudResponse::RoomCheckpoint { revision: checkpoint_revision, bytes })).unwrap();
        app.settle_cloud_room(1150.0);
        if metadata_revision == checkpoint_revision {
            let (request, reply) = next(&requests);
            assert_eq!(request, CloudRequest::RoomChanges { id: ID.into(), after: checkpoint_revision, through: Some(metadata_revision) });
            reply.send(Err(room_revision_expired())).unwrap();
            app.settle_cloud_room(1200.0);
        }
        assert_eq!(app.session.doc.plain_text(StoryRef::Body), "original");
    }
    assert!(!app.cloud_room_polling());
    assert!(requests.try_recv().is_err());
}

#[test]
fn an_expired_operation_receipt_keeps_the_pending_submission_for_explicit_recovery() {
    let (mut app, requests, downloads, _) = app();
    open_at_zero(&mut app, &requests, RoomRole::Editor);
    edit(&mut app, "uncertain ");
    app.settle_cloud_room(1000.0);
    let (request, reply) = next(&requests);
    let CloudRequest::RoomSubmit { proposal, .. } = request else { panic!("expected a submitted edit") };
    reply.send(Err(CloudFailure { kind: CloudFailureKind::Recovery, message: "receipt expired".into() })).unwrap();
    app.settle_cloud_room(1050.0);

    let room = &app.cloud.rooms[&app.session.document_id()];
    assert_eq!(room.replica.in_flight(), Some(&proposal));
    assert!(!app.cloud_room_polling());
    assert!(requests.try_recv().is_err());
    app.run("ui.cloudDownload", json!({})).unwrap();
    let downloaded = downloads.borrow();
    let doc = wordcraft_engine::io::open_docx_bytes(&downloaded[0].1, None).unwrap().0;
    assert_eq!(doc.plain_text(StoryRef::Body), "uncertain original");
}

#[test]
fn a_publish_waiting_for_checkpoint_recovery_uses_its_confirmed_head() {
    let (mut app, requests, _, _) = app();
    open_at_zero(&mut app, &requests, RoomRole::Owner);
    app.settle_cloud_room(1000.0);
    let (_, expired) = next(&requests);
    app.run("file.save", json!({})).unwrap();
    expired.send(Err(room_revision_expired())).unwrap();
    app.settle_cloud_room(1050.0);

    let (request, reply) = next(&requests);
    assert_eq!(request, CloudRequest::RoomMetadata { id: ID.into() });
    assert!(requests.try_recv().is_err());
    reply.send(Ok(metadata(4, 4, RoomRole::Owner))).unwrap();
    app.settle_cloud_room(1100.0);
    let (request, reply) = next(&requests);
    assert_eq!(request, CloudRequest::RoomCheckpoint { id: ID.into() });
    let (bytes, _) = checkpoint("new checkpoint");
    reply.send(Ok(CloudResponse::RoomCheckpoint { revision: 4, bytes })).unwrap();
    app.settle_cloud_room(1150.0);
    let (request, reply) = next(&requests);
    assert_eq!(request, CloudRequest::RoomChanges { id: ID.into(), after: 4, through: Some(4) });
    assert!(requests.try_recv().is_err());
    reply.send(Ok(page(4, None, RoomRole::Owner, vec![]))).unwrap();
    app.settle_cloud_room(1200.0);

    let (request, reply) = next(&requests);
    assert!(matches!(request, CloudRequest::RoomSave { id, room_revision: 4, .. } if id == ID));
    reply.send(Ok(CloudResponse::RoomSaved { file_revision: 2, room_revision: 4 })).unwrap();
    app.settle_cloud_room(1250.0);
    assert_eq!(app.cloud.documents[&app.session.document_id()].revision, 2);
}

#[test]
fn checkpoint_recovery_retries_transport_and_authentication_failures() {
    let (mut app, requests, _, _) = app();
    open_at_zero(&mut app, &requests, RoomRole::Editor);
    app.settle_cloud_room(1000.0);
    let (_, expired) = next(&requests);
    expired.send(Err(room_revision_expired())).unwrap();
    app.settle_cloud_room(1050.0);

    let (metadata_request, reply) = next(&requests);
    reply.send(Err(CloudFailure { kind: CloudFailureKind::Retryable, message: "network".into() })).unwrap();
    app.settle_cloud_room(1100.0);
    assert!(app.cloud_room_polling(), "a network interruption must leave recovery active");
    assert!(app.run("text.insert", json!({"text": "during retry"})).is_err());
    app.settle_cloud_room(1449.0);
    assert!(requests.try_recv().is_err());
    app.settle_cloud_room(1450.0);
    let (request, reply) = next(&requests);
    assert_eq!(request, metadata_request);
    reply.send(Ok(metadata(4, 4, RoomRole::Editor))).unwrap();
    app.settle_cloud_room(1500.0);
    let (request, reply) = next(&requests);
    assert_eq!(request, CloudRequest::RoomCheckpoint { id: ID.into() });
    let (bytes, _) = checkpoint("recovered");
    reply.send(Ok(CloudResponse::RoomCheckpoint { revision: 4, bytes })).unwrap();
    app.settle_cloud_room(1550.0);

    let (replay, reply) = next(&requests);
    reply.send(Err(CloudFailure { kind: CloudFailureKind::Authentication, message: "sign in".into() })).unwrap();
    app.settle_cloud_room(1600.0);
    assert!(app.cloud_room_polling());
    app.settle_cloud_room(1949.0);
    assert!(requests.try_recv().is_err());
    app.settle_cloud_room(1950.0);
    let (request, reply) = next(&requests);
    assert_eq!(request, replay);
    reply.send(Ok(page(4, None, RoomRole::Editor, vec![]))).unwrap();
    app.settle_cloud_room(2000.0);
    assert_eq!(app.session.doc.plain_text(StoryRef::Body), "recovered");
    assert!(app.run("text.insert", json!({"text": "after recovery"})).is_ok());
}

#[test]
fn an_expired_cursor_waits_for_the_input_method_before_reopening() {
    for commit in [true, false] {
        let (mut app, requests, _, _) = app();
        open_at_zero(&mut app, &requests, RoomRole::Editor);
        app.settle_cloud_room(1000.0);
        let (_, expired) = next(&requests);
        app.canvas.ime_preedit = "にほん".into();
        expired.send(Err(room_revision_expired())).unwrap();
        app.settle_cloud_room(1050.0);
        assert_eq!(app.hosted_room_read_only(app.session.document_id()), Some(false), "the IME must still be able to commit");
        assert!(requests.try_recv().is_err());

        app.canvas.ime_preedit.clear();
        if commit {
            edit(&mut app, "日本 ");
        }
        app.settle_cloud_room(1100.0);
        if commit {
            assert!(!app.cloud_room_polling());
            assert_eq!(app.session.doc.plain_text(StoryRef::Body), "日本 original");
            assert!(requests.try_recv().is_err(), "the edit must remain local");
        } else {
            let (request, _) = next(&requests);
            assert_eq!(request, CloudRequest::RoomMetadata { id: ID.into() });
            assert_eq!(app.session.doc.plain_text(StoryRef::Body), "original");
        }
    }
}

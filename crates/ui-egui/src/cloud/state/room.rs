//! Hosted room loading and synchronization. The app keeps its normal editing Session.

mod grants;
mod presence;
mod save;
use grants::{GrantAction, OwnerGrants};
use presence::RoomPresence;
use save::Publish;

use crate::cloud::{CloudEntry, RoomAccess, RoomAccessState, RoomChanges, RoomGrantedRole, RoomMetadata, RoomRole};
use serde_json::{Value, json};
use wordcraft_engine::{
    Session,
    room::{Role, remote::RemoteReplica},
};

use super::{
    CloudFailure, CloudFailureKind, CloudReply, CloudRequest, CloudResponse, CloudService, DocumentSnapshot, MAX_BYTES, OpenResult, Opening, WordApp,
    poll, unexpected,
};

const POLL_MS: f64 = 350.0;

pub(super) enum HostedOpen {
    Metadata(CloudReply),
    Checkpoint(RoomMetadata, CloudReply),
    Replay(RoomMetadata, Box<RemoteReplica>, CloudReply),
}

impl HostedOpen {
    pub(super) fn reply(&self) -> &CloudReply {
        match self {
            Self::Metadata(reply) | Self::Checkpoint(_, reply) | Self::Replay(_, _, reply) => reply,
        }
    }

    pub(super) fn advance(self, response: CloudResponse, service: &CloudService, id: &str) -> Result<OpenResult, String> {
        match (self, response) {
            (Self::Metadata(_), CloudResponse::RoomMetadata(meta))
                if meta.id == id && meta.file_revision > 0 && meta.checkpoint_revision <= meta.head =>
            {
                let reply = (service.request)(CloudRequest::RoomCheckpoint { id: id.into() });
                Ok(OpenResult::Hosted(Self::Checkpoint(meta, reply)))
            }
            (Self::Checkpoint(meta, _), CloudResponse::RoomCheckpoint { revision, bytes })
                if revision >= meta.checkpoint_revision && !bytes.is_empty() && bytes.len() <= MAX_BYTES =>
            {
                if revision > meta.head {
                    // Compaction won the race with the metadata read. Capture its new head.
                    let reply = (service.request)(CloudRequest::RoomMetadata { id: id.into() });
                    return Ok(OpenResult::Hosted(Self::Metadata(reply)));
                }
                let doc = wordcraft_engine::io::open_room_checkpoint(&bytes)?;
                Ok(Self::replay(meta, Box::new(RemoteReplica::new(revision, doc)), service))
            }
            (Self::Replay(mut meta, mut replica, _), CloudResponse::RoomChanges(page)) if valid_page(&page, replica.revision(), Some(meta.head)) => {
                for entry in page.changes {
                    replica.integrate(entry.revision, &entry.operation_id, &entry.change).map_err(|error| error.to_string())?;
                }
                meta.role = page.role;
                Ok(Self::replay(meta, replica, service))
            }
            _ => Err(unexpected().message),
        }
    }

    fn replay(meta: RoomMetadata, replica: Box<RemoteReplica>, service: &CloudService) -> OpenResult {
        if replica.revision() == meta.head {
            OpenResult::HostedReady(meta, replica)
        } else {
            let reply = changes(service, &meta.id, replica.revision(), Some(meta.head));
            OpenResult::Hosted(Self::Replay(meta, replica, reply))
        }
    }
}

fn valid_room_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn changes(service: &CloudService, id: &str, after: u64, through: Option<u64>) -> CloudReply {
    (service.request)(CloudRequest::RoomChanges { id: id.into(), after, through })
}

fn valid_page(page: &RoomChanges, after: u64, through: Option<u64>) -> bool {
    if page.changes.len() > 32 || page.head < after || through.is_some_and(|head| page.head != head) {
        return false;
    }
    let mut last = after;
    for entry in &page.changes {
        if last.checked_add(1) != Some(entry.revision) || entry.revision > page.head {
            return false;
        }
        last = entry.revision;
    }
    if last == page.head { page.next_after.is_none() } else { last > after && page.next_after == Some(last) }
}

enum Exchange {
    Changes(CloudReply),
    Submit(CloudReply),
}

pub(super) struct HostedRoom {
    id: String,
    role: RoomRole,
    replica: Box<RemoteReplica>,
    grants: OwnerGrants,
    presence: RoomPresence,
    published_revision: u64,
    publish: Option<Publish>,
    seen: u64,
    waiting: Option<Exchange>,
    /// The captured page head or the head a durable receipt asks us to read through.
    through: Option<u64>,
    receipt: Option<u64>,
    next_poll: f64,
    check_role: bool,
    message: Option<String>,
}

impl HostedRoom {
    pub(super) fn new(meta: RoomMetadata, replica: Box<RemoteReplica>, session: &Session, now: f64) -> Self {
        Self {
            id: meta.id,
            role: meta.role,
            replica,
            grants: OwnerGrants::default(),
            presence: RoomPresence::new(now),
            published_revision: meta.published_room_revision,
            publish: None,
            seen: session.changes().version(),
            waiting: None,
            through: None,
            receipt: None,
            next_poll: now + POLL_MS,
            check_role: false,
            message: None,
        }
    }

    fn reading_only(&self) -> bool {
        self.role == RoomRole::Viewer || self.check_role || self.replica.needs_recovery()
    }

    fn recover(&mut self, message: &str) {
        self.replica.require_recovery();
        self.waiting = None;
        self.message = Some(message.into());
    }

    fn capture(&mut self, session: &Session) -> Result<(), ()> {
        for (version, change) in session.changes().since(self.seen).map_err(|_| ())? {
            if self.reading_only() {
                return Err(());
            }
            self.replica.queue(&change).map_err(|_| ())?;
            self.seen = version;
        }
        Ok(())
    }

    fn apply_page(&mut self, session: &mut Session, page: RoomChanges) -> Result<(), ()> {
        if !valid_page(&page, self.replica.revision(), self.through) {
            return Err(());
        }
        for entry in page.changes {
            if self.receipt == Some(entry.revision) {
                if self.replica.in_flight().is_none_or(|proposal| proposal.operation_id != entry.operation_id) {
                    return Err(());
                }
                self.receipt = None;
            }
            if let Some(change) = self.replica.integrate(entry.revision, &entry.operation_id, &entry.change).map_err(|_| ())? {
                let dirty = session.dirty;
                session.apply_change(&change).map_err(|_| ())?;
                session.dirty = dirty;
                self.seen = session.changes().version();
            }
        }
        self.role = page.role;
        self.check_role = false;
        self.through = page.next_after.map(|_| page.head);
        if page.next_after.is_none() && self.role == RoomRole::Viewer && self.replica.pending_len() > 0 {
            self.recover(tl!("Editing access changed. Download a copy to keep your edits."));
        } else if self.replica.pending_len() == 0 {
            session.dirty = false;
        }
        Ok(())
    }

    fn tick(&mut self, session: &mut Session, service: &CloudService, now: f64) {
        if self.replica.needs_recovery() {
            return;
        }
        if self.capture(session).is_err() {
            self.recover(tl!("These edits could not be sent to the room. Download a copy to keep your edits."));
            return;
        }
        if let Some(exchange) = &self.waiting {
            let (is_submit, reply) = match exchange {
                Exchange::Changes(reply) => (false, reply),
                Exchange::Submit(reply) => (true, reply),
            };
            let Some(response) = poll(reply) else { return };
            self.waiting = None;
            match response {
                Ok(CloudResponse::RoomSubmitted { revision, head })
                    if is_submit && revision > self.replica.revision() && head >= revision && self.replica.in_flight().is_some() =>
                {
                    self.receipt = Some(revision);
                    self.through = Some(head);
                    self.next_poll = now;
                    self.message = None;
                }
                Ok(CloudResponse::RoomChanges(page)) if !is_submit => {
                    if self.apply_page(session, page).is_err() {
                        self.recover(tl!("The room changed in a way that could not be applied. Download a copy to keep your edits."));
                    }
                    if !self.replica.needs_recovery() {
                        self.message = None;
                    }
                    self.next_poll = if self.through.is_some() || self.replica.pending_len() > 0 { now } else { now + POLL_MS };
                }
                Err(error) => self.exchange_failed(error, now),
                _ => self.exchange_failed(unexpected(), now),
            }
        }
        if self.replica.needs_recovery() || now < self.next_poll {
            return;
        }
        if self.through.is_none() && !self.reading_only() && self.replica.pending_len() > 0 {
            let proposal = if let Some(proposal) = self.replica.in_flight() {
                Some(proposal)
            } else {
                match (service.new_operation_id)() {
                    Ok(id) => match self.replica.prepare(&id) {
                        Ok(proposal) => proposal,
                        Err(_) => {
                            self.recover(tl!("These edits could not be sent to the room. Download a copy to keep your edits."));
                            return;
                        }
                    },
                    Err(error) => {
                        self.exchange_failed(error, now);
                        return;
                    }
                }
            };
            if let Some(proposal) = proposal {
                let reply = (service.request)(CloudRequest::RoomSubmit { id: self.id.clone(), proposal: proposal.clone() });
                self.waiting = Some(Exchange::Submit(reply));
                return;
            }
        }
        self.waiting = Some(Exchange::Changes(changes(service, &self.id, self.replica.revision(), self.through)));
    }

    fn exchange_failed(&mut self, error: CloudFailure, now: f64) {
        self.next_poll = now + POLL_MS;
        match error.kind {
            CloudFailureKind::Retryable | CloudFailureKind::Authentication => {
                self.message = Some(error.message);
            }
            CloudFailureKind::AccessDenied => {
                self.check_role = true;
                self.through = None;
                self.message = Some(error.message);
            }
            CloudFailureKind::Missing => self.recover(tl!("Room access ended. Download a copy to keep your edits.")),
            _ => self.recover(tl!("The room could not accept these edits. Download a copy to keep your edits.")),
        }
    }
}

pub(super) struct RoomLink {
    id: String,
    reply: Option<CloudReply>,
    access: Option<RoomAccess>,
    message: Option<String>,
}

impl WordApp {
    pub(super) fn cloud_open_room(&mut self, params: &Value) -> Result<Value, String> {
        let id = params
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| valid_room_id(id))
            .ok_or_else(|| tl!("The shared document link is invalid.").to_owned())?;
        if (self.document_unsaved() || crate::panes::has_comment_drafts(self)) && params.get("force").and_then(Value::as_bool) != Some(true) {
            return Err(tl!("Save or explicitly discard your changes before opening a cloud document.").into());
        }
        let reply = (self.cloud_service()?.request)(CloudRequest::RoomMetadata { id: id.into() });
        let entry = CloudEntry { id: id.into(), name: tl!("Shared document.docx").into(), revision: 0, byte_size: 0, saved_at: 0 };
        self.cloud.room_link = Some(RoomLink { id: id.into(), reply: None, access: None, message: None });
        self.begin_cloud_open(entry, OpenResult::Hosted(HostedOpen::Metadata(reply)), params);
        Ok(json!({"pending": "cloudOpen"}))
    }

    pub(super) fn cloud_room_access(&mut self, request_access: bool) -> Result<Value, String> {
        let service = self.services.cloud.as_ref().ok_or_else(|| tl!("Cloud documents are unavailable here.").to_owned())?;
        let link = self.cloud.room_link.as_mut().ok_or_else(|| tl!("Open a shared document link first.").to_owned())?;
        if link.reply.is_none() {
            let id = link.id.clone();
            let request = if request_access { CloudRequest::RoomRequestAccess { id } } else { CloudRequest::RoomAccess { id } };
            link.reply = Some((service.request)(request));
            link.message = None;
        }
        Ok(json!({"pending": "roomAccess"}))
    }

    pub(super) fn settle_room_access(&mut self) {
        let Some(link) = self.cloud.room_link.as_mut() else { return };
        let Some(response) = link.reply.as_ref().and_then(poll) else { return };
        link.reply = None;
        match response {
            Ok(CloudResponse::RoomAccess(access)) => link.access = Some(access),
            Ok(_) => link.message = Some(unexpected().message),
            Err(error) => link.message = Some(error.message),
        }
    }

    pub(crate) fn cloud_room_request_pending(&self) -> bool {
        self.cloud.rooms.get(&self.session.document_id()).is_some_and(|room| {
            room.waiting.is_some()
                || room.publish.as_ref().is_some_and(Publish::waiting)
                || (!room.replica.needs_recovery() && room.presence.waiting())
        }) || self.cloud.room_link.as_ref().is_some_and(|link| link.reply.is_some())
    }

    pub(super) fn cloud_manage_room_grants(&mut self, params: Option<&Value>) -> Result<Value, String> {
        let service = self.services.cloud.as_ref().ok_or_else(|| tl!("Cloud documents are unavailable here.").to_owned())?;
        let room = self
            .cloud
            .rooms
            .get_mut(&self.session.document_id())
            .filter(|room| room.role == RoomRole::Owner && !room.replica.needs_recovery())
            .ok_or_else(|| tl!("Only the owner can manage access to this shared document.").to_owned())?;
        let action = match params {
            None => GrantAction::Refresh,
            Some(params) => {
                let id = params
                    .get("grant_id")
                    .and_then(Value::as_str)
                    .filter(|id| valid_room_id(id))
                    .ok_or_else(|| tl!("The request code is invalid.").to_owned())?;
                let role = match params.get("role").and_then(Value::as_str) {
                    Some("editor") => RoomGrantedRole::Editor,
                    Some("viewer") => RoomGrantedRole::Viewer,
                    Some("revoked") => RoomGrantedRole::Revoked,
                    _ => return Err(tl!("Choose a role for the tester.").into()),
                };
                GrantAction::Set { grant_id: id.into(), role }
            }
        };
        room.grants.start(service, &room.id, action)?;
        Ok(json!({"pending": "roomGrants"}))
    }

    pub(super) fn cloud_activate_room(&mut self) -> Result<Value, String> {
        let document = self.session.document_id();
        if self.cloud.rooms.contains_key(&document) || self.cloud.opening.is_some() {
            return Err(tl!("This document is already shared or opening.").into());
        }
        if self.document_unsaved() || crate::panes::has_comment_drafts(self) {
            return Err(tl!("Save this document to Cloud before sharing it.").into());
        }
        let entry = self.cloud.documents.get(&document).cloned().ok_or_else(|| tl!("Save this document to Cloud before sharing it.").to_owned())?;
        let reply = (self.cloud_service()?.request)(CloudRequest::RoomActivate { id: entry.id.clone(), file_revision: entry.revision });
        self.cloud.opening = Some(Opening {
            document,
            snapshot: DocumentSnapshot::capture(&self.session),
            entry,
            result: OpenResult::Hosted(HostedOpen::Metadata(reply)),
            save_pending: false,
            confirmed: true,
            asks: true,
        });
        Ok(json!({ "pending": "cloudOpen" }))
    }

    pub(super) fn settle_cloud_room(&mut self, now: f64) {
        let document = self.session.document_id();
        let Some(service) = &self.services.cloud else { return };
        let Some(room) = self.cloud.rooms.get_mut(&document) else { return };
        let before = room.message.clone();
        room.tick(&mut self.session, service, now);
        let published = room.tick_publish(service, now);
        room.grants.poll();
        if !room.replica.needs_recovery() && !room.check_role {
            room.presence.tick(&room.id, &room.replica, &self.session, service, now);
        }
        let status = room.message.as_ref().filter(|message| Some(*message) != before.as_ref()).cloned();
        self.sync_cloud_room_role();
        if let Some(message) = status {
            self.status(message);
        }
        if let Some(result) = published {
            self.finish_room_publish(document, result);
        }
    }

    pub(crate) fn sync_cloud_room_role(&mut self) {
        if self.services.cloud.is_some() {
            self.session.role = self.hosted_room_read_only(self.session.document_id()).filter(|read_only| *read_only).map(|_| Role::Viewer);
        }
    }

    pub(crate) fn nudge_cloud_room(&mut self) {
        if let Some(room) = self.cloud.rooms.get_mut(&self.session.document_id()) {
            room.next_poll = 0.0;
        }
    }

    pub(crate) fn cloud_room_link_open(&self) -> bool {
        self.cloud.room_link.is_some()
    }

    pub(crate) fn cloud_room_polling(&self) -> bool {
        self.cloud.rooms.get(&self.session.document_id()).is_some_and(|room| !room.replica.needs_recovery())
    }

    pub(crate) fn hosted_room_read_only(&self, document: u64) -> Option<bool> {
        self.cloud.rooms.get(&document).map(HostedRoom::reading_only)
    }

    pub(crate) fn hosted_room_open(&self) -> bool {
        self.cloud.rooms.contains_key(&self.session.document_id())
    }

    pub(crate) fn hosted_others(&self) -> Option<&[(wordcraft_engine::room::Other, f64)]> {
        let room = self.cloud.rooms.get(&self.session.document_id())?;
        Some(if room.replica.needs_recovery() || room.check_role { &[] } else { &room.presence.others })
    }

    pub(crate) fn show_hosted_share(&mut self, ui: &mut egui::Ui) {
        if self.show_hosted_room_status(ui) {
            if let Some(action) = self
                .cloud
                .rooms
                .get_mut(&self.session.document_id())
                .filter(|room| room.role == RoomRole::Owner && !room.replica.needs_recovery())
                .and_then(|room| room.grants.show(ui))
            {
                let (command, params) = action.command();
                let _ = self.run(command, params);
            }
            return;
        }
        if self.cloud.fetching() {
            return;
        }
        if let Some(link) = &self.cloud.room_link {
            let id = link.id.clone();
            let state = link.access.as_ref().map(|access| access.state);
            if let Some(message) = &link.message {
                ui.label(message);
            }
            if link.reply.is_some() {
                ui.spinner();
            } else if state == Some(RoomAccessState::Unrequested) {
                ui.label(tl!("The owner must approve access to this shared document."));
                if ui.button(tl!("Request Access")).clicked() {
                    let _ = self.run("ui.cloudRoomRequestAccess", json!({}));
                }
            } else if matches!(state, Some(RoomAccessState::Owner | RoomAccessState::Editor | RoomAccessState::Viewer)) {
                if ui.button(tl!("Open Shared Document")).clicked() {
                    let _ = self.run("ui.cloudOpenRoom", json!({"id": id}));
                }
            } else {
                if state == Some(RoomAccessState::Pending) {
                    ui.label(tl!("Your access request is waiting for the owner."));
                    if let Some(code) = link.access.as_ref().and_then(|access| access.request_code.as_deref()) {
                        ui.label(tl!("Request code"));
                        ui.monospace(code);
                    }
                } else if state == Some(RoomAccessState::Revoked) {
                    ui.label(tl!("The owner removed your access."));
                }
                if ui.button(tl!("Check Access")).clicked() {
                    let _ = self.run("ui.cloudRoomCheckAccess", json!({}));
                }
            }
        } else if self.cloud.bound(self.session.document_id()) && !self.document_unsaved() {
            if ui.button(tl!("Enable Sharing")).clicked() {
                let _ = self.run("ui.cloudActivateRoom", json!({}));
            }
        } else {
            ui.label(tl!("Save this document to Cloud before sharing it."));
        }
        if let Some((message, _)) = &self.status_msg {
            ui.label(message);
        }
    }

    pub(super) fn show_hosted_room_status(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(room) = self.cloud.rooms.get(&self.session.document_id()) else { return false };
        let recovering = room.replica.needs_recovery();
        let can_publish = room.role == RoomRole::Owner && !recovering;
        let reload = recovering.then(|| room.id.clone());
        if let Some(message) = &room.message {
            ui.label(message);
        } else if room.publishing() {
            ui.label(tl!("Publishing DOCX…"));
        } else if room.role == RoomRole::Viewer {
            ui.label(tl!("Shared document · View only"));
        } else if room.replica.pending_len() > 0 {
            ui.label(tl!("Sending edits to the room…"));
        } else {
            ui.label(tl!("Shared document"));
        }
        if room.role == RoomRole::Owner
            && !room.replica.needs_recovery()
            && let Some(base) = &self.services.cloud_room_base_url
            && ui.button(tl!("Copy Share Link")).clicked()
        {
            ui.ctx().copy_text(format!("{base}?room={}", room.id));
        }
        if can_publish {
            let revision = tl!("Published room revision {revision}").replace("{revision}", &room.published_revision.to_string());
            ui.label(revision);
            if ui.add_enabled(!room.publishing(), egui::Button::new(tl!("Publish DOCX"))).clicked() {
                let _ = self.run("file.save", json!({}));
            }
        }
        if ui.button(tl!("Download DOCX")).clicked() {
            let _ = self.run("ui.cloudDownload", json!({}));
        }
        if let Some(id) = reload
            && ui.button(tl!("Open Latest")).clicked()
        {
            let _ = self.run("ui.cloudOpenRoom", json!({"id": id}));
        }
        true
    }
}

#[cfg(test)]
mod tests;

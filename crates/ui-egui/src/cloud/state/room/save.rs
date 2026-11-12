use super::{
    CloudFailureKind, CloudReply, CloudRequest, CloudResponse, CloudService, DocumentSnapshot, HostedRoom, RoomRole, Value, WordApp, json, poll,
    unexpected,
};

const RETRY_MS: f64 = 2_000.0;

pub(super) enum Publish {
    Queued,
    Sending { request: CloudRequest, reply: CloudReply },
    Retry { request: CloudRequest, at: f64 },
}

impl Publish {
    pub(super) fn waiting(&self) -> bool {
        matches!(self, Self::Sending { .. })
    }
}

impl HostedRoom {
    pub(in crate::cloud::state) fn publishing(&self) -> bool {
        self.publish.is_some()
    }

    pub(super) fn tick_publish(&mut self, service: &CloudService, now: f64) -> Option<Result<u64, String>> {
        let state = self.publish.take()?;
        let request = match state {
            Publish::Sending { request, reply } => match poll(&reply) {
                None => {
                    self.publish = Some(Publish::Sending { request, reply });
                    return None;
                }
                Some(Ok(CloudResponse::RoomSaved { file_revision, room_revision }))
                    if file_revision > 0
                        && matches!(request, CloudRequest::RoomSave { room_revision: expected, .. } if expected == room_revision) =>
                {
                    self.published_revision = self.published_revision.max(room_revision);
                    return Some(Ok(file_revision));
                }
                Some(Err(error)) if matches!(error.kind, CloudFailureKind::Retryable | CloudFailureKind::Authentication) => {
                    self.publish = Some(Publish::Retry { request, at: now + RETRY_MS });
                    return None;
                }
                Some(Err(error)) => return Some(Err(error.message)),
                Some(Ok(_)) => return Some(Err(unexpected().message)),
            },
            state if self.replica.needs_recovery() || self.role != RoomRole::Owner => {
                let _ = state;
                return Some(Err(tl!("The shared document cannot be published while access or edits need recovery.").into()));
            }
            state if self.reopening.is_some() => {
                self.publish = Some(state);
                return None;
            }
            Publish::Queued if self.replica.pending_len() > 0 || self.check_role => {
                self.publish = Some(Publish::Queued);
                return None;
            }
            Publish::Queued => match (service.new_operation_id)() {
                Ok(operation_id) => CloudRequest::RoomSave { id: self.id.clone(), operation_id, room_revision: self.replica.revision() },
                Err(error) => return Some(Err(error.message)),
            },
            Publish::Retry { request, at } if now < at => {
                self.publish = Some(Publish::Retry { request, at });
                return None;
            }
            Publish::Retry { request, .. } => request,
        };
        let reply = (service.request)(request.clone());
        self.publish = Some(Publish::Sending { request, reply });
        None
    }
}

impl WordApp {
    pub(in crate::cloud::state) fn cloud_publish_room(&mut self, params: &Value) -> Result<Value, String> {
        if params.get("password").is_some_and(|password| !password.is_null()) {
            return Err(tl!("Document protection is unavailable while sharing a cloud document.").into());
        }
        let room = self.cloud.rooms.get_mut(&self.session.document_id()).ok_or_else(|| tl!("No shared document is open.").to_owned())?;
        if room.role != RoomRole::Owner || room.replica.needs_recovery() {
            return Err(tl!("The owner can publish a shared document when its edits are synchronized.").into());
        }
        match &mut room.publish {
            None => room.publish = Some(Publish::Queued),
            Some(Publish::Retry { at, .. }) => *at = 0.0,
            _ => {}
        }
        self.report_dirty();
        Ok(json!({"pending": "cloudSave"}))
    }

    pub(super) fn finish_room_publish(&mut self, document: u64, result: Result<u64, String>) {
        let held = self.cloud.held.take_if(|held| held.document == document);
        match result {
            Ok(revision) => {
                if let Some(entry) = self.cloud.documents.get_mut(&document) {
                    entry.revision = entry.revision.max(revision);
                }
                self.cloud.library.refresh_needed = true;
                if let Some(held) = held {
                    if self.document_unsaved() || crate::panes::has_comment_drafts(self) {
                        self.cloud_save_prompt(held.then, held.params, document, tl!("The document has changes made after the save began.").into());
                    } else {
                        if held.then == "ui.cloudApply"
                            && let Some(opening) = &mut self.cloud.opening
                        {
                            opening.snapshot = DocumentSnapshot::capture(&self.session);
                            opening.save_pending = false;
                            opening.confirmed = true;
                        }
                        if let Err(error) = self.execute_held_back(&held.then, held.params) {
                            self.status(error);
                        }
                    }
                }
            }
            Err(message) => {
                self.status(message.clone());
                if let Some(held) = held {
                    self.cloud_save_prompt(held.then, held.params, document, message);
                }
            }
        }
    }
}

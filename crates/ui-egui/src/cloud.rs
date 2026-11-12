//! Optional document storage and shared rooms supplied by the hosted editor.
use serde::{Deserialize, Serialize};
use std::sync::{Arc, mpsc::Receiver};
use wordcraft_doc::{ChangeSet, change::Anchor};
use wordcraft_engine::room::remote::Proposal;

mod state;
pub(crate) use state::{State, show_library, show_save, show_status, show_waiting_navigation};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CloudEntry {
    pub id: String,
    pub name: String,
    pub revision: u64,
    pub byte_size: u64,
    pub saved_at: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CloudRequest {
    List { cursor: Option<String> },
    Open { id: String },
    OpenEditor { id: String },
    Save { operation_id: String, id: Option<String>, base_revision: Option<u64>, name: String, bytes: Arc<[u8]> },
    Delete { id: String, revision: u64 },
    RoomActivate { id: String, file_revision: u64 },
    RoomMetadata { id: String },
    RoomCheckpoint { id: String },
    RoomChanges { id: String, after: u64, through: Option<u64> },
    RoomSubmit { id: String, proposal: Proposal },
    RoomAccess { id: String },
    RoomRequestAccess { id: String },
    RoomGrants { id: String },
    RoomSetRole { id: String, grant_id: String, role: RoomGrantedRole },
    RoomPresence { id: String },
    RoomHeartbeat { id: String, session_id: String, revision: u64, position: RoomPosition },
    RoomSave { id: String, operation_id: String, room_revision: u64 },
}

#[derive(Debug)]
pub enum CloudResponse {
    List { documents: Vec<CloudEntry>, next_cursor: Option<String> },
    Open { revision: u64, bytes: Vec<u8> },
    Save(CloudEntry),
    Delete { cleanup_pending: bool },
    RoomMetadata(RoomMetadata),
    RoomCheckpoint { revision: u64, bytes: Vec<u8> },
    RoomChanges(RoomChanges),
    RoomSubmitted { revision: u64, head: u64 },
    RoomAccess(RoomAccess),
    RoomGrants(Vec<RoomGrant>),
    RoomGrant(RoomGrant),
    RoomPresence(Vec<RoomParticipant>),
    RoomHeartbeat,
    RoomSaved { file_revision: u64, room_revision: u64 },
}

#[derive(Debug, Deserialize)]
pub struct RoomMetadata {
    pub id: String,
    pub file_revision: u64,
    pub checkpoint_revision: u64,
    pub published_room_revision: u64,
    pub head: u64,
    pub role: RoomRole,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RoomRole {
    Owner,
    Editor,
    Viewer,
}

#[derive(Debug, Deserialize)]
pub struct RoomChanges {
    pub head: u64,
    pub role: RoomRole,
    pub changes: Vec<RoomChange>,
    pub next_after: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct RoomChange {
    pub revision: u64,
    pub operation_id: String,
    pub change: ChangeSet,
}

#[derive(Debug, Deserialize)]
pub struct RoomAccess {
    pub state: RoomAccessState,
    pub request_code: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RoomAccessState {
    Owner,
    Unrequested,
    Pending,
    Editor,
    Viewer,
    Revoked,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct RoomGrant {
    pub id: String,
    pub display_name: Option<String>,
    pub role: RoomGrantRole,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RoomGrantRole {
    Pending,
    Editor,
    Viewer,
    Revoked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomGrantedRole {
    Editor,
    Viewer,
    Revoked,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RoomPosition {
    pub anchor: Option<Anchor>,
    pub focus: Option<Anchor>,
}

#[derive(Debug, Deserialize)]
pub struct RoomParticipant {
    pub session_id: String,
    pub role: RoomRole,
    pub display_name: Option<String>,
    pub revision: u64,
    pub position: RoomPosition,
}

#[derive(Debug)]
pub struct CloudFailure {
    pub kind: CloudFailureKind,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloudFailureKind {
    Retryable,
    Authentication,
    Missing,
    Stale,
    Abandoned,
    Invalid,
    RoomRequired,
    Conflict,
    Recovery,
    Capacity,
    AccessDenied,
}

type CloudReply = Receiver<Result<CloudResponse, CloudFailure>>;

pub struct CloudService {
    pub new_operation_id: Box<dyn Fn() -> Result<String, CloudFailure>>,
    pub request: Box<dyn Fn(CloudRequest) -> CloudReply>,
}

#[cfg(test)]
mod tests;

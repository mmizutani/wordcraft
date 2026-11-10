//! Optional document storage supplied by the hosted editor.
use std::sync::{Arc, mpsc::Receiver};

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
    Save { operation_id: String, id: Option<String>, base_revision: Option<u64>, name: String, bytes: Arc<[u8]> },
    Delete { id: String, revision: u64 },
}

#[derive(Debug)]
pub enum CloudResponse {
    List { documents: Vec<CloudEntry>, next_cursor: Option<String> },
    Open { revision: u64, bytes: Vec<u8> },
    Save(CloudEntry),
    Delete { cleanup_pending: bool },
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
}

type CloudReply = Receiver<Result<CloudResponse, CloudFailure>>;

pub struct CloudService {
    pub new_operation_id: Box<dyn Fn() -> Result<String, CloudFailure>>,
    pub request: Box<dyn Fn(CloudRequest) -> CloudReply>,
}

#[cfg(test)]
mod tests;

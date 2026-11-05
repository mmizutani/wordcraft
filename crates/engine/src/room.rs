//! Rooms: one document that people and agents edit at once, each in a copy of their own, in
//! this process.
//!
//! Every participant edits a [`Replica`]: a [`Session`] of their own, whose commands change its
//! copy of the document at once, with no wait for anyone. Its changes go to the room's [`Hub`]
//! with the version of the document they were made on. The hub puts the changes in one order:
//! it accepts a change made on its latest version as it is, moves one made on an earlier version
//! past the changes it accepted since ([`ChangeSet::rebase`]), and sends back one that clashes
//! with one of them (one edits characters, blocks or properties the other edits or takes away).
//! Every copy makes the accepted changes in that order, so they all end up holding the same
//! document, under the same block ids, and a change only ever lands on the content it was made
//! on ([`wordcraft_doc::Document::apply`] checks).
//!
//! A copy keeps the changes the hub hasn't accepted yet, and moves them past the changes of
//! others as those arrive, so what its participant sees never waits for the hub. When one of
//! them clashes with another participant's change, the copy drops it and the edits made after
//! it, and makes them again on the version with the other change: it runs the same commands
//! from the same places, the caret, the selection and the places in their params moved
//! through the other change.
//!
//! Next in milestone A1: agents' copies on threads of their own, with keys and roles; undo per
//! participant (until then a copy drops its undo steps when another participant's change
//! arrives, [`Session::apply_change`]); presence.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use wordcraft_doc::{ChangeSet, Document};

use crate::{Registry, Session};

mod hub;
mod replica;

pub use hub::{Accepted, Bounce, Hub, MAX_BATCH, MAX_KEPT, MAX_KEPT_BYTES, Took};
pub use replica::{Replica, Stats};

/// A participant of a room, numbered from 1 in the order they joined.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ParticipantId(pub u32);

impl std::fmt::Display for ParticipantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "participant {}", self.0)
    }
}

/// One shared document: its hub, and what its participants' copies share.
pub struct Room {
    hub: Mutex<Hub>,
    /// Signalled when the hub accepts a change.
    changed: Condvar,
    /// The commands, shared by the copies' sessions.
    registry: Arc<Registry>,
    /// Participants who joined so far.
    joined: AtomicU32,
}

impl Room {
    /// A room for `doc`, which it holds at version 0.
    pub fn new(doc: Document) -> Arc<Room> {
        Room::with_hub(Hub::new(doc))
    }

    /// A room whose hub is `hub`.
    pub fn with_hub(hub: Hub) -> Arc<Room> {
        Arc::new(Room { hub: Mutex::new(hub), changed: Condvar::new(), registry: Arc::new(crate::cmd::registry()), joined: AtomicU32::new(0) })
    }

    /// The hub, whatever a thread that panicked while holding it left (the hub changes its
    /// state only once a change is accepted whole).
    fn hub(&self) -> MutexGuard<'_, Hub> {
        self.hub.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The document's latest version: how many changes the hub has accepted.
    pub fn version(&self) -> u64 {
        self.hub().version()
    }

    /// The document as of its latest version, and that version.
    pub fn document(&self) -> (u64, Document) {
        let hub = self.hub();
        (hub.version(), hub.document().clone())
    }

    /// Join the room as a new participant called `name` (the author of their comments and
    /// tracked changes), with a copy of the document as of its latest version.
    pub fn join(self: &Arc<Self>, name: &str) -> Replica {
        let id = ParticipantId(self.joined.fetch_add(1, Ordering::Relaxed).saturating_add(1));
        let (version, doc) = self.document();
        let mut session = Session::with_registry(doc.clone(), self.registry.clone());
        session.author = name.to_string();
        Replica::new(self.clone(), id, session, version, doc)
    }

    /// Wait until the document is past `version`, or `timeout` has passed; the latest version.
    /// The web build has no threads to wait for: there it doesn't wait.
    pub fn wait_past(&self, version: u64, timeout: Duration) -> u64 {
        let hub = self.hub();
        if cfg!(target_arch = "wasm32") || hub.version() > version {
            return hub.version();
        }
        let waited = self.changed.wait_timeout_while(hub, timeout, |h| h.version() <= version);
        let (hub, _) = waited.unwrap_or_else(PoisonError::into_inner);
        hub.version()
    }

    /// Submit `changes`, made on `base`, from participant `by` ([`Hub::submit_all`]), and wake
    /// the participants waiting for a change when the hub accepts any.
    fn submit(&self, base: u64, changes: &[ChangeSet], by: ParticipantId) -> Result<Took, Bounce> {
        let took = self.hub().submit_all(base, changes, by);
        if took.is_ok() {
            self.changed.notify_all();
        }
        took
    }
}

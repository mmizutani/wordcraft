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
//! Each participant has a [`Role`]. The room's own process joins people and agents under a
//! name and a role ([`Room::join`]); anyone else joins with a key the room handed out for a
//! name and a role ([`Room::invite`], [`Room::join_with`]), so they can't claim another's name.
//! A viewer changes nothing; a commenter changes comments of their own; a suggester's edits are
//! tracked changes in their name, which others accept or reject. Their copies hold each
//! command to that ([`Session::run`] undoes one that does more), and the hub each change.
//!
//! Next in milestone A1: agents' copies on threads of their own; undo per participant (until
//! then a copy drops its undo steps when another participant's change arrives,
//! [`Session::apply_change`]); presence.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use wordcraft_doc::{ChangeSet, Document};

use crate::scope::Scope;
use crate::{Registry, Session};

mod hub;
mod replica;
pub(crate) mod rules;

pub use hub::{Accepted, Bounce, Hub, MAX_BATCH, MAX_INVITES, MAX_KEPT, MAX_KEPT_BYTES, MAX_PARTICIPANTS, Took};
pub use replica::{Replica, Stats};

/// A participant of a room, numbered from 1 in the order they joined.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ParticipantId(pub u32);

impl std::fmt::Display for ParticipantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "participant {}", self.0)
    }
}

/// What a participant may do in a room. Each role may do what the ones before it may, and more.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    /// Reads the document, and moves their own caret, selection and view; changes nothing.
    Viewer,
    /// Also adds comments and replies, and edits and deletes their own.
    Commenter,
    /// Also edits, always as tracked changes in their name (Track Changes is on for them
    /// alone), which others accept or reject; withdraws their own.
    Suggester,
    /// Also edits directly, and accepts and rejects tracked changes.
    Editor,
    /// Also changes the application's settings in their copy.
    Owner,
}

impl Role {
    /// Every role, from the one that may do least.
    pub const ALL: [Role; 5] = [Role::Viewer, Role::Commenter, Role::Suggester, Role::Editor, Role::Owner];

    /// Whether this role may run commands of `scope` in its copy. Everyone reads the document
    /// and moves their own selection and view; what a command changes in the document is held
    /// to the role once it ran ([`Session::role`]). The application's settings are the owner's.
    /// Files, other documents and windows are no copy's: the shared document is the room's.
    pub fn allows(self, scope: Scope) -> bool {
        match scope {
            Scope::Document | Scope::Selection | Scope::View => true,
            Scope::App => self == Role::Owner,
            Scope::File | Scope::Ui => false,
        }
    }

    /// The role's name: `viewer`, `commenter`, `suggester`, `editor` or `owner`.
    pub fn name(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Commenter => "commenter",
            Role::Suggester => "suggester",
            Role::Editor => "editor",
            Role::Owner => "owner",
        }
    }

    /// The role called `name` ([`Role::name`]).
    pub fn named(name: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|r| r.name() == name)
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Who a participant is: their number in the room, their name (the author of their comments
/// and tracked changes) and their role.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Participant {
    pub id: ParticipantId,
    pub name: String,
    pub role: Role,
}

/// Why a room didn't let someone in.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RoomError {
    /// No invite has the key, or it was taken back.
    #[error("no invite has this key")]
    UnknownKey,
    /// The name can't be a participant's, or an invite has it already.
    #[error("{0}")]
    Name(String),
    /// The room has as many participants or invites as it holds, or no key could be made.
    #[error("{0}")]
    Full(String),
}

/// One shared document: its hub, and what its participants' copies share.
pub struct Room {
    hub: Mutex<Hub>,
    /// Signalled when the hub accepts a change.
    changed: Condvar,
    /// The commands, shared by the copies' sessions.
    registry: Arc<Registry>,
}

impl Room {
    /// A room for `doc`, which it holds at version 0.
    pub fn new(doc: Document) -> Arc<Room> {
        Room::with_hub(Hub::new(doc))
    }

    /// A room whose hub is `hub`.
    pub fn with_hub(hub: Hub) -> Arc<Room> {
        Arc::new(Room { hub: Mutex::new(hub), changed: Condvar::new(), registry: Arc::new(crate::cmd::registry()) })
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
    /// tracked changes) with `role`, and a copy of the document as of its latest version. For
    /// the room's own process: anyone else joins with a key ([`Room::join_with`]).
    pub fn join(self: &Arc<Self>, name: &str, role: Role) -> Result<Replica, RoomError> {
        let (who, version, doc) = {
            let mut hub = self.hub();
            let who = hub.admit(name, role)?;
            (who, hub.version(), hub.document().clone())
        };
        Ok(self.copy_for(who, version, doc))
    }

    /// A key for a participant called `name` with `role`: whoever holds it joins as them
    /// ([`Room::join_with`]), and can't join as anyone else. Refused when the name is empty,
    /// too long or holds control characters, or another invite has it.
    pub fn invite(&self, name: &str, role: Role) -> Result<String, RoomError> {
        self.hub().invite(name, role)
    }

    /// Take back the invite with `key`: no one joins with it any more (copies that joined with
    /// it stay). Whether there was one.
    pub fn revoke(&self, key: &str) -> bool {
        self.hub().revoke(key)
    }

    /// Join the room with a key [`Room::invite`] handed out, under its name and role.
    pub fn join_with(self: &Arc<Self>, key: &str) -> Result<Replica, RoomError> {
        let (who, version, doc) = {
            let mut hub = self.hub();
            let who = hub.admit_with(key)?;
            (who, hub.version(), hub.document().clone())
        };
        Ok(self.copy_for(who, version, doc))
    }

    /// The participants who joined, in the order they did.
    pub fn participants(&self) -> Vec<Participant> {
        self.hub().participants().cloned().collect()
    }

    /// A copy of version `version` of the document, `doc`, for participant `who`.
    fn copy_for(self: &Arc<Self>, who: Participant, version: u64, doc: Document) -> Replica {
        let mut session = Session::with_registry(doc.clone(), self.registry.clone());
        session.author = who.name.clone();
        session.role = Some(who.role);
        Replica::new(self.clone(), who, session, version, doc)
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

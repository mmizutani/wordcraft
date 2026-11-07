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
//! The app's window shares the document it shows ([`Room::host`]): its owner's copy works on the
//! window's own session, lent to it for each step ([`Replica::lend`]), so what the person types
//! goes through the copy like anyone's. Agents with a key reach the room through the process's
//! [`Lobby`] (the app's control channel): each gets a copy of their own, on their connection's
//! thread, which answers their requests ([`hosted::call`]).
//!
//! Undo stays each participant's own: a copy's newest undo and redo steps keep the changes
//! others made since ([`Session::apply_theirs`]), so Undo takes the participant's step back
//! past them, as the hub moves an edit past those it accepted meanwhile, or refuses and names
//! who changed what the step changed. Edits made again after a clash keep the steps they had.
//!
//! Next in milestone A1: presence.

use std::net::SocketAddr;
use std::ops::RangeInclusive;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use wordcraft_doc::change::{Anchor, Mapped};
use wordcraft_doc::{ChangeSet, Document, StoryRef};

use crate::scope::Scope;
use crate::{Registry, Session};

pub mod hosted;
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

    /// Why this role's copy doesn't run commands of `scope`, when it doesn't ([`Role::allows`]).
    pub fn refuses(self, scope: Scope) -> Option<String> {
        if self.allows(scope) {
            return None;
        }
        Some(match scope {
            Scope::File | Scope::Ui => "files, other documents and windows aren't part of a shared document".to_string(),
            _ => format!("a {self} doesn't change the application's settings"),
        })
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
    /// The room was closed: its document isn't shared any more.
    #[error("the document isn't shared any more")]
    Closed,
}

/// What a room calls when its hub accepted changes or it closed: the app's window asks to be
/// drawn again, to show them.
type OnChange = Arc<dyn Fn() + Send + Sync>;

/// One shared document: its hub, and what its participants' copies share.
pub struct Room {
    hub: Mutex<Hub>,
    /// Signalled when the hub accepts a change, and when the room closes.
    changed: Condvar,
    /// Called after that ([`Room::on_change`]).
    on_change: Mutex<Option<OnChange>>,
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
        Room::with_registry(hub, Arc::new(crate::cmd::registry()))
    }

    fn with_registry(hub: Hub, registry: Arc<Registry>) -> Arc<Room> {
        Arc::new(Room { hub: Mutex::new(hub), changed: Condvar::new(), on_change: Mutex::new(None), registry })
    }

    /// Share the active document of `session`, a session the room's own process keeps for
    /// itself (the app's window, with its other documents): a room holding it at version 0,
    /// which `name` joins as its owner with a copy that works on `session` itself, lent to it
    /// for each step ([`Replica::lend`]). Blocks without an id get one first, in `session`
    /// too. While the room is open, the document counts as used, so a file opened in `session`
    /// opens beside it, never in its place. Refused when the name can't be a participant's.
    pub fn host(session: &mut Session, name: &str) -> Result<(Arc<Room>, Replica), RoomError> {
        // What changed outside a command is the session's own, before the room has it.
        session.record_outside_changes();
        let mut hub = Hub::new(session.doc.clone());
        let who = hub.admit(name, Role::Owner)?;
        if *hub.document() != session.doc || hub.document().block_ids() != session.doc.block_ids() {
            session.reset_shared(hub.document().clone());
        }
        let doc = hub.document().clone();
        let room = Room::with_registry(hub, session.registry.clone());
        // A shared document is never replaced by one opened in its place.
        session.shared_through(&room);
        // The copy's own session stands in while the window's isn't lent to it.
        let stand_in = Session::with_registry(Document::new(), room.registry.clone());
        let copy = Replica::lent_by(room.clone(), who, session, stand_in, doc);
        Ok((room, copy))
    }

    /// Call `f` whenever the hub accepted changes, and when the room closes, on the thread that
    /// made that happen (in place of what was called before). The room holds no lock of its own
    /// while `f` runs, so `f` may use the room, even close it.
    pub fn on_change(&self, f: impl Fn() + Send + Sync + 'static) {
        *self.on_change.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(f));
    }

    /// Wake whoever waits for a change ([`Room::wait_past`], [`Room::on_change`]).
    fn changed(&self) {
        self.changed.notify_all();
        let f = self.on_change.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(f) = f {
            f();
        }
    }

    /// Close the room: no one joins any more, the hub accepts no change, and the copies
    /// hosted for those with a key stop answering ([`hosted::call`]). What it has stays
    /// readable.
    pub fn close(&self) {
        self.hub().close();
        self.changed();
    }

    /// Whether the room was closed ([`Room::close`]).
    pub fn is_closed(&self) -> bool {
        self.hub().is_closed()
    }

    /// Participant `id` leaves the room ([`Hub::leave`]); a copy does when it's dropped.
    pub fn leave(&self, id: ParticipantId) {
        self.hub().leave(id);
    }

    /// Whether participant `id` is in the room: they joined, didn't leave, the invite they
    /// joined with wasn't taken back, and the room isn't closed.
    pub fn is_in(&self, id: ParticipantId) -> bool {
        let hub = self.hub();
        !hub.is_closed() && hub.participant(id).is_some()
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

    /// Where the changes that made versions `versions` are in the latest version, as anchors
    /// moved past the changes accepted after each: from the start of the first text they put in
    /// or changed to the end of the last ([`touched`]), in the body when they changed text
    /// there, else in the story changed first; where one only took text out, the place that
    /// text was. With `by`, only the changes of the participant of that name count (every copy
    /// that joined with their invite has it): a tool call's, with the person's edits made
    /// meanwhile left out. `None` when the hub no longer keeps the first of them, none was
    /// `by`'s, or none of the text they changed is there any more (or they changed none:
    /// styles or settings alone). What shows a participant's change, such as the one an
    /// assistant's tool call made.
    pub fn where_changed(&self, versions: RangeInclusive<u64>, by: Option<&str>) -> Option<(Anchor, Anchor)> {
        let (first, last) = (*versions.start(), *versions.end());
        if first == 0 || first > last {
            return None;
        }
        let (changes, theirs, doc) = {
            let hub = self.hub();
            let changes = hub.since(first.saturating_sub(1)).ok()?;
            let theirs: Vec<bool> = changes.iter().map(|a| a.version <= last && by.is_none_or(|name| hub.name_of(a.by) == Some(name))).collect();
            (changes, theirs, hub.document().clone())
        };
        if changes.first()?.version != first {
            return None;
        }
        let mut spans: Vec<(usize, (Anchor, Anchor))> = theirs
            .iter()
            .enumerate()
            .filter(|(_, t)| **t)
            .filter_map(|(i, _)| changes.get(i).map(|a| (i, a)))
            .flat_map(|(i, a)| touched(&a.change).into_iter().map(move |s| (i, s)))
            .collect();
        // Changes across many paragraphs are shown by the first and the last places they
        // touched: each place is looked for in the whole document.
        if spans.len() > MOST_PLACES / 2 {
            spans.drain(MOST_PLACES / 4..spans.len().saturating_sub(MOST_PLACES / 4));
        }
        let mut places = Vec::new();
        for (i, (start, end)) in spans {
            let after = changes.get(i.saturating_add(1)..).unwrap_or_default();
            let Some((s, e)) = carry(start, end, after) else { continue };
            places.extend([doc.place(Mapped::At(s), None), doc.place(Mapped::At(e), None)].into_iter().flatten());
        }
        let story = if places.iter().any(|p| p.story == StoryRef::Body) { StoryRef::Body } else { places.first()?.story };
        let mut there = places.iter().filter(|p| p.story == story);
        let first = there.next()?;
        let (start, end) = there.fold((first, first), |(lo, hi), p| (lo.min(p), hi.max(p)));
        Some((doc.anchor_at(start)?, doc.anchor_at(end)?))
    }

    /// The first and the last version after `version` that participant `name` made (any copy
    /// that joined with their invite), if they made any: what a tool call that started then
    /// changed. `None` too when the hub no longer keeps the changes after `version`.
    pub fn changes_by(&self, version: u64, name: &str) -> Option<(u64, u64)> {
        let hub = self.hub();
        let changes = hub.since(version).ok()?;
        let mut theirs = changes.iter().filter(|a| hub.name_of(a.by) == Some(name)).map(|a| a.version);
        let first = theirs.next()?;
        Some((first, theirs.next_back().unwrap_or(first)))
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

    /// [`Room::invite`] under `name`, or under the first of "name (2)", "name (3)"… that no
    /// invite, participant, tracked change or comment has had ([`Hub::invite_numbered`]): the
    /// name it got, and the key. For a participant that comes again and again, like each
    /// conversation with an assistant.
    pub fn invite_numbered(&self, name: &str, role: Role) -> Result<(String, String), RoomError> {
        self.hub().invite_numbered(name, role)
    }

    /// Take back the invite with `key`: no one joins with it any more, and the copies that
    /// joined with it are out of the room (the hub accepts no change of theirs, and a hosted
    /// copy stops answering). Whether there was one.
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

    /// The participants in the room, in the order they joined.
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

    /// Wait until the document is past `version`, the room closes, or `timeout` has passed;
    /// the latest version. The web build has no threads to wait for: there it doesn't wait.
    pub fn wait_past(&self, version: u64, timeout: Duration) -> u64 {
        let hub = self.hub();
        if cfg!(target_arch = "wasm32") || hub.version() > version || hub.is_closed() {
            return hub.version();
        }
        let waited = self.changed.wait_timeout_while(hub, timeout, |h| h.version() <= version && !h.is_closed());
        let (hub, _) = waited.unwrap_or_else(PoisonError::into_inner);
        hub.version()
    }

    /// Submit `changes`, made on `base`, from participant `by` ([`Hub::submit_all`]), and wake
    /// the participants waiting for a change when the hub accepts any.
    fn submit(&self, base: u64, changes: &[ChangeSet], by: ParticipantId) -> Result<Took, Bounce> {
        let took = self.hub().submit_all(base, changes, by);
        if took.is_ok() {
            self.changed();
        }
        took
    }
}

/// Where those with a key find the room the process shares, if it shares one: the app's
/// window opens it when it shares its document, and its control channel lets participants in
/// through it ([`Lobby::join_with`]), as does the room's own port, whose address the lobby
/// keeps ([`Lobby::addr`]). Clones are the same lobby.
#[derive(Clone, Default)]
pub struct Lobby(Arc<Mutex<Option<Arc<Room>>>>, Arc<Mutex<Option<SocketAddr>>>);

impl Lobby {
    fn slot(&self) -> MutexGuard<'_, Option<Arc<Room>>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Where participants with a key reach the lobby on this computer: the room's own port
    /// (`127.0.0.1`, which only invites' keys open), if the process listens for them.
    pub fn addr(&self) -> Option<SocketAddr> {
        *self.1.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Say where participants reach the lobby ([`Lobby::addr`]); `None` once nothing listens.
    pub fn set_addr(&self, addr: Option<SocketAddr>) {
        *self.1.lock().unwrap_or_else(PoisonError::into_inner) = addr;
    }

    /// Let participants with a key into `room` (in place of the room before, which closes).
    pub fn open(&self, room: Arc<Room>) {
        let before = self.slot().replace(room.clone());
        if let Some(before) = before.filter(|b| !Arc::ptr_eq(b, &room)) {
            before.close();
        }
    }

    /// Close the room the lobby lets participants into, if it has one ([`Room::close`]).
    pub fn close(&self) {
        let before = self.slot().take();
        if let Some(room) = before {
            room.close();
        }
    }

    /// The room participants with a key join, while it's open.
    pub fn room(&self) -> Option<Arc<Room>> {
        self.slot().clone().filter(|r| !r.is_closed())
    }

    /// Join the room with a key it handed out ([`Room::join_with`]). [`RoomError::Closed`]
    /// when no document is shared.
    pub fn join_with(&self, key: &str) -> Result<Replica, RoomError> {
        self.room().ok_or(RoomError::Closed)?.join_with(key)
    }
}

impl std::fmt::Debug for Lobby {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lobby").field("open", &self.room().is_some()).finish()
    }
}

/// Most places of a change [`Room::where_changed`] looks for.
const MOST_PLACES: usize = 256;

/// Where the span `start`..`end` a change put in or changed is once `after` (the changes
/// accepted after it, in order) are in, its ends carried together: text put in right before
/// the span isn't part of it (its start moves past that text), nor text put in right after it
/// (its end stays before). `None` when its part is gone, or when it held text and a later
/// change took all of that text out (rejected, deleted): text put in at that place afterwards
/// isn't the change's either.
fn carry(start: Anchor, end: Anchor, after: &[hub::Accepted]) -> Option<(Anchor, Anchor)> {
    let held_text = start != end;
    let (mut s, mut e) = (start, end);
    for later in after {
        s = anchor(if held_text { later.change.map_past(s) } else { later.change.map(s) })?;
        e = anchor(later.change.map(e))?;
        if held_text && s == e {
            return None;
        }
    }
    Some((s, e))
}

/// Mapped place `m` as an anchor: the start or the end of a block it is before or after.
fn anchor(m: Mapped) -> Option<Anchor> {
    match m {
        Mapped::At(a) => Some(a),
        Mapped::Before(block) => Some(Anchor { block, off: 0 }),
        Mapped::After(block) => Some(Anchor { block, off: usize::MAX }),
        Mapped::Gone => None,
    }
}

/// The places change `cs` put in or changed text, as the start and the end of each, as anchors
/// in the document right after it: the characters it put in each paragraph it edited in place
/// (the whole paragraph when only its properties changed); the text it put in the blocks it put
/// in ([`wordcraft_doc::change::Splice::put_in`]: a table from the start of its first paragraph
/// to the end of its last); and where it only took blocks out, the block after them (or before
/// them, at the end of their list), both ends the same. Parts it added or removed whole, and
/// fields, have no place.
fn touched(cs: &ChangeSet) -> Vec<(Anchor, Anchor)> {
    let mut out = Vec::new();
    for e in &cs.edits {
        let (from, to) = e.chars.as_ref().map_or((0, usize::MAX), |c| (c.at, c.at.saturating_add(c.inserted.text.len())));
        out.push((Anchor { block: e.id, off: from }, Anchor { block: e.id, off: to }));
    }
    for s in &cs.splices {
        match s.put_in() {
            Some((from, to)) => {
                if let (Some(from), Some(to)) = (anchor(from), anchor(to)) {
                    out.push((from, to));
                }
            }
            None => match (s.before, s.after) {
                (Some(block), _) => out.push((Anchor { block, off: 0 }, Anchor { block, off: 0 })),
                (None, Some(block)) => out.push((Anchor { block, off: usize::MAX }, Anchor { block, off: usize::MAX })),
                (None, None) => {}
            },
        }
    }
    out
}

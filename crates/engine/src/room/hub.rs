//! [`Hub`]: the one place that puts a shared document's changes in order.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use wordcraft_doc::change::Side;
use wordcraft_doc::{ChangeSet, Document};

use wordcraft_doc::change::Anchor;

use super::{Participant, ParticipantId, Presence, Role, RoomError, Seen, rules};
use crate::agent_files::AgentFiles;

/// Most changes a hub keeps for copies that are behind; a copy further behind reads the document
/// again.
pub const MAX_KEPT: usize = 1 << 14;

/// Most memory the kept changes may hold, roughly (as for a session's changes): the oldest go
/// first.
pub const MAX_KEPT_BYTES: usize = 64 << 20;

/// Most changes one [`Hub::submit_all`] takes.
pub const MAX_BATCH: usize = 1024;

/// Most participants a hub admits.
pub const MAX_PARTICIPANTS: usize = 4096;

/// Most invites a hub keeps.
pub const MAX_INVITES: usize = 4096;

/// Most characters in a participant's name.
const MAX_NAME: usize = 128;

/// Most participants whose place the hub keeps at once ([`Hub::set_presence`]): past them, a
/// place told last pushes out the one told longest ago.
pub const MAX_PRESENCES: usize = 64;

/// How many colours tell participants apart ([`Seen::color`]).
pub const COLORS: u8 = 8;

/// A change the hub accepted.
#[derive(Clone, Debug)]
pub struct Accepted {
    /// The version of the document it made (the first change makes version 1).
    pub version: u64,
    /// Who made it.
    pub by: ParticipantId,
    /// The change, as moved past the changes accepted before it: it applies to the document at
    /// the version before.
    pub change: Arc<ChangeSet>,
}

/// What the hub did with changes it accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Took {
    /// The version of the document the first of them made; the others made the versions after.
    pub version: u64,
    /// How many it accepted: the first ones, up to one that clashes with a change accepted since
    /// the version they were made on.
    pub accepted: usize,
    /// How many changes they were moved past: those accepted since the version they were made
    /// on.
    pub moved_past: usize,
    /// Why it sent the others back, when it did ([`Bounce::Clash`]).
    pub clash: Option<Bounce>,
}

/// Why the hub sent a change back. The copy that made it drops it and makes the edit again on
/// the latest version: after the change it clashed with, or after reading the document again.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Bounce {
    /// It clashes with a change accepted since the version it was made on: the copy finds the
    /// same clash when that change reaches it.
    #[error("it clashes with change {version}: {why}")]
    Clash { version: u64, why: String },
    /// The hub can't move it to its latest version: it was made on a version older than the
    /// changes the hub keeps, or on one the document never had.
    #[error("{0}")]
    Version(String),
    /// It doesn't apply to the document (it wasn't made on the content it replaces), or it
    /// puts in a block under an id the document already has.
    #[error("{0}")]
    Refused(String),
}

/// A key the hub handed out ([`Hub::invite`]).
#[derive(Clone)]
struct Invite {
    key: String,
    name: String,
    role: Role,
    /// Which invite this is: the participants who join with it keep its number.
    number: u64,
    /// Which files its participant's commands can use, when the invite chose for them (never
    /// more than the room's setting); `None`: what the setting says ([`crate::room::Room::reach`]).
    files: Option<AgentFiles>,
}

impl std::fmt::Debug for Invite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The key stays out of logs.
        f.debug_struct("Invite").field("name", &self.name).field("role", &self.role).field("files", &self.files).finish_non_exhaustive()
    }
}

/// A participant, with how they joined and whether they're still in the room.
#[derive(Clone, Debug)]
struct Member {
    who: Participant,
    /// The invite they joined with, by its number; `None` when the room's own process let
    /// them in ([`Hub::admit`]).
    via: Option<u64>,
    /// Whether they're in the room: not once they left ([`Hub::leave`]), or the invite they
    /// joined with was taken back ([`Hub::revoke`]).
    here: bool,
    /// Which of the [`COLORS`] is theirs: the one their name got when it first joined.
    color: u8,
    /// Where their selection is, once their copy said ([`Hub::set_presence`]).
    at: Option<Placed>,
}

/// Where a participant's selection is in the document as of a version.
#[derive(Clone, Copy, Debug)]
struct Placed {
    version: u64,
    at: Presence,
    /// When it was told: how many places the hub had been told before it.
    order: u64,
}

/// The hub of a shared document: the document as of its latest version, the changes that made
/// the latest versions, and who may change it. Changes reach it one at a time ([`super::Room`]
/// holds it behind a lock).
#[derive(Clone, Debug)]
pub struct Hub {
    doc: Document,
    version: u64,
    kept: VecDeque<Accepted>,
    /// What the kept changes weigh (as a session's changes do).
    bytes: usize,
    /// Most changes kept: [`MAX_KEPT`], fewer in tests.
    most: usize,
    /// Who joined, in the order they did: participant `n` is the `n`th.
    participants: Vec<Member>,
    /// The keys handed out.
    invites: Vec<Invite>,
    /// How many invites were handed out: the next one's number.
    invited: u64,
    /// Whether the room was closed ([`Hub::close`]).
    closed: bool,
    /// How many places participants' copies told the hub ([`Hub::set_presence`]).
    told: u64,
}

impl Hub {
    /// A hub for `doc` at version 0. Every block gets an id of its own, and the document a
    /// paragraph if it has none.
    pub fn new(mut doc: Document) -> Hub {
        doc.ensure_nonempty();
        doc.ensure_block_ids(None);
        Hub {
            doc,
            version: 0,
            kept: VecDeque::new(),
            bytes: 0,
            most: MAX_KEPT,
            participants: Vec::new(),
            invites: Vec::new(),
            invited: 0,
            closed: false,
            told: 0,
        }
    }

    /// Admit a participant called `name` with `role`. Refused when the name is empty, too long
    /// or holds control characters, an invite ([`Hub::invite`]) or a participant (one who left
    /// too) has it, whatever its case, or the room is closed: comments and tracked changes are
    /// a participant's by their name, so no two have the same one. (Another copy of an invited
    /// participant's joins with their key, [`Hub::admit_with`].)
    pub fn admit(&mut self, name: &str, role: Role) -> Result<Participant, RoomError> {
        self.open()?;
        let name = valid_name(name)?;
        if self.invites.iter().any(|i| same_name(&i.name, name)) {
            return Err(RoomError::Name(format!("{name} is invited: they join with their key")));
        }
        if self.participants.iter().any(|m| same_name(&m.who.name, name)) {
            return Err(RoomError::Name(format!("{name} is someone's name in this room already")));
        }
        self.add(name, role, None)
    }

    /// A key for a participant called `name` with `role` ([`Hub::admit_with`]): 32 hex digits
    /// from the system's random source. Refused like [`Hub::admit`], or when another invite
    /// or a participant has the name (one who left too: their comments and tracked changes
    /// keep it).
    pub fn invite(&mut self, name: &str, role: Role) -> Result<String, RoomError> {
        self.invite_with(name, role, None)
    }

    /// [`Hub::invite`], choosing which files its participant's commands can use, at most what the
    /// room's setting allows ([`Hub::files_for`]); `None`: what the setting says.
    pub fn invite_with(&mut self, name: &str, role: Role, files: Option<AgentFiles>) -> Result<String, RoomError> {
        self.open()?;
        let name = valid_name(name)?;
        if self.invites.iter().any(|i| same_name(&i.name, name)) || self.participants.iter().any(|m| same_name(&m.who.name, name)) {
            return Err(RoomError::Name(format!("{name} is someone's name in this room already")));
        }
        if self.invites.len() >= MAX_INVITES {
            return Err(RoomError::Full(format!("the room holds {MAX_INVITES} invites at most")));
        }
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|e| RoomError::Full(format!("no key could be made: {e}")))?;
        let key: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let number = self.invited;
        self.invited = self.invited.saturating_add(1);
        self.invites.push(Invite { key: key.clone(), name: name.to_string(), role, number, files });
        Ok(key)
    }

    /// [`Hub::invite`] under `name`, or, when someone has had it, under the first of "name (2)",
    /// "name (3)"… no one has had (the name shortened so the number fits): the name it got, and
    /// the key. Someone has had a name when an invite or a participant has it, and when a tracked
    /// change or a comment in the document is in it: the role rules give a participant everything
    /// in their name, so a newcomer under it could take back or rewrite those as their own (an
    /// assistant's suggestions from a room since closed, or another person's assistant's). One
    /// pass over the names, however many are taken.
    pub fn invite_numbered(&mut self, name: &str, role: Role) -> Result<(String, String), RoomError> {
        self.open()?;
        let name = valid_name(name)?;
        let authors: HashSet<&str> =
            self.doc.revisions.iter().map(|r| r.author.as_str()).chain(self.doc.comments.values().map(|c| c.author.as_str())).collect();
        let taken: HashSet<String> = self
            .invites
            .iter()
            .map(|i| i.name.as_str())
            .chain(self.participants.iter().map(|m| m.who.name.as_str()))
            .chain(authors)
            .map(str::to_lowercase)
            .collect();
        // A free name is among the first `taken.len() + 1`; " (n)" takes at most 2 + 20 characters.
        let short: String = name.chars().take(MAX_NAME.saturating_sub(22)).collect();
        let short = short.trim_end();
        let free = (1..=taken.len().saturating_add(1))
            .map(|n| if n == 1 { name.to_string() } else { format!("{short} ({n})") })
            .find(|candidate| !taken.contains(&candidate.to_lowercase()))
            .ok_or_else(|| RoomError::Name(format!("no name like {name} is free in this room")))?;
        let key = self.invite(&free, role)?;
        Ok((free, key))
    }

    /// Whether participant `id` joined with a key an invite handed out ([`Hub::admit_with`]), so
    /// their commands are held to the files the room gives them, rather than joining in the
    /// room's own process (the window, whose commands are the person's).
    pub fn joined_with_key(&self, id: ParticipantId) -> bool {
        self.member(id).is_some_and(|m| m.via.is_some())
    }

    /// The files participant `id`'s commands can use while the room's setting is `setting`: what
    /// their invite chose, but never more than the setting ([`AgentFiles::at_most`]), else the
    /// setting. None at all when they aren't in the room (they left, or the invite they joined
    /// with was taken back) or the room doesn't know them.
    pub fn files_for(&self, id: ParticipantId, setting: AgentFiles) -> AgentFiles {
        let Some(member) = self.member(id).filter(|m| m.here) else { return AgentFiles::None };
        let Some(number) = member.via else { return setting };
        match self.invites.iter().find(|i| i.number == number) {
            Some(invite) => invite.files.map_or(setting, |f| f.at_most(setting)),
            None => AgentFiles::None,
        }
    }

    /// Take back the invite with `key`: no one joins with it any more, and those who joined
    /// with it are out of the room (the hub accepts no change of theirs). Whether there was one.
    pub fn revoke(&mut self, key: &str) -> bool {
        let gone: Vec<u64> = self.invites.iter().filter(|i| same_key(&i.key, key)).map(|i| i.number).collect();
        self.invites.retain(|i| !same_key(&i.key, key));
        for m in &mut self.participants {
            if m.via.is_some_and(|n| gone.contains(&n)) {
                m.here = false;
                m.at = None;
            }
        }
        !gone.is_empty()
    }

    /// Admit the participant the invite with `key` names, with its role. The same key admits
    /// them again (another copy of theirs) until it is revoked. Refused when the room is closed.
    pub fn admit_with(&mut self, key: &str) -> Result<Participant, RoomError> {
        self.open()?;
        // Every invite is compared, so how long it takes says nothing about the key.
        let found = self.invites.iter().fold(None, |found, i| if same_key(&i.key, key) { Some(i) } else { found });
        let Invite { name, role, number, .. } = found.cloned().ok_or(RoomError::UnknownKey)?;
        self.add(&name, role, Some(number))
    }

    fn add(&mut self, name: &str, role: Role, via: Option<u64>) -> Result<Participant, RoomError> {
        if self.participants.len() >= MAX_PARTICIPANTS {
            return Err(RoomError::Full(format!("the room holds {MAX_PARTICIPANTS} participants at most")));
        }
        let n = u32::try_from(self.participants.len()).map_err(|_| RoomError::Full("too many participants".into()))?;
        let who = Participant { id: ParticipantId(n.saturating_add(1)), name: name.to_string(), role };
        // Another copy of theirs has the colour they have; back after they left, they have the
        // one they had, unless someone in the room took it meanwhile.
        let theirs = |m: &&Member| same_name(&m.who.name, name);
        let had = self.participants.iter().filter(theirs).map(|m| m.color).last();
        let taken = |c: u8| self.participants.iter().any(|m| m.here && m.color == c && !same_name(&m.who.name, name));
        let color = match self.participants.iter().filter(|m| m.here).find(theirs) {
            Some(m) => m.color,
            None => had.filter(|c| !taken(*c)).unwrap_or_else(|| self.free_color()),
        };
        self.participants.push(Member { who: who.clone(), via, here: true, color, at: None });
        Ok(who)
    }

    /// The colour for a name that joins for the first time: the first of the [`COLORS`] no one
    /// in the room has, else the one fewest have.
    fn free_color(&self) -> u8 {
        let mut used = [0usize; COLORS as usize];
        for m in self.participants.iter().filter(|m| m.here) {
            if let Some(n) = used.get_mut(usize::from(m.color)) {
                *n = n.saturating_add(1);
            }
        }
        (0..COLORS).min_by_key(|c| used.get(usize::from(*c)).copied().unwrap_or(0)).unwrap_or(0)
    }

    /// Which of the [`COLORS`] participant `id` has, also after they left.
    pub fn color_of(&self, id: ParticipantId) -> Option<u8> {
        self.member(id).map(|m| m.color)
    }

    /// Participant `id`, while they're in the room.
    pub fn participant(&self, id: ParticipantId) -> Option<&Participant> {
        self.member(id).filter(|m| m.here).map(|m| &m.who)
    }

    /// The name of participant `id`, also after they left: their edits keep it.
    pub fn name_of(&self, id: ParticipantId) -> Option<&str> {
        self.member(id).map(|m| m.who.name.as_str())
    }

    fn member(&self, id: ParticipantId) -> Option<&Member> {
        id.0.checked_sub(1).and_then(|n| self.participants.get(usize::try_from(n).ok()?))
    }

    /// The participants in the room, in the order they joined.
    pub fn participants(&self) -> impl Iterator<Item = &Participant> {
        self.participants.iter().filter(|m| m.here).map(|m| &m.who)
    }

    /// Participant `id`'s selection is `at` in the document as of `version`, as their copy has
    /// it (`None`: the others don't see it): the others see it there, moved with the text
    /// through the changes accepted since ([`Presence::through`]) and those the hub accepts from
    /// now on. `None` when the hub keeps nothing: they aren't in the room, `version` is ahead of
    /// the document, the hub no longer keeps the changes since it, or the text `at` is in is
    /// gone (the hub keeps moving the place their copy told it before). Otherwise whether that
    /// changed where the others see them. The hub keeps the places of [`MAX_PRESENCES`]
    /// participants: past them, the place told longest ago of one who joined with a key goes
    /// (theirs is told again when it moves).
    pub fn set_presence(&mut self, id: ParticipantId, version: u64, at: Option<Presence>) -> Option<bool> {
        let i = self.here_at(id)?;
        let now = self.version;
        if version > now {
            return None;
        }
        let at = match at {
            Some(p) => Some(self.moved(id, p, version, now)?),
            None => None,
        };
        let was = self.place_at(i, now);
        let order = self.told;
        self.told = self.told.saturating_add(1);
        let m = self.participants.get_mut(i)?;
        m.at = at.map(|at| Placed { version: now, at, order });
        if at.is_some() {
            self.keep_at_most(i);
        }
        Some(was != at)
    }

    /// The participants in the room, in the order they joined, each with where their selection
    /// is in the document as of `version` ([`Seen`]), when the hub knows. Each place is moved
    /// to `version` once, through the changes accepted since it was told, and kept there. A
    /// place told as of a later version is [`Seen::later`]; none is known when `version` is
    /// ahead of the document.
    pub fn presences_at(&mut self, version: u64) -> Vec<Seen> {
        let ahead = version > self.version;
        let mut seen = Vec::new();
        for i in 0..self.participants.len() {
            let Some(m) = self.participants.get(i).filter(|m| m.here) else { continue };
            let (who, color, placed) = (m.who.clone(), m.color, m.at);
            let (at, later) = match placed {
                None => (None, false),
                Some(_) if ahead => (None, false),
                Some(p) if p.version > version => (None, true),
                Some(p) if p.version == version => (Some(p.at), false),
                Some(p) => {
                    let at = self.moved(who.id, p.at, p.version, version);
                    if let Some(m) = self.participants.get_mut(i) {
                        m.at = at.map(|at| Placed { version, at, order: p.order });
                    }
                    (at, false)
                }
            };
            seen.push(Seen { who, color, at, later });
        }
        seen
    }

    /// Presence `p` of participant `id`, as of version `from`, moved through the changes accepted
    /// after it up to version `to`: past text `id` put in where it is, before text others put in
    /// there. An end a change leaves at the end of a paragraph (where a deleted paragraph left
    /// it, [`Mapped::After`](wordcraft_doc::change::Mapped::After)) is at that paragraph's end as
    /// it was then, as a copy has it, so text typed there later goes after it. `None` when its
    /// text is gone, or the hub doesn't keep those changes.
    fn moved(&self, id: ParticipantId, p: Presence, from: u64, to: u64) -> Option<Presence> {
        if from > to || to > self.version {
            return None;
        }
        // The kept changes made versions `first`, `first + 1`… in turn.
        let first = self.kept.front().map_or(self.version.saturating_add(1), |a| a.version);
        if from.saturating_add(1) < first {
            return None;
        }
        let skip = usize::try_from(from.saturating_add(1).saturating_sub(first)).ok()?;
        let take = usize::try_from(to.saturating_sub(from)).ok()?;
        let mut p = p;
        for (k, a) in self.kept.iter().enumerate().skip(skip).take(take) {
            let moved = p.through(&a.change, a.by == id)?;
            p = Presence { anchor: self.end_after(moved.anchor, k), focus: self.end_after(moved.focus, k) };
        }
        if to < self.version {
            return Some(p);
        }
        // One whose paragraph's length then couldn't be worked out is at its end now.
        let settle = |a: Anchor| if a.off == usize::MAX { self.doc.anchor_at(&self.doc.pos_of(a, None)?) } else { Some(a) };
        Some(Presence { anchor: settle(p.anchor)?, focus: settle(p.focus)? })
    }

    /// Anchor `a` as kept change `k` (by place in the kept list) left it: at the end of its block
    /// when its offset is past every text ([`super::anchor`]), as an offset, the length the
    /// paragraph had then (the end of a table's last paragraph, for a table). Worked out from
    /// the paragraph's length at the first change after `k` that took it out whole (moved or
    /// replaced it: the change says how long it was), else as it is now, less what the changes
    /// after `k` before that put in it and plus what they took out. `a` as it is when the
    /// paragraph isn't in the document.
    fn end_after(&self, a: Anchor, k: usize) -> Anchor {
        if a.off != usize::MAX {
            return a;
        }
        let Some(para) = self.doc.pos_of(a, None).and_then(|pos| self.doc.para_at(&pos)) else { return a };
        let block = para.id;
        // What the changes after `k` put in the paragraph, less what they took out of it.
        let (mut put, mut took) = (0usize, 0usize);
        let mut len = None;
        'later: for later in self.kept.iter().skip(k.saturating_add(1)) {
            let c = &later.change;
            for e in c.edits.iter().filter(|e| e.id == block) {
                if let Some(chars) = &e.chars {
                    put = put.saturating_add(chars.inserted.text.len());
                    took = took.saturating_add(chars.removed);
                }
            }
            // A table taken out whole and put back (a row put in) keeps the paragraph's text,
            // or the place went with it ([`ChangeSet::map`]), so it changes no length.
            if let Some(old) = c.splices.iter().find_map(|s| s.removed.iter().find(|o| o.id == block)) {
                len = Some(old.len);
                break 'later;
            }
        }
        let len = len.unwrap_or_else(|| para.len());
        len.checked_sub(put).and_then(|n| n.checked_add(took)).map_or(a, |off| Anchor { block, off })
    }

    /// Where participant `i` (by place in the list) is as of `version`, as the hub knows it.
    fn place_at(&self, i: usize, version: u64) -> Option<Presence> {
        let m = self.participants.get(i)?;
        let p = m.at.filter(|p| p.version <= version)?;
        self.moved(m.who.id, p.at, p.version, version)
    }

    /// Where participant `id` is in the list, while they're in the room.
    fn here_at(&self, id: ParticipantId) -> Option<usize> {
        let i = usize::try_from(id.0.checked_sub(1)?).ok()?;
        self.participants.get(i).filter(|m| m.here).map(|_| i)
    }

    /// At most [`MAX_PRESENCES`] places, now that participant `keep` (by place in the list) told
    /// theirs: past them, the one told longest ago of a participant who joined with a key goes
    /// (of anyone's, when only those the room's own process let in are left).
    fn keep_at_most(&mut self, keep: usize) {
        if self.participants.iter().filter(|m| m.at.is_some()).count() <= MAX_PRESENCES {
            return;
        }
        let oldest = self
            .participants
            .iter()
            .enumerate()
            .filter(|(j, m)| *j != keep && m.at.is_some())
            .min_by_key(|(_, m)| (m.via.is_none(), m.at.map_or(0, |p| p.order)))
            .map(|(j, _)| j);
        if let Some(m) = oldest.and_then(|j| self.participants.get_mut(j)) {
            m.at = None;
        }
    }

    /// Every place moved to the latest version, before the changes it is at are let go.
    fn bring_places_up(&mut self) {
        let now = self.version;
        for i in 0..self.participants.len() {
            let Some((id, p)) = self.participants.get(i).and_then(|m| Some((m.who.id, m.at?))).filter(|(_, p)| p.version < now) else {
                continue;
            };
            let at = self.moved(id, p.at, p.version, now);
            if let Some(m) = self.participants.get_mut(i) {
                m.at = at.map(|at| Placed { version: now, at, order: p.order });
            }
        }
    }

    /// Participant `id` leaves the room: the hub accepts no change of theirs any more (their
    /// name stays theirs). Whether they were in it.
    pub fn leave(&mut self, id: ParticipantId) -> bool {
        let Some(m) = id.0.checked_sub(1).and_then(|n| self.participants.get_mut(usize::try_from(n).ok()?)) else { return false };
        m.at = None;
        std::mem::replace(&mut m.here, false)
    }

    /// Close the room: no one joins any more, and the hub accepts no change. What it has stays
    /// readable.
    pub fn close(&mut self) {
        self.closed = true;
    }

    /// Whether the room was closed ([`Hub::close`]).
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    fn open(&self) -> Result<(), RoomError> {
        if self.closed { Err(RoomError::Closed) } else { Ok(()) }
    }

    /// The same hub, keeping at most `most` changes (at least one).
    pub fn keeping(mut self, most: usize) -> Hub {
        self.most = most.max(1);
        self
    }

    /// The document's latest version: how many changes the hub has accepted.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// The document as of [`Self::version`].
    pub fn document(&self) -> &Document {
        &self.doc
    }

    /// The changes accepted after version `since`, oldest first. An error when the hub no
    /// longer keeps them all, or `since` is ahead of the document.
    pub fn since(&self, since: u64) -> Result<Vec<Accepted>, Bounce> {
        if since > self.version {
            return Err(Bounce::Version(format!("version {since} is ahead of the document, which is at version {}", self.version)));
        }
        let oldest = self.kept.front().map_or(self.version, |a| a.version.saturating_sub(1));
        if since < oldest {
            return Err(Bounce::Version(format!("the changes after version {since} are no longer kept: read the document again")));
        }
        Ok(self.kept.iter().filter(|a| a.version > since).cloned().collect())
    }

    /// Accept `change`, which participant `by` made on version `base`: [`Hub::submit_all`] with
    /// one change.
    pub fn submit(&mut self, base: u64, change: &ChangeSet, by: ParticipantId) -> Result<Took, Bounce> {
        self.submit_all(base, std::slice::from_ref(change), by)
    }

    /// Accept `changes`, which participant `by` made one after another on version `base`: moved
    /// past the changes accepted since `base` (they go after them, and text they put in at the
    /// same place as one of those goes after it), each makes the next version. When one clashes
    /// with a change accepted since `base`, the hub accepts the ones before it and sends back
    /// the rest, which were made after it; the copy that made them finds the same clash, drops
    /// them and makes them again. All are sent back, leaving the hub as it was, when the first
    /// clashes, when `base` is not a version the hub can move them from, when one doesn't
    /// apply, when one puts in a block under an id the document already has, when one is
    /// more than `by`'s role lets them change (a viewer changes nothing; a commenter only
    /// comments of their own; a suggester also tracked changes in their name), when `by` isn't
    /// in the room, or when the room is closed.
    pub fn submit_all(&mut self, base: u64, changes: &[ChangeSet], by: ParticipantId) -> Result<Took, Bounce> {
        if changes.len() > MAX_BATCH {
            return Err(Bounce::Refused(format!("{} changes at once are more than the {MAX_BATCH} the hub takes", changes.len())));
        }
        if changes.is_empty() || changes.iter().any(ChangeSet::is_empty) {
            return Err(Bounce::Refused("a change that changes nothing".into()));
        }
        if self.closed {
            return Err(Bounce::Refused("the room is closed".into()));
        }
        let who = self.participant(by).cloned().ok_or_else(|| Bounce::Refused(format!("{by} isn't in the room")))?;
        if who.role == Role::Viewer {
            return Err(Bounce::Refused(format!("{} is a viewer, who changes nothing", who.name)));
        }
        let missed = self.since(base)?;
        let mut mine: Vec<ChangeSet> = changes.to_vec();
        let mut clash = None;
        // Each change accepted since goes before the copy's changes: they move past it, and it
        // moves past each of them in turn, to meet the next as that copy's document had it.
        for a in &missed {
            let mut other: ChangeSet = (*a.change).clone();
            for k in 0..mine.len() {
                let Some(c) = mine.get(k) else { break };
                match (c.rebase(&other, Side::After), other.rebase(c, Side::Before)) {
                    (Ok(c), Ok(o)) => {
                        if let Some(m) = mine.get_mut(k) {
                            *m = c;
                        }
                        other = o;
                    }
                    (Err(e), _) | (_, Err(e)) => {
                        mine.truncate(k);
                        clash = Some(Bounce::Clash { version: a.version, why: e.to_string() });
                        break;
                    }
                }
            }
        }
        if mine.is_empty() {
            return Err(clash.unwrap_or_else(|| Bounce::Refused("no change to accept".into())));
        }
        let first = self.version.checked_add(1).ok_or_else(|| Bounce::Refused("the document has had as many changes as it can count".into()))?;
        let mut doc = self.doc.clone();
        for (k, c) in mine.iter().enumerate() {
            let before = (who.role < Role::Editor).then(|| doc.clone());
            doc.apply(c)
                .map_err(|e| Bounce::Refused(format!("change {} of {} doesn't apply to version {}: {e}", k + 1, changes.len(), self.version)))?;
            if let Some(id) = doc.repeated_id(&c.ids_put_in()) {
                return Err(Bounce::Refused(format!("it puts in a block under id {id}, which the document already has")));
            }
            if let Some(before) = before {
                rules::check(who.role, &who.name, &before, &doc).map_err(|why| Bounce::Refused(format!("{}: {why}", who.name)))?;
            }
        }
        let accepted = mine.len();
        for c in mine {
            self.version = self.version.saturating_add(1);
            self.bytes = self.bytes.saturating_add(crate::session::change_weight(&c));
            self.kept.push_back(Accepted { version: self.version, by, change: Arc::new(c) });
        }
        self.doc = doc;
        // Participants' places are moved through the changes about to go first.
        if self.kept.len() > self.most || (self.bytes > MAX_KEPT_BYTES && self.kept.len() > 1) {
            self.bring_places_up();
        }
        while self.kept.len() > self.most || (self.bytes > MAX_KEPT_BYTES && self.kept.len() > 1) {
            if let Some(old) = self.kept.pop_front() {
                self.bytes = self.bytes.saturating_sub(crate::session::change_weight(&old.change));
            }
        }
        Ok(Took { version: first, accepted, moved_past: missed.len(), clash })
    }
}

/// `name` with spaces around it trimmed, when it can be a participant's: not empty, at most
/// [`MAX_NAME`] characters, no control characters.
fn valid_name(name: &str) -> Result<&str, RoomError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(RoomError::Name("a participant needs a name".into()));
    }
    if name.chars().count() > MAX_NAME {
        return Err(RoomError::Name(format!("a participant's name has {MAX_NAME} characters at most")));
    }
    if name.chars().any(char::is_control) {
        return Err(RoomError::Name("a participant's name has no control characters".into()));
    }
    Ok(name)
}

/// Whether two names are the same, whatever their case.
fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Whether two keys are the same, comparing every byte whatever the first difference.
fn same_key(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

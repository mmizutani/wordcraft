//! [`Replica`]: a participant's own copy of a room's document.

use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use wordcraft_doc::change::{Anchor, Mapped, Side};
use wordcraft_doc::{ChangeSet, Document, Pos};

use super::{Accepted, Bounce, MAX_BATCH, ParticipantId, Room};
use crate::scope::{Place, Scope, positions};
use crate::{CmdError, Selection, Session};

/// How many times a copy makes the same edit again after reading the document again before it
/// gives up on it (the hub refused it, or the copy and the hub disagreed). Edits made again
/// after a clash don't count: each clash is another participant's change accepted first.
const MAX_TRIES: u32 = 3;

/// What a copy did since it joined.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    /// Changes it sent to the hub: one for each command that changed the document.
    pub sent: u64,
    /// Of those, how many the hub accepted after moving them past changes it accepted meanwhile.
    pub moved: u64,
    /// Of those, how many the hub sent back.
    pub bounced: u64,
    /// Changes of other participants it made in its copy.
    pub received: u64,
    /// Of those, how many clashed with edits of its own the hub hadn't accepted yet.
    pub clashes: u64,
    /// Commands it ran again, after a clash or after reading the document again.
    pub made_again: u64,
    /// Edits it gave up: commands that failed when run again (or were run again too often),
    /// and changes made outside a command, which can't be.
    pub lost: u64,
    /// Times it read the whole document from the hub again.
    pub read_again: u64,
}

/// The selection as anchors: where it starts and where the caret is.
type SelAnchors = Option<(Anchor, Anchor)>;

/// A command the copy ran, with what it takes to run it again from the same place.
#[derive(Clone, Debug)]
struct Made {
    /// The command and its params; `None` for a change made outside a command.
    command: Option<(String, Value)>,
    /// The selection before and after it.
    before: SelAnchors,
    after: SelAnchors,
    /// Its params that are places in the document ([`positions`]).
    places: Vec<(String, Anchor)>,
    /// How many times it was run again after the copy read the document again.
    tries: u32,
}

impl Made {
    /// A change made outside a command.
    fn outside() -> Made {
        Made { command: None, before: None, after: None, places: Vec::new(), tries: 0 }
    }
}

/// Edits of the copy's own the hub hasn't accepted yet: their change, which applies to the
/// copy's document before them, and the commands that made them.
#[derive(Clone, Debug)]
struct Unit {
    change: ChangeSet,
    made: Vec<Made>,
    /// Which edits these are, for the copy to know them after a round trip.
    serial: u64,
}

/// The selection of session `s`, as anchors (`None` when an end isn't in a paragraph).
fn selection_anchors(s: &Session) -> SelAnchors {
    Some((s.doc.anchor_at(&s.sel.anchor)?, s.doc.anchor_at(&s.sel.focus)?))
}

/// Where anchor `a` is after change `cs`: in the text it was in, or at the start or end of the
/// block it is left beside (see [`Document::pos_of`]); `None` when its part is gone.
fn moved_anchor(a: Anchor, cs: &ChangeSet) -> Option<Anchor> {
    match cs.map(a) {
        Mapped::At(a) => Some(a),
        Mapped::Before(block) => Some(Anchor { block, off: 0 }),
        Mapped::After(block) => Some(Anchor { block, off: usize::MAX }),
        Mapped::Gone => None,
    }
}

impl Made {
    /// Move the places it keeps through `before`, a change made in the copy's document before
    /// the command, and `after`, the same change moved past the command's own.
    fn moved(&mut self, before: &ChangeSet, after: &ChangeSet) {
        let sel = |s: SelAnchors, cs: &ChangeSet| s.and_then(|(a, f)| Some((moved_anchor(a, cs)?, moved_anchor(f, cs)?)));
        self.before = sel(self.before, before);
        self.after = sel(self.after, after);
        self.places.retain_mut(|(_, at)| match moved_anchor(*at, before) {
            Some(a) => {
                *at = a;
                true
            }
            None => false,
        });
    }
}

/// A participant's copy of a room's document (see [`super`]). Its commands run at once
/// ([`Replica::run`]); [`Replica::send`] sends the edits they made to the hub,
/// [`Replica::receive`] makes the changes the hub accepted in the copy. A copy on a thread of
/// its own calls them when it likes: the copy is never wrong, only behind.
pub struct Replica {
    room: Arc<Room>,
    id: ParticipantId,
    session: Session,
    /// The session's id for the room's document, the only one the copy has open.
    document: u64,
    /// The version of the document the copy got every change up to.
    received: u64,
    /// The document as of `received`, as the hub had it.
    confirmed: Document,
    /// Its own edits the hub hasn't accepted yet, oldest first: the session's document is
    /// `confirmed` with them made in it.
    pending: Vec<Unit>,
    /// The changes of its own the hub accepted that haven't come back yet, the first ones of
    /// `pending`: the version the first of them made, and how many they are.
    sent: Option<(u64, usize)>,
    /// How many changes of the session's the copy has looked at.
    seen: u64,
    /// The next unit's serial.
    serials: u64,
    stats: Stats,
}

impl Replica {
    pub(super) fn new(room: Arc<Room>, id: ParticipantId, session: Session, version: u64, confirmed: Document) -> Replica {
        let document = session.document_id();
        let seen = session.changes().version();
        Replica {
            room,
            id,
            session,
            document,
            received: version,
            confirmed,
            pending: Vec::new(),
            sent: None,
            seen,
            serials: 0,
            stats: Stats::default(),
        }
    }

    /// Which participant this is.
    pub fn id(&self) -> ParticipantId {
        self.id
    }

    /// The room.
    pub fn room(&self) -> &Arc<Room> {
        &self.room
    }

    /// The copy's session: its document, selection and the rest. Commands go through
    /// [`Replica::run`], which keeps what they change.
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// The version of the document the copy got every change up to.
    pub fn received(&self) -> u64 {
        self.received
    }

    /// Whether the copy has edits the hub hasn't accepted, or accepted and not sent back yet.
    pub fn has_unconfirmed(&self) -> bool {
        !self.pending.is_empty()
    }

    /// What the copy did since it joined.
    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// Run command `id` in the copy ([`Session::run`]): what it changes is in the copy at once,
    /// and goes to the hub with the next [`Replica::send`]. Commands on files, other documents
    /// and windows are refused: a copy has the room's document only.
    pub fn run(&mut self, id: &str, params: &Value) -> Result<Value, CmdError> {
        match Scope::of(id) {
            Some(Scope::Document | Scope::Selection | Scope::View | Scope::App) => {}
            _ => return Err(CmdError::Disabled(format!("{id}: files, other documents and windows aren't part of a shared document"))),
        }
        self.capture();
        self.run_made(id, params, 0)
    }

    /// Run command `id`, run again `tries` times after reading the document again so far, and
    /// keep what it changed as an edit of the copy's own.
    fn run_made(&mut self, id: &str, params: &Value, tries: u32) -> Result<Value, CmdError> {
        let before = selection_anchors(&self.session);
        let doc = &self.session.doc;
        let places = positions(id)
            .iter()
            .filter(|(_, how)| *how == Place::Pos)
            .filter_map(|(key, _)| Some((key.to_string(), doc.anchor_at(&crate::cmd::parse_pos(params.get(*key)?)?)?)))
            .collect();
        let result = self.session.run(id, params);
        self.stay_in_document();
        let made = Made { command: Some((id.to_string(), params.clone())), before, after: selection_anchors(&self.session), places, tries };
        self.collect(made);
        result
    }

    /// Send the copy's edits the hub hasn't had yet to the hub, each command's change on its
    /// own, made on the version the copy got last; unless changes it sent haven't all come back
    /// yet. When the hub sends one back, the copy makes it and those after it again on the
    /// latest version.
    pub fn send(&mut self) {
        self.capture();
        if self.sent.is_some() || self.pending.is_empty() {
            return;
        }
        let n = self.pending.len().min(MAX_BATCH);
        let changes: Vec<ChangeSet> = self.pending.iter().take(n).map(|u| u.change.clone()).collect();
        let serials: Vec<u64> = self.pending.iter().take(n).map(|u| u.serial).collect();
        self.stats.sent = self.stats.sent.saturating_add(n as u64);
        let took = self.room.submit(self.received, &changes, self.id);
        let back = match took {
            Ok(took) => {
                self.sent = Some((took.version, took.accepted));
                if took.moved_past > 0 {
                    self.stats.moved = self.stats.moved.saturating_add(took.accepted as u64);
                }
                serials.get(took.accepted).copied()
            }
            Err(Bounce::Clash { .. }) => serials.first().copied(),
            Err(_) => {
                self.stats.bounced = self.stats.bounced.saturating_add(n as u64);
                self.read_again();
                return;
            }
        };
        if let Some(back) = back {
            let bounced = n.saturating_sub(serials.iter().position(|s| *s == back).unwrap_or(n));
            self.stats.bounced = self.stats.bounced.saturating_add(bounced as u64);
            // The change the first of them clashed with clashes here too: the copy drops them
            // there and makes them again.
            self.receive();
            if self.pending.iter().any(|u| u.serial == back) {
                self.read_again();
            }
        }
    }

    /// Get the changes the hub accepted since the version the copy got last: its own come
    /// back, and the others' are made in the copy, past its edits the hub hasn't accepted
    /// (which move past them). Edits of its own that clash with one are made again after it.
    pub fn receive(&mut self) {
        self.capture();
        let got = self.room.hub().since(self.received);
        match got {
            Ok(changes) => {
                for a in &changes {
                    self.integrate(a);
                }
            }
            Err(_) => self.read_again(),
        }
    }

    /// [`Replica::receive`], then [`Replica::send`].
    pub fn sync(&mut self) {
        self.receive();
        self.send();
    }

    /// Make accepted change `a` in the copy.
    fn integrate(&mut self, a: &Accepted) {
        if a.version <= self.received {
            // The copy read the document again meanwhile.
            return;
        }
        if a.version != self.received.saturating_add(1) {
            self.read_again();
            return;
        }
        if let Some((version, count)) = self.sent
            && version == a.version
        {
            // Its own change, back, as the hub moved it: the copy moved it the same way.
            let same = self.pending.first().is_some_and(|u| u.change == *a.change);
            if !same || self.confirmed.apply(&a.change).is_err() {
                self.read_again();
                return;
            }
            self.sent = (count > 1).then(|| (version.saturating_add(1), count - 1));
            self.pending.remove(0);
            self.received = a.version;
            return;
        }
        // Another participant's change: it goes before the copy's own edits, which move past it.
        // `through` has it as it meets each of them (it moves past each in turn), and after the
        // last it moved past.
        let mut m: ChangeSet = (*a.change).clone();
        let mut moved = Vec::with_capacity(self.pending.len());
        let mut through = Vec::with_capacity(self.pending.len().saturating_add(1));
        let mut clash = None;
        for (i, u) in self.pending.iter().enumerate() {
            match (u.change.rebase(&m, Side::After), m.rebase(&u.change, Side::Before)) {
                (Ok(c), Ok(n)) => {
                    moved.push(c);
                    through.push(std::mem::replace(&mut m, n));
                }
                _ => {
                    clash = Some(i);
                    break;
                }
            }
        }
        through.push(m.clone());
        // The hub accepted the changes it sent after this one, so they don't clash with it.
        if let (Some(i), Some((_, count))) = (clash, self.sent)
            && i < count
        {
            self.read_again();
            return;
        }
        if self.confirmed.apply(&a.change).is_err() {
            self.read_again();
            return;
        }
        self.received = a.version;
        self.stats.received = self.stats.received.saturating_add(1);
        let dropped = clash.map(|i| self.pending.split_off(i)).unwrap_or_default();
        for (k, (u, c)) in self.pending.iter_mut().zip(moved).enumerate() {
            u.change = c;
            // The places its commands were made at move with the text, to make them again from
            // there if a later change clashes with them.
            if let (Some(before), Some(after)) = (through.get(k), through.get(k + 1)) {
                for made in &mut u.made {
                    made.moved(before, after);
                }
            }
        }
        if dropped.is_empty() {
            if self.session.apply_change(&m).is_err() {
                self.read_again();
                return;
            }
            self.seen = self.session.changes().version();
            return;
        }
        // Its edits from the clash on go; the copy holds the change with the edits before
        // them, and makes the dropped ones again from where they were made, moved through it.
        self.stats.clashes = self.stats.clashes.saturating_add(1);
        match self.expected() {
            Some(doc) => self.make_again(doc, dropped, Some(&m), false),
            None => {
                self.pending.extend(dropped);
                self.read_again();
            }
        }
    }

    /// Read the document again from the hub, and make the copy's edits the hub hasn't accepted
    /// again on it.
    fn read_again(&mut self) {
        self.stats.read_again = self.stats.read_again.saturating_add(1);
        let (version, doc) = self.room.document();
        let mut dropped = std::mem::take(&mut self.pending);
        // The changes the hub accepted are in its document already.
        if let Some((first, count)) = self.sent.take()
            && first <= version
        {
            dropped.drain(..count.min(dropped.len()));
        }
        self.received = version;
        self.confirmed = doc.clone();
        self.make_again(doc, dropped, None, true);
    }

    /// Put `doc` in the copy and run the commands of `dropped` again: each from the selection
    /// it was made with, unless it went on from where the command before it left the caret,
    /// and with the places in its params, moved through `first` (the change the first of them
    /// clashed with) or found again by their paragraph. `read_again` when the copy read the
    /// document again, which counts as a try.
    fn make_again(&mut self, doc: Document, dropped: Vec<Unit>, first: Option<&ChangeSet>, read_again: bool) {
        let now = selection_anchors(&self.session);
        let made: Vec<Made> = dropped.into_iter().flat_map(|u| u.made).collect();
        let last = made.last().map(|m| m.after);
        self.session.reset_shared(doc);
        self.seen = self.session.changes().version();
        let mut went_on: Option<SelAnchors> = None;
        for (k, m) in made.into_iter().enumerate() {
            let through = if k == 0 { first } else { None };
            let Made { command, before, after, places, tries } = m;
            let tries = if read_again { tries.saturating_add(1) } else { tries };
            let Some((id, mut params)) = command.filter(|_| tries <= MAX_TRIES) else {
                self.stats.lost = self.stats.lost.saturating_add(1);
                went_on = Some(after);
                continue;
            };
            if went_on != Some(before)
                && let Some(sel) = self.selection_at(before, through)
            {
                self.session.sel = sel;
            }
            for (key, at) in &places {
                if let Some(p) = self.find(*at, through)
                    && let Some(o) = params.as_object_mut()
                {
                    o.insert(key.clone(), crate::cmd::pos_json(&p));
                }
            }
            went_on = Some(after);
            match self.run_made(&id, &params, tries) {
                Ok(_) => self.stats.made_again = self.stats.made_again.saturating_add(1),
                Err(e) => {
                    self.stats.lost = self.stats.lost.saturating_add(1);
                    self.session.status = format!("An edit made at the same time as another participant's couldn't be made again ({id}): {e}");
                }
            }
        }
        // The participant's own selection, when they moved it after their last edit, moved
        // through the change their first edit clashed with too.
        if last != Some(now)
            && let Some(sel) = self.selection_at(now, first)
        {
            self.session.sel = sel;
            self.session.clamp_selection();
        }
    }

    /// Where anchor `a` is now: moved through `through`, which the copy's document had made
    /// since, or found by its paragraph.
    fn find(&self, a: Anchor, through: Option<&ChangeSet>) -> Option<Pos> {
        let m = through.map_or(Mapped::At(a), |cs| cs.map(a));
        self.session.doc.place(m, None)
    }

    /// The selection `sel` names now ([`Replica::find`]).
    fn selection_at(&self, sel: SelAnchors, through: Option<&ChangeSet>) -> Option<Selection> {
        let (a, f) = sel?;
        Some(Selection { anchor: self.find(a, through)?, focus: self.find(f, through)? })
    }

    /// Take what the session changed outside [`Replica::run`] since the copy last looked as edits
    /// of its own, which can't be made again.
    fn capture(&mut self) {
        self.stay_in_document();
        self.collect(Made::outside());
    }

    /// Take the changes the session recorded since the copy last looked as edits of the copy's
    /// own: the last of them made by `made`, any before it outside a command.
    fn collect(&mut self, made: Made) {
        let changes: Vec<Arc<ChangeSet>> = match self.session.changes().since(self.seen) {
            Ok(changes) => changes.into_iter().map(|(_, cs)| cs).collect(),
            // More than the session keeps: what changed since what the copy expected.
            Err(_) => match self.expected() {
                Some(doc) => vec![Arc::new(ChangeSet::between(&doc, &self.session.doc))],
                None => {
                    self.read_again();
                    return;
                }
            },
        };
        self.seen = self.session.changes().version();
        let n = changes.len();
        for (i, cs) in changes.into_iter().enumerate() {
            if cs.is_empty() {
                continue;
            }
            let made = if i + 1 == n { made.clone() } else { Made::outside() };
            let serial = self.serial();
            self.pending.push(Unit { change: (*cs).clone(), made: vec![made], serial });
        }
    }

    /// Bring the room's document back as the session's active one, if a command made another
    /// one active (scopes keep commands that can from copies; this is a net under them).
    fn stay_in_document(&mut self) {
        if self.session.document_id() == self.document || self.session.return_to(self.document) {
            return;
        }
        // The room's document was closed: the copy starts again from the hub's.
        let (_, doc) = self.room.document();
        self.session.set_document(doc);
        self.document = self.session.document_id();
        self.seen = self.session.changes().version();
        self.read_again();
    }

    /// What the session's document should be: the version the copy got last, with the copy's
    /// own edits made in it. `None` when they don't apply.
    fn expected(&self) -> Option<Document> {
        let mut doc = self.confirmed.clone();
        for u in &self.pending {
            doc.apply(&u.change).ok()?;
        }
        Some(doc)
    }

    fn serial(&mut self) -> u64 {
        self.serials = self.serials.wrapping_add(1);
        self.serials
    }

    /// Whether the copy is as it should be: its session holds the version it got last with its
    /// own edits made in it, under the same block ids, and that version is the hub's when
    /// nothing came since.
    #[cfg(test)]
    pub(crate) fn check(&self) -> Result<(), String> {
        let doc = self.expected().ok_or("the copy's own edits don't apply to the version it got")?;
        if doc != self.session.doc {
            return Err("the session holds other content than the copy's version with its edits".into());
        }
        if doc.block_ids() != self.session.doc.block_ids() {
            return Err("the session holds its content under other block ids".into());
        }
        let (version, hub) = self.room.document();
        if version == self.received && (hub != self.confirmed || hub.block_ids() != self.confirmed.block_ids()) {
            return Err(format!("the copy's version {version} isn't the hub's"));
        }
        Ok(())
    }
}

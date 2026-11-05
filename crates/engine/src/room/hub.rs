//! [`Hub`]: the one place that puts a shared document's changes in order.

use std::collections::VecDeque;
use std::sync::Arc;

use wordcraft_doc::change::Side;
use wordcraft_doc::{ChangeSet, Document};

use super::ParticipantId;

/// Most changes a hub keeps for copies that are behind; a copy further behind reads the document
/// again.
pub const MAX_KEPT: usize = 1 << 14;

/// Most memory the kept changes may hold, roughly (as for a session's changes): the oldest go
/// first.
pub const MAX_KEPT_BYTES: usize = 64 << 20;

/// Most changes one [`Hub::submit_all`] takes.
pub const MAX_BATCH: usize = 1024;

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

/// The hub of a shared document: the document as of its latest version, and the changes that
/// made the latest versions. Changes reach it one at a time ([`super::Room`] holds it behind a
/// lock).
#[derive(Clone, Debug)]
pub struct Hub {
    doc: Document,
    version: u64,
    kept: VecDeque<Accepted>,
    /// What the kept changes weigh (as a session's changes do).
    bytes: usize,
    /// Most changes kept: [`MAX_KEPT`], fewer in tests.
    most: usize,
}

impl Hub {
    /// A hub for `doc` at version 0. Every block gets an id of its own, and the document a
    /// paragraph if it has none.
    pub fn new(mut doc: Document) -> Hub {
        doc.ensure_nonempty();
        doc.ensure_block_ids(None);
        Hub { doc, version: 0, kept: VecDeque::new(), bytes: 0, most: MAX_KEPT }
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
    /// apply, or when one puts in a block under an id the document already has.
    pub fn submit_all(&mut self, base: u64, changes: &[ChangeSet], by: ParticipantId) -> Result<Took, Bounce> {
        if changes.len() > MAX_BATCH {
            return Err(Bounce::Refused(format!("{} changes at once are more than the {MAX_BATCH} the hub takes", changes.len())));
        }
        if changes.is_empty() || changes.iter().any(ChangeSet::is_empty) {
            return Err(Bounce::Refused("a change that changes nothing".into()));
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
            doc.apply(c)
                .map_err(|e| Bounce::Refused(format!("change {} of {} doesn't apply to version {}: {e}", k + 1, changes.len(), self.version)))?;
            if let Some(id) = doc.repeated_id(&c.ids_put_in()) {
                return Err(Bounce::Refused(format!("it puts in a block under id {id}, which the document already has")));
            }
        }
        let accepted = mine.len();
        for c in mine {
            self.version = self.version.saturating_add(1);
            self.bytes = self.bytes.saturating_add(crate::session::change_weight(&c));
            self.kept.push_back(Accepted { version: self.version, by, change: Arc::new(c) });
        }
        self.doc = doc;
        while self.kept.len() > self.most || (self.bytes > MAX_KEPT_BYTES && self.kept.len() > 1) {
            if let Some(old) = self.kept.pop_front() {
                self.bytes = self.bytes.saturating_sub(crate::session::change_weight(&old.change));
            }
        }
        Ok(Took { version: first, accepted, moved_past: missed.len(), clash })
    }
}

//! The recent changes of each open document ([`ChangeLog`]), which agents and other copies of
//! the document follow (`document.changes`, MCP `get_changes`).

use std::collections::VecDeque;
use std::sync::Arc;

use wordcraft_doc::change::{FieldChange, PartChange};
use wordcraft_doc::{Block, ChangeSet, Document};

use super::Session;

/// Most changes a document keeps.
pub const MAX_CHANGES: usize = 1024;

/// Most memory the kept changes may hold, roughly ([`weight`]): the blocks, pictures and whole
/// style sheets, lists or settings they put in, which they keep alive after the document and
/// its undo steps let go of them. The oldest go first.
pub const MAX_CHANGE_BYTES: usize = 64 << 20;

/// A document's changes since it was opened: how many, the latest of them (at most
/// [`MAX_CHANGES`], holding at most about [`MAX_CHANGE_BYTES`]), each with the version it made,
/// and the document as of the latest. Whatever a command changes is one change (a command run
/// inside another adds none of its own), undo and redo included; so is a snapshot put back, or
/// the metadata a save stamps, outside a command.
#[derive(Clone, Debug)]
pub struct ChangeLog {
    version: u64,
    recent: VecDeque<(u64, Arc<ChangeSet>)>,
    /// What the kept changes weigh ([`weight`]).
    bytes: usize,
    /// The document at `version` (sharing its unchanged blocks with the open one): what the
    /// next change is taken against, and what a new copy starts from.
    base: Document,
}

impl ChangeLog {
    /// The changes of `doc`, which has had none yet.
    pub fn new(doc: &Document) -> ChangeLog {
        ChangeLog { version: 0, recent: VecDeque::new(), bytes: 0, base: doc.clone() }
    }

    /// How many changes the document has had since it was opened (or replaced, which starts
    /// the count again under a new document id).
    pub fn version(&self) -> u64 {
        self.version
    }

    /// The document as of [`Self::version`]: with its block ids, what a new copy starts from
    /// to follow the changes after it.
    pub fn document(&self) -> &Document {
        &self.base
    }

    fn push(&mut self, cs: ChangeSet, now: Document) {
        self.version = self.version.saturating_add(1);
        self.bytes = self.bytes.saturating_add(weight(&cs));
        self.recent.push_back((self.version, Arc::new(cs)));
        self.base = now;
        while self.recent.len() > MAX_CHANGES || (self.bytes > MAX_CHANGE_BYTES && self.recent.len() > 1) {
            if let Some((_, old)) = self.recent.pop_front() {
                self.bytes = self.bytes.saturating_sub(weight(&old));
            }
        }
    }

    /// The changes after version `since`, oldest first, each with the version it made. An error
    /// when `since` is ahead of the document, or older than the changes it keeps.
    pub fn since(&self, since: u64) -> Result<Vec<(u64, Arc<ChangeSet>)>, String> {
        if since > self.version {
            return Err(format!("version {since} is ahead of the document, which is at version {}", self.version));
        }
        let oldest = self.recent.front().map_or(self.version, |(v, _)| v.saturating_sub(1));
        if since < oldest {
            return Err(format!(
                "the changes before version {} are no longer kept (the last {MAX_CHANGES} are): read the document again",
                oldest.saturating_add(1)
            ));
        }
        Ok(self.recent.iter().filter(|(v, _)| *v > since).cloned().collect())
    }
}

/// About how much memory change `cs` holds: the characters and blocks it puts in, the pictures
/// and other files, and a flat amount for each style sheet, list set, settings or glossary it
/// replaces whole (estimates: the cap is a bound on the memory followers' changes keep, not a
/// count).
fn weight(cs: &ChangeSet) -> usize {
    const WHOLE: usize = 64 << 10;
    const SMALL: usize = 1 << 10;
    let blocks = |bl: &mut dyn Iterator<Item = &Arc<Block>>| bl.map(|b| block_weight(b, 0)).fold(0usize, usize::saturating_add);
    let edits = cs
        .edits
        .iter()
        .map(|e| {
            let chars = e.chars.as_ref().map_or(0, |c| {
                let ins = &c.inserted;
                ins.text.len().saturating_add(ins.runs.len().saturating_mul(64)).saturating_add(ins.objects.len().saturating_mul(256))
            });
            chars.saturating_add(if e.shell.is_some() { SMALL } else { 0 }).saturating_add(64)
        })
        .fold(0usize, usize::saturating_add);
    let splices = cs
        .splices
        .iter()
        .map(|s| blocks(&mut s.inserted.iter().map(|b| &b.0)).saturating_add(s.text.inserted.len()))
        .fold(0usize, usize::saturating_add);
    let parts = cs
        .parts
        .iter()
        .map(|p| match p {
            PartChange::Added { blocks: bl, .. } => blocks(&mut bl.iter().map(|b| &b.0)),
            _ => SMALL,
        })
        .fold(0usize, usize::saturating_add);
    let fields = cs
        .fields
        .iter()
        .map(|f| match f {
            FieldChange::Media { bytes, .. } | FieldChange::Passthrough { bytes, .. } => bytes.as_ref().map_or(0, |b| b.len()),
            FieldChange::Styles(_) | FieldChange::Numbering(_) | FieldChange::Settings(_) | FieldChange::Glossary(_) => WHOLE,
            FieldChange::Revisions { append, .. } => append.len().saturating_mul(SMALL),
            _ => SMALL,
        })
        .fold(0usize, usize::saturating_add);
    edits.saturating_add(splices).saturating_add(parts).saturating_add(fields).saturating_add(cs.tables.len().saturating_mul(SMALL))
}

/// About how much memory block `b` holds.
fn block_weight(b: &Block, depth: usize) -> usize {
    const BLOCK: usize = 256;
    match b {
        Block::Para(p) => {
            p.text.len().saturating_add(p.runs.len().saturating_mul(64)).saturating_add(p.objects.len().saturating_mul(BLOCK)).saturating_add(BLOCK)
        }
        // Past the deepest nesting ids go to, the rest is counted flat.
        Block::Table(_) if depth > 64 => BLOCK,
        Block::Table(t) => t
            .rows
            .iter()
            .flat_map(|r| &r.cells)
            .flat_map(|c| &c.blocks)
            .map(|b| block_weight(b, depth + 1).saturating_add(BLOCK))
            .fold(BLOCK, usize::saturating_add),
    }
}

/// Whether `a` and `b` hold the very same blocks, not copies of them.
fn same_blocks(a: &Document, b: &Document) -> bool {
    let same = |x: &[Arc<Block>], y: &[Arc<Block>]| x.len() == y.len() && x.iter().zip(y).all(|(p, q)| Arc::ptr_eq(p, q));
    same(&a.body, &b.body)
        && a.parts.len() == b.parts.len()
        && a.parts.iter().zip(&b.parts).all(|((i, p), (j, q))| i == j && same(&p.blocks, &q.blocks))
}

impl Session {
    /// The active document's changes.
    pub fn changes(&self) -> &ChangeLog {
        &self.changes
    }

    /// Record what changed in the active document since its latest change, as one change
    /// (nothing when nothing did). [`Session::run`] calls it when a command ends, unless a
    /// command on the same document is still running; so does whatever changes the document
    /// outside a command.
    pub(crate) fn record_changes(&mut self) {
        let mut cs = ChangeSet::between(&self.changes.base, &self.doc);
        // Commands give the blocks they make ids, and undo and redo bring back blocks with
        // theirs; a block that has none all the same gets one before it travels.
        if cs.puts_in_unnamed_blocks() {
            self.doc.ensure_block_ids(None);
            cs = ChangeSet::between(&self.changes.base, &self.doc);
        }
        if !cs.is_empty() {
            self.changes.push(cs, self.doc.clone());
        } else if !same_blocks(&self.changes.base, &self.doc) {
            // Copies of the blocks, holding what they held: share the open document's, so the
            // next look compares pointers again.
            self.changes.base = self.doc.clone();
        }
    }

    /// Start the active document's changes again from what it holds now (tests that watch
    /// what else keeps old versions of the document alive).
    #[cfg(test)]
    pub(crate) fn forget_changes(&mut self) {
        self.changes = ChangeLog::new(&self.doc);
    }

    /// Whether no command on the active document is running: what changes it now records
    /// its own change.
    pub(crate) fn outside_commands(&self) -> bool {
        !self.command_docs.contains(&self.document_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn some_change() -> ChangeSet {
        let mut cs = ChangeSet::default();
        cs.fields.push(wordcraft_doc::change::FieldChange::Core(Default::default()));
        cs
    }

    #[test]
    fn since_gives_the_changes_after_a_version_and_refuses_what_it_no_longer_has() {
        let mut log = ChangeLog::new(&Document::default());
        assert_eq!(log.since(0).map(|v| v.len()), Ok(0));
        assert!(log.since(1).is_err());
        for _ in 0..3 {
            log.push(some_change(), Document::default());
        }
        assert_eq!(log.version(), 3);
        assert_eq!(log.since(1).map(|v| v.iter().map(|(v, _)| *v).collect::<Vec<_>>()), Ok(vec![2, 3]));
        assert_eq!(log.since(3).map(|v| v.len()), Ok(0));
        for _ in 0..MAX_CHANGES {
            log.push(some_change(), Document::default());
        }
        assert_eq!(log.recent.len(), MAX_CHANGES);
        assert!(log.since(2).is_err());
        assert_eq!(log.since(3).map(|v| v.len()), Ok(MAX_CHANGES));
    }
}

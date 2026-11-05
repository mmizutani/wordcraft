//! [`ChangeSet::map`]: where a place in the text is after a change.

use serde::{Deserialize, Serialize};

use super::{ChangeSet, MARK, NewBlock, PartChange, Splice, inside_ids, window_len};
use crate::{Block, BlockId, Document, Path, Pos, StoryRef};

/// A place in a paragraph: its id, and a byte offset in its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Anchor {
    pub block: BlockId,
    pub off: usize,
}

/// Where an [`Anchor`] is after a change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mapped {
    /// In this paragraph at this offset: where the text it was in went, or where it was when
    /// the change didn't touch its paragraph.
    At(Anchor),
    /// The text around it is gone and nothing took its place: it belongs at the start of this
    /// block.
    Before(BlockId),
    /// … or, at the end of its list, at the end of this block.
    After(BlockId),
    /// Its part (a note, a comment, a text box) is gone.
    Gone,
}

impl ChangeSet {
    /// Where anchor `a` is after this change. Text past a paragraph split follows its text into
    /// the new paragraph; an anchor in text the change replaced moves to where the replacement
    /// starts, and one in text whose formatting alone changed stays with its characters; an
    /// anchor where text was put in stays before it; one in a table the change replaced whole
    /// moves to where the table was.
    pub fn map(&self, a: Anchor) -> Mapped {
        for e in &self.edits {
            if e.id == a.block {
                let off = e.chars.as_ref().map_or(a.off, |c| {
                    let (at, removed, inserted) = c.replaced();
                    map_pos(a.off, at, removed, inserted)
                });
                return Mapped::At(Anchor { block: a.block, off });
            }
        }
        for s in &self.splices {
            // A paragraph in a table the change replaced whole that is in the table put there
            // (a row put in, a cell's properties changed) is still there: the anchor stays in it.
            if s.removed.iter().any(|old| old.inside.contains(&a.block)) && s.inserted.iter().any(|b| inside_ids(&b.0).contains(&a.block)) {
                return Mapped::At(a);
            }
            if let Some(w) = window_pos(s, a) {
                return resolve(s, map_pos(w, s.text.at, s.text.removed, s.text.inserted.len()));
            }
        }
        for p in &self.parts {
            if let PartChange::Removed { blocks, .. } = p
                && blocks.iter().any(|b| b.id == a.block || b.inside.contains(&a.block))
            {
                return Mapped::Gone;
            }
        }
        Mapped::At(a)
    }
}

impl Document {
    /// The anchor of place `p`: its paragraph's id, and its offset there (moved back to a
    /// character boundary in the text). `None` when no paragraph is at `p`.
    pub fn anchor_at(&self, p: &Pos) -> Option<Anchor> {
        self.para_at(p).map(|para| Anchor { block: para.id, off: para.clamp(p.off) })
    }

    /// Where anchor `a` is in this document: in the paragraph with its id, at its offset (moved
    /// back to a character boundary in the text). The paragraph at `hint`, where the anchor
    /// was before, is looked at first. An anchor at a table is at the start of its first
    /// paragraph, or at the end of its last when its offset is past 0. `None` when no block has
    /// the id.
    pub fn pos_of(&self, a: Anchor, hint: Option<&Pos>) -> Option<Pos> {
        if let Some(h) = hint
            && let Some(p) = self.para_at(h)
            && p.id == a.block
        {
            return Some(Pos { story: h.story, path: h.path.clone(), off: p.clamp(a.off) });
        }
        let mut found = None;
        let mut table = false;
        self.each_block(&mut |story, path, b| {
            if found.is_none() && b.id() == a.block {
                match &**b {
                    Block::Para(p) => found = Some(Pos { story, path: Path(path.to_vec()), off: p.clamp(a.off) }),
                    Block::Table(_) => table = true,
                }
            }
        });
        if table && found.is_none() {
            return self.block_edge(a.block, a.off > 0);
        }
        found
    }

    /// Where an anchor [`ChangeSet::map`] mapped through a change this document has had now
    /// is: [`Mapped::At`] as [`Document::pos_of`] finds it; [`Mapped::Before`] and
    /// [`Mapped::After`] at the start or end of the block (of the first or last paragraph in a
    /// table). `None` when it is gone, or the change didn't come from this document.
    pub fn place(&self, m: Mapped, hint: Option<&Pos>) -> Option<Pos> {
        match m {
            Mapped::At(a) => self.pos_of(a, hint),
            Mapped::Before(id) => self.block_edge(id, false),
            Mapped::After(id) => self.block_edge(id, true),
            Mapped::Gone => None,
        }
    }

    /// The start (`end` false) or the end of block `id`: of the paragraph, or of the first or
    /// last paragraph in the table.
    fn block_edge(&self, id: BlockId, end: bool) -> Option<Pos> {
        let mut at: Option<(StoryRef, Path)> = None;
        self.each_block(&mut |story, path, b| {
            if at.is_none() && b.id() == id {
                at = Some((story, Path(path.to_vec())));
            }
        });
        let (story, path) = at?;
        let path = match self.block(story, &path)? {
            Block::Para(_) => path,
            Block::Table(_) => {
                let mut inside = self.para_paths(story).into_iter().filter(|q| q.0.len() > path.0.len() && q.0.starts_with(&path.0));
                if end { inside.next_back() } else { inside.next() }?
            }
        };
        let off = if end { self.para(story, &path)?.len() } else { 0 };
        Some(Pos { story, path, off })
    }
}

/// Where `a` is in the window splice `s` replaced (its blocks' text with a [`MARK`] between),
/// if it is in one of them; an anchor in a table counts as being at the table.
fn window_pos(s: &Splice, a: Anchor) -> Option<usize> {
    let mut pos = 0usize;
    for old in &s.removed {
        if old.id == a.block {
            return Some(pos.saturating_add(a.off.min(old.len)));
        }
        if old.inside.contains(&a.block) {
            return Some(pos);
        }
        pos = pos.saturating_add(old.len).saturating_add(MARK.len_utf8());
    }
    None
}

/// Where position `w` is after `removed` bytes from `at` became `inserted` bytes.
fn map_pos(w: usize, at: usize, removed: usize, inserted: usize) -> usize {
    let end = at.saturating_add(removed);
    if w <= at {
        w
    } else if w < end {
        at
    } else {
        w.saturating_sub(removed).saturating_add(inserted)
    }
}

/// The block and offset at window position `w` of the blocks splice `s` put in.
fn resolve(s: &Splice, w: usize) -> Mapped {
    let mut pos = 0usize;
    for NewBlock(b) in &s.inserted {
        let len = window_len(b);
        if w <= pos.saturating_add(len) {
            return match &**b {
                Block::Para(_) => Mapped::At(Anchor { block: b.id(), off: w.saturating_sub(pos) }),
                Block::Table(_) => first_para(b).map_or(Mapped::Before(b.id()), |block| Mapped::At(Anchor { block, off: 0 })),
            };
        }
        pos = pos.saturating_add(len).saturating_add(MARK.len_utf8());
        if w < pos {
            // Inside the paragraph mark after it: the end of the block.
            return match &**b {
                Block::Para(p) => Mapped::At(Anchor { block: b.id(), off: p.text.len() }),
                Block::Table(_) => Mapped::After(b.id()),
            };
        }
    }
    match (s.before, s.after) {
        (Some(b), _) => Mapped::Before(b),
        (None, Some(a)) => Mapped::After(a),
        // A list with nothing in it, which no document has.
        (None, None) => Mapped::Gone,
    }
}

/// The first paragraph inside table `b`, in document order.
fn first_para(b: &Block) -> Option<BlockId> {
    fn first(t: &crate::Table, depth: usize) -> Option<BlockId> {
        if depth > crate::block_id::MAX_ID_DEPTH {
            return None;
        }
        for b in t.rows.iter().flat_map(|r| &r.cells).flat_map(|c| &c.blocks) {
            let found = match &**b {
                Block::Para(p) => Some(p.id),
                Block::Table(inner) => first(inner, depth + 1),
            };
            if found.is_some() {
                return found;
            }
        }
        None
    }
    match b {
        Block::Table(t) => first(t, 1),
        Block::Para(p) => Some(p.id),
    }
}

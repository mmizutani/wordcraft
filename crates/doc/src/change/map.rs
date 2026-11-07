//! [`ChangeSet::map`]: where a place in the text is after a change.

use serde::{Deserialize, Serialize};

use super::{ChangeSet, Fingerprint, MARK, NewBlock, PartChange, Splice, text_fingerprint, window_len};
use crate::block_id::each_inside;
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
    /// stays in its paragraph when the table put there has it with the same text, is gone when
    /// that text changed, and otherwise moves to where the table was.
    pub fn map(&self, a: Anchor) -> Mapped {
        self.map_with(a, false)
    }

    /// [`ChangeSet::map`], but an anchor where text was put in (and none taken out) moves past
    /// that text: the start of text that was there, which what was put in front of it isn't
    /// part of.
    pub fn map_past(&self, a: Anchor) -> Mapped {
        self.map_with(a, true)
    }

    fn map_with(&self, a: Anchor, past: bool) -> Mapped {
        for e in &self.edits {
            if e.id == a.block {
                let off = e.chars.as_ref().map_or(a.off, |c| {
                    let (at, removed, inserted) = c.replaced();
                    map_pos(a.off, at, removed, inserted, past)
                });
                return Mapped::At(Anchor { block: a.block, off });
            }
        }
        for s in &self.splices {
            // A paragraph in a table the change replaced whole that is in the table put there
            // (a row put in, a cell's properties changed) is still there: the anchor stays in it
            // when its text is the same. Whose text changed too, the change doesn't say how,
            // so where in it the anchor is now isn't known.
            if let Some(was) =
                s.removed.iter().find_map(|old| old.inside.iter().position(|id| *id == a.block).map(|i| old.inside_text.get(i).copied()))
                && let Some(now) = s.inserted.iter().find_map(|b| text_inside(&b.0, a.block))
            {
                return if was == Some(now) { Mapped::At(a) } else { Mapped::Gone };
            }
            if let Some(w) = window_pos(s, a) {
                return resolve(s, map_pos(w, s.text.at, s.text.removed, s.text.inserted.len(), past), false);
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

impl Splice {
    /// Where the text this splice put in is, in the blocks it put in, as the start and the end
    /// of that text ([`TextSplice`](super::TextSplice)): where it only took text out, the place
    /// that text was (both ends the same); where its blocks' text didn't change (a table's
    /// cells, say), its blocks whole. A table is placed at the start of its first paragraph and
    /// the end of its last. `None` when it put in no block.
    pub fn put_in(&self) -> Option<(Mapped, Mapped)> {
        if self.inserted.is_empty() {
            return None;
        }
        let (from, to) = if self.text.removed == 0 && self.text.inserted.is_empty() {
            let marks = MARK.len_utf8().saturating_mul(self.inserted.len().saturating_sub(1));
            (0, self.inserted.iter().fold(marks, |n, b| n.saturating_add(window_len(&b.0))))
        } else {
            (self.text.at, self.text.at.saturating_add(self.text.inserted.len()))
        };
        Some((resolve(self, from, false), resolve(self, to, true)))
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

/// Where position `w` is after `removed` bytes from `at` became `inserted` bytes: before text
/// put in right at it, or with `past`, after it.
fn map_pos(w: usize, at: usize, removed: usize, inserted: usize, past: bool) -> usize {
    let end = at.saturating_add(removed);
    if w < at || (w == at && !(past && removed == 0)) {
        w
    } else if w < end {
        at
    } else {
        w.saturating_sub(removed).saturating_add(inserted)
    }
}

/// The block and offset at window position `w` of the blocks splice `s` put in. A place in a
/// table is at the start of its first paragraph, or with `end`, at the end of its last.
fn resolve(s: &Splice, w: usize, end: bool) -> Mapped {
    let mut pos = 0usize;
    let mut table_before = None;
    for NewBlock(b) in &s.inserted {
        // The end of text that ends with a table and the mark after it: the table's end.
        if end
            && w == pos
            && let Some(t) = table_before
        {
            return edge_para(t, true).map_or(Mapped::Before(b.id()), |block| Mapped::At(Anchor { block, off: usize::MAX }));
        }
        let len = window_len(b);
        if w <= pos.saturating_add(len) {
            return match &**b {
                Block::Para(_) => Mapped::At(Anchor { block: b.id(), off: w.saturating_sub(pos) }),
                Block::Table(_) if end => edge_para(b, true).map_or(Mapped::After(b.id()), |block| Mapped::At(Anchor { block, off: usize::MAX })),
                Block::Table(_) => first_para(b).map_or(Mapped::Before(b.id()), |block| Mapped::At(Anchor { block, off: 0 })),
            };
        }
        pos = pos.saturating_add(len).saturating_add(MARK.len_utf8());
        table_before = matches!(&**b, Block::Table(_)).then_some(&**b);
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

/// [`text_fingerprint`] of block `id` inside table `b`, if it is there.
fn text_inside(b: &Block, id: BlockId) -> Option<Fingerprint> {
    let mut found = None;
    each_inside(b, &mut |inner| {
        if found.is_none() && inner.id() == id {
            found = Some(text_fingerprint(inner));
        }
    });
    found
}

/// The first paragraph inside table `b`, in document order.
fn first_para(b: &Block) -> Option<BlockId> {
    edge_para(b, false)
}

/// The first paragraph inside table `b`, or with `last` the last.
fn edge_para(b: &Block, last: bool) -> Option<BlockId> {
    fn edge(t: &crate::Table, depth: usize, last: bool) -> Option<BlockId> {
        if depth > crate::block_id::MAX_ID_DEPTH {
            return None;
        }
        let mut blocks: Vec<&Block> = t.rows.iter().flat_map(|r| &r.cells).flat_map(|c| &c.blocks).map(|b| &**b).collect();
        if last {
            blocks.reverse();
        }
        for b in blocks {
            let found = match b {
                Block::Para(p) => Some(p.id),
                Block::Table(inner) => edge(inner, depth + 1, last),
            };
            if found.is_some() {
                return found;
            }
        }
        None
    }
    match b {
        Block::Table(t) => edge(t, 1, last),
        Block::Para(p) => Some(p.id),
    }
}

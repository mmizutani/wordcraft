//! Block identity: which paragraph or table a block is, whatever it holds.
//!
//! Every paragraph and table in a document's stories (the body, the headers, footers, notes,
//! comments and text boxes, and the blocks in their table cells) has a [`BlockId`] of its own.
//! Editing a block keeps its id. A block a command makes, or copies from another, gets a new one
//! from [`Document::ensure_block_ids`], which the session runs after every command and when it
//! opens a document; a block a command took away and made again, holding the same, gets its id
//! back. Co-editing will name blocks by these ids: a change made in one copy of a document says
//! which blocks it touched, and another copy applies it to the same blocks. So the ids are
//! random, not counted from 1: copies that add blocks at the same time don't make the same ids.
//!
//! An id isn't part of what a block holds: `==` ignores it, files don't store it (a document read
//! from one gets new ids), and the markup kept from a file doesn't depend on it.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use crate::{Block, Blocks, Document, Path, StoryRef};

/// Which block this is (see the [module](self) docs). [`BlockId::UNSET`] until
/// [`Document::ensure_block_ids`] gives the block one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(u64);

impl BlockId {
    /// No id yet: a block just made.
    pub const UNSET: BlockId = BlockId(0);

    /// A new id. Ids made in one process never repeat; ids made in two processes repeat with a
    /// chance of about one in 10^13 when each makes a million.
    pub fn fresh() -> BlockId {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let next = || mix(seed().wrapping_add(NEXT.fetch_add(1, Ordering::Relaxed)));
        let id = next();
        // `mix` is one-to-one, so only one count comes out as 0, and the next one can't.
        BlockId(if id == 0 { next() } else { id })
    }

    /// Whether the block has an id (not [`BlockId::UNSET`]).
    pub fn is_set(self) -> bool {
        self != BlockId::UNSET
    }
}

/// Where this process's ids start: random, so that two processes (two people's copies of a
/// document) make different ids.
fn seed() -> u64 {
    static SEED: OnceLock<u64> = OnceLock::new();
    *SEED.get_or_init(|| {
        let mut bytes = [0u8; 8];
        if getrandom::fill(&mut bytes).is_ok() {
            return u64::from_le_bytes(bytes);
        }
        // No system randomness: the hasher's own keys (random on most systems) and where this
        // runs in memory.
        use std::hash::BuildHasher;
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_usize(std::ptr::from_ref(&bytes).addr());
        h.finish()
    })
}

/// SplitMix64's finaliser: scrambles the 64 bits, one to one.
fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Hashes an id as it is: ids are random already, and the set of them is built after every
/// command.
#[derive(Default)]
struct IdHasher(u64);

impl Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(*b);
        }
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = n;
    }
}

type IdSet = HashSet<u64, BuildHasherDefault<IdHasher>>;

/// Tables nested deeper than this aren't walked. Edits stop at [`crate::table::MAX_TABLE_DEPTH`]
/// and the readers soon after, so every block a document holds is well within it; the bound
/// keeps a hostile document from overflowing the stack.
const MAX_ID_DEPTH: usize = 64;

/// [`Document::each_block`] for one block list.
fn walk<'a>(bl: &'a [Arc<Block>], story: StoryRef, path: &mut Vec<u32>, depth: usize, f: &mut dyn FnMut(StoryRef, &[u32], &'a Arc<Block>)) {
    if depth > MAX_ID_DEPTH {
        return;
    }
    for (i, b) in bl.iter().enumerate() {
        path.push(i as u32);
        f(story, path, b);
        if let Block::Table(t) = &**b {
            for (r, row) in t.rows.iter().enumerate() {
                for (c, cell) in row.cells.iter().enumerate() {
                    path.push(r as u32);
                    path.push(c as u32);
                    walk(&cell.blocks, story, path, depth + 1, f);
                    path.pop();
                    path.pop();
                }
            }
        }
        path.pop();
    }
}

impl Document {
    /// Every block of the stories, in document order: the body, then the parts by id, and a
    /// table before the blocks in its cells. `f` gets each block's story and path (as in
    /// [`Path`]).
    pub fn each_block<'a>(&'a self, f: &mut dyn FnMut(StoryRef, &[u32], &'a Arc<Block>)) {
        let mut path = Vec::new();
        walk(&self.body, StoryRef::Body, &mut path, 0, f);
        for (id, part) in &self.parts {
            walk(&part.blocks, StoryRef::Part(*id), &mut path, 0, f);
        }
    }

    /// Give every block of the stories an id of its own, and return how many blocks got one.
    ///
    /// With no `before`, every block is looked at: one without an id gets one, and of blocks
    /// sharing an id, all but the first get new ones.
    ///
    /// With `before`, the document as it was before an edit (with ids of its own, as this
    /// leaves them), only what the edit changed is looked at: the blocks between those both
    /// versions share (the same allocations, which keep their ids), in each story and in each
    /// cell of a table edited in place. Among those, a block keeps its id if the changed blocks
    /// of `before` had it and no other changed block keeps it first (the one `before` held
    /// unchanged comes first). Any other block gets the id of a changed block of `before` that
    /// held the same and is gone now, from the same story if there is one (a command that
    /// deletes a paragraph and makes it again, or moves text into a text box, leaves it the
    /// same paragraph), or else a new one. Copies of blocks the edit didn't change, and blocks
    /// from elsewhere, get new ids.
    pub fn ensure_block_ids(&mut self, before: Option<&Document>) -> usize {
        let assign = match before {
            Some(was) => changed_ids(was, self),
            None => self.unnamed().into_iter().map(|(story, path)| (story, path, BlockId::fresh())).collect(),
        };
        for (story, path, id) in &assign {
            if let Ok(bl) = self.container_mut(*story, path)
                && let Some(b) = bl.get_mut(path.last())
            {
                *Arc::make_mut(b).id_mut() = *id;
            }
        }
        assign.len()
    }

    /// The blocks [`Document::ensure_block_ids`] gives ids with no `before`: those without one,
    /// and all but the first of the blocks sharing one.
    fn unnamed(&self) -> Vec<(StoryRef, Path)> {
        let mut seen = IdSet::default();
        let mut out = Vec::new();
        self.each_block(&mut |story, path, b| {
            let id = b.id();
            if !id.is_set() || !seen.insert(id.0) {
                out.push((story, Path(path.to_vec())));
            }
        });
        out
    }
}

/// The blocks an edit changed, from [`windows`]: the edited version's and the earlier
/// version's, each in document order.
#[derive(Default)]
struct Changed<'a> {
    now: Vec<(StoryRef, Path, &'a Arc<Block>)>,
    was: Vec<(StoryRef, &'a Arc<Block>)>,
}

impl<'a> Changed<'a> {
    /// `b` and the blocks inside it, at `path`, as changed in the edited version.
    fn now(&mut self, b: &'a Arc<Block>, story: StoryRef, path: &mut Vec<u32>, depth: usize) {
        self.now.push((story, Path(path.clone()), b));
        if let Block::Table(t) = &**b {
            for (r, row) in t.rows.iter().enumerate() {
                for (c, cell) in row.cells.iter().enumerate() {
                    path.push(r as u32);
                    path.push(c as u32);
                    walk(&cell.blocks, story, path, depth + 1, &mut |story, p, b| self.now.push((story, Path(p.to_vec()), b)));
                    path.pop();
                    path.pop();
                }
            }
        }
    }

    /// `b` and the blocks inside it, as changed in the earlier version.
    fn was(&mut self, b: &'a Arc<Block>, story: StoryRef, depth: usize) {
        walk(std::slice::from_ref(b), story, &mut Vec::new(), depth, &mut |story, _, b| self.was.push((story, b)));
    }
}

/// What changed from `b` to `a`, the earlier and edited block lists at `path` in `story`: the
/// blocks between their common start and end (the same allocations), and inside a table edited
/// in place (the same id and shape), what changed in each cell.
fn windows<'a>(a: &'a Blocks, b: &'a Blocks, story: StoryRef, path: &mut Vec<u32>, depth: usize, out: &mut Changed<'a>) {
    if depth > MAX_ID_DEPTH {
        return;
    }
    let start = a.iter().zip(b).take_while(|(x, y)| Arc::ptr_eq(x, y)).count();
    let room = a.len().min(b.len()).saturating_sub(start);
    let end = a.iter().rev().zip(b.iter().rev()).take(room).take_while(|(x, y)| Arc::ptr_eq(x, y)).count();
    let (na, nb) = (a.len().saturating_sub(start + end), b.len().saturating_sub(start + end));
    let changed_a = a.iter().enumerate().skip(start).take(na);
    if na != nb {
        for (i, x) in changed_a {
            path.push(i as u32);
            out.now(x, story, path, depth);
            path.pop();
        }
        for y in b.iter().skip(start).take(nb) {
            out.was(y, story, depth);
        }
        return;
    }
    // As many blocks before as after: each against the one in its place.
    for ((i, x), y) in changed_a.zip(b.iter().skip(start)) {
        if Arc::ptr_eq(x, y) {
            continue;
        }
        path.push(i as u32);
        match (&**x, &**y) {
            (Block::Table(tx), Block::Table(ty)) if tx.id == ty.id && same_shape(tx, ty) => {
                out.now.push((story, Path(path.clone()), x));
                out.was.push((story, y));
                for (r, (rx, ry)) in tx.rows.iter().zip(&ty.rows).enumerate() {
                    for (c, (cx, cy)) in rx.cells.iter().zip(&ry.cells).enumerate() {
                        // Most cells hold the blocks they held (typing in one cell of many).
                        if cx.blocks.len() == cy.blocks.len() && cx.blocks.iter().zip(&cy.blocks).all(|(p, q)| Arc::ptr_eq(p, q)) {
                            continue;
                        }
                        path.push(r as u32);
                        path.push(c as u32);
                        windows(&cx.blocks, &cy.blocks, story, path, depth + 1, out);
                        path.pop();
                        path.pop();
                    }
                }
            }
            _ => {
                out.now(x, story, path, depth);
                out.was(y, story, depth);
            }
        }
        path.pop();
    }
}

/// Whether two tables have the same rows with the same number of cells each.
fn same_shape(a: &crate::Table, b: &crate::Table) -> bool {
    a.rows.len() == b.rows.len() && a.rows.iter().zip(&b.rows).all(|(x, y)| x.cells.len() == y.cells.len())
}

/// [`Document::ensure_block_ids`] with an earlier version: the ids to give, by story and path.
fn changed_ids(before: &Document, after: &Document) -> Vec<(StoryRef, Path, BlockId)> {
    let mut changed = Changed::default();
    let mut path = Vec::new();
    windows(&after.body, &before.body, StoryRef::Body, &mut path, 0, &mut changed);
    // Parts: in both, only in the edited version (all new), only in the earlier one (all gone).
    let (mut a, mut b) = (after.parts.iter().peekable(), before.parts.iter().peekable());
    loop {
        let side = match (a.peek(), b.peek()) {
            (None, None) => break,
            (Some((ka, _)), Some((kb, _))) => ka.cmp(kb),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
        };
        match side {
            std::cmp::Ordering::Equal => {
                if let (Some((k, pa)), Some((_, pb))) = (a.next(), b.next()) {
                    windows(&pa.blocks, &pb.blocks, StoryRef::Part(*k), &mut path, 0, &mut changed);
                }
            }
            std::cmp::Ordering::Less => {
                if let Some((k, pa)) = a.next() {
                    for (i, x) in pa.blocks.iter().enumerate() {
                        path.push(i as u32);
                        changed.now(x, StoryRef::Part(*k), &mut path, 0);
                        path.pop();
                    }
                }
            }
            std::cmp::Ordering::Greater => {
                if let Some((k, pb)) = b.next() {
                    for y in &pb.blocks {
                        changed.was(y, StoryRef::Part(*k), 0);
                    }
                }
            }
        }
    }
    let Changed { now, was } = changed;
    // The changed blocks of `before` by id (the first, if `before` shared one).
    let mut held: HashMap<u64, &Arc<Block>, BuildHasherDefault<IdHasher>> = HashMap::default();
    for (_, b) in &was {
        if b.id().is_set() {
            held.entry(b.id().0).or_insert(b);
        }
    }
    // Which changed block keeps each of those ids: the one `before` held as it is, else the
    // first.
    let mut keeper: HashMap<u64, usize, BuildHasherDefault<IdHasher>> = HashMap::default();
    for (i, (_, _, b)) in now.iter().enumerate() {
        let id = b.id().0;
        let Some(was) = held.get(&id) else { continue };
        let first = keeper.entry(id).or_insert(i);
        if Arc::ptr_eq(b, was) && !now.get(*first).is_some_and(|k| Arc::ptr_eq(k.2, was)) {
            *first = i;
        }
    }
    // The changed blocks of `before` whose id nobody keeps, in document order, by a cheap key;
    // compared in full on a match.
    let mut gone: HashMap<u64, Vec<(StoryRef, &Arc<Block>)>> = HashMap::new();
    for (story, b) in was {
        if b.id().is_set() && !keeper.contains_key(&b.id().0) {
            gone.entry(content_key(b)).or_default().push((story, b));
        }
    }
    let kept: HashSet<usize> = keeper.values().copied().collect();
    let mut out = Vec::new();
    for (i, (story, path, b)) in now.into_iter().enumerate() {
        if kept.contains(&i) {
            continue;
        }
        let again = gone.get_mut(&content_key(b)).and_then(|same| {
            let holds = |c: &(StoryRef, &Arc<Block>)| **c.1 == **b;
            let at = same.iter().position(|c| c.0 == story && holds(c)).or_else(|| same.iter().position(holds))?;
            Some(same.remove(at).1.id())
        });
        out.push((story, path, again.unwrap_or_else(BlockId::fresh)));
    }
    out
}

/// What [`changed_ids`] files gone blocks under: a paragraph's text, a table's shape. Blocks
/// with the same key are compared in full.
fn content_key(b: &Block) -> u64 {
    use std::hash::Hash;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match b {
        Block::Para(p) => p.text.hash(&mut h),
        Block::Table(t) => (t.rows.len(), t.grid.len()).hash(&mut h),
    }
    h.finish()
}

#[cfg(test)]
mod tests;

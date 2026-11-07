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
//! An id isn't part of what a block holds: `==` ignores it, the document's JSON doesn't hold it,
//! and the markup kept from a file doesn't depend on it. Word files keep it: the docx writer saves
//! it as the paragraph's id (`w14:paraId`, with the whole id beside it when that can't hold it),
//! and the reader gives the paragraph that id back. A document read from a file without them gets
//! new ids.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use crate::{Block, Blocks, DocError, Document, Path, Result, StoryRef};

/// Which block this is (see the [module](self) docs). [`BlockId::UNSET`] until
/// [`Document::ensure_block_ids`] gives the block one. Written (in a [`crate::ChangeSet`], an
/// agent's reply) as 16 hexadecimal digits, so readers whose numbers are doubles (JavaScript)
/// keep every bit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(u64);

impl std::fmt::Display for BlockId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

impl serde::Serialize for BlockId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for BlockId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        if s.is_empty() || s.len() > 16 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(serde::de::Error::custom(format!("`{s}` is not a block id (16 hexadecimal digits)")));
        }
        u64::from_str_radix(&s, 16).map(BlockId).map_err(serde::de::Error::custom)
    }
}

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

    /// The id with these 64 bits: one a file saved ([`BlockId::bits`]). 0 is
    /// [`BlockId::UNSET`].
    pub const fn from_bits(bits: u64) -> BlockId {
        BlockId(bits)
    }

    /// The id's 64 bits, as a file saves them.
    pub const fn bits(self) -> u64 {
        self.0
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

/// Hashes an id cheaply, scrambled with this process's random key: the set of ids is built after
/// every command, and ids read from a file (`wc:blockId`) are whatever the file says, so they
/// can't be trusted to be random (ids that differ only in their high bits would otherwise all
/// fall in one bucket).
#[derive(Default)]
struct IdHasher(u64);

impl Hasher for IdHasher {
    fn finish(&self) -> u64 {
        mix(self.0 ^ seed())
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
pub(crate) const MAX_ID_DEPTH: usize = 64;

/// The ids of the blocks inside `b` (in its table cells, as [`Document::each_block`] walks them),
/// unset ones included: what a copy needs, beside `b` itself, to hold it with the same ids.
pub(crate) fn inside_ids(b: &Block) -> Vec<BlockId> {
    let mut out = Vec::new();
    each_inside(b, &mut |inner| out.push(inner.id()));
    out
}

/// Each block inside `b` (in its table cells), in the order [`inside_ids`] lists them.
pub(crate) fn each_inside<'a>(b: &'a Block, f: &mut dyn FnMut(&'a Arc<Block>)) {
    if let Block::Table(t) = b {
        for cell in t.rows.iter().flat_map(|r| &r.cells) {
            walk(&cell.blocks, StoryRef::Body, &mut Vec::new(), 1, &mut |_, _, inner| f(inner));
        }
    }
}

/// Give the blocks inside `b` the ids [`inside_ids`] listed, in the same order. False (and `b`
/// partly renamed) when `ids` has more or fewer than `b` holds.
pub(crate) fn set_inside_ids(b: &mut Block, ids: &[BlockId]) -> bool {
    let mut next = ids.iter();
    if let Block::Table(t) = b {
        for cell in t.rows.iter_mut().flat_map(|r| &mut r.cells) {
            if !rename(&mut cell.blocks, 1, &mut next) {
                return false;
            }
        }
    }
    next.next().is_none()
}

/// The blocks of `bl` and those inside them, at `depth`, take the next ids of `ids` in walk
/// order; false when `ids` runs out.
fn rename(bl: &mut Blocks, depth: usize, ids: &mut std::slice::Iter<'_, BlockId>) -> bool {
    if depth > MAX_ID_DEPTH {
        return true;
    }
    for b in bl.iter_mut() {
        let Some(id) = ids.next() else { return false };
        let b = Arc::make_mut(b);
        *b.id_mut() = *id;
        if let Block::Table(t) = b {
            for cell in t.rows.iter_mut().flat_map(|r| &mut r.cells) {
                if !rename(&mut cell.blocks, depth + 1, ids) {
                    return false;
                }
            }
        }
    }
    true
}

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

    /// Every block's id, in the order [`Document::each_block`] walks them: with the document's
    /// JSON (which leaves ids out), what another copy needs to hold the same blocks under the
    /// same ids ([`Document::set_block_ids`]).
    pub fn block_ids(&self) -> Vec<BlockId> {
        let mut out = Vec::new();
        self.each_block(&mut |_, _, b| out.push(b.id()));
        out
    }

    /// Give the blocks the ids [`Document::block_ids`] listed for a copy of this document. An
    /// error, leaving the document as it was, when `ids` has more or fewer than it holds.
    pub fn set_block_ids(&mut self, ids: &[BlockId]) -> Result<()> {
        let mut d = self.clone();
        let mut next = ids.iter();
        let mut ok = rename(&mut d.body, 0, &mut next);
        for part in d.parts.values_mut() {
            ok = ok && rename(&mut part.blocks, 0, &mut next);
        }
        if !ok || next.next().is_some() {
            return Err(DocError::Invalid(format!("{} block ids for a document of {} blocks", ids.len(), self.block_ids().len())));
        }
        *self = d;
        Ok(())
    }

    /// The first of `ids` that more than one block of the stories has, if any: after a change
    /// was applied, one of the blocks it put in ([`crate::ChangeSet::ids_put_in`]) whose id the
    /// document already held.
    pub fn repeated_id(&self, ids: &[BlockId]) -> Option<BlockId> {
        if ids.is_empty() {
            return None;
        }
        let wanted: IdSet = ids.iter().map(|id| id.0).collect();
        let mut seen = IdSet::default();
        let mut repeated = None;
        self.each_block(&mut |_, _, b| {
            let id = b.id().0;
            if repeated.is_none() && wanted.contains(&id) && !seen.insert(id) {
                repeated = Some(BlockId(id));
            }
        });
        repeated
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

//! Paragraph ids (`w14:paraId`, [MS-DOCX] 2.6.2) from block ids, so that a document saved and
//! read again, in WordCraft or after Word, names its blocks as it did.
//!
//! A paragraph id is 31 bits (more than 0, less than 0x80000000) and unique in the document, and
//! a [`BlockId`] is 64. So a paragraph's id is its block id's low 31 bits, or a free one when
//! another paragraph has those already (or they are 0). When the paragraph id doesn't hold the
//! whole block id, the paragraph also gets `wc:blockId`, an attribute of WordCraft's own
//! namespace, which the parts list as ignorable (`mc:Ignorable`, ECMA-376 Part 3): other
//! programs leave it alone, and Word drops it but keeps the paragraph id. A table has no
//! paragraph id of its own, so it gets only `wc:blockId`. The reader takes `wc:blockId` first,
//! then the paragraph id (`read::story::blocks::block_id`).

use std::collections::{HashMap, HashSet};

use wordcraft_doc::{BlockId, Paragraph};

/// The largest paragraph id Word takes.
const MAX: u32 = 0x7FFF_FFFF;

/// The paragraph ids of one document as it is written.
#[derive(Default)]
pub(crate) struct ParaIds {
    /// A block's paragraph id, given before it was written (a comment's last paragraph) or when
    /// it was.
    by_block: HashMap<BlockId, u32>,
    /// Paragraphs without a block id that need a paragraph id anyway (a comment's last), by
    /// address.
    by_addr: HashMap<usize, u32>,
    /// Every paragraph id given out.
    used: HashSet<u32>,
    /// Where the search for a free id stopped last ([`ParaIds::reserve`]).
    cursor: u32,
    /// The blocks written: a block written twice gets its ids the first time only, so no two
    /// paragraphs of the file share one.
    written: HashSet<BlockId>,
}

/// What a paragraph's `w:p` says about it: its paragraph id, and its whole block id when the
/// paragraph id doesn't hold it.
pub(crate) struct ParaTag {
    pub para_id: String,
    pub block_id: Option<String>,
}

impl ParaIds {
    /// `want`'s low 31 bits when no paragraph has them yet, else the next free id from where the
    /// last such search stopped: the searches together pass each id at most once, so a file whose
    /// ids all share their low bits is still written in linear time.
    fn reserve(&mut self, want: u64) -> u32 {
        let v = (want & u64::from(MAX)) as u32;
        if v != 0 && self.used.insert(v) {
            return v;
        }
        // Ends: fewer ids are used than there are.
        for _ in 0..=self.used.len() {
            self.cursor = if self.cursor >= MAX { 1 } else { self.cursor + 1 };
            if self.used.insert(self.cursor) {
                break;
            }
        }
        self.cursor
    }

    fn for_block(&mut self, id: BlockId) -> u32 {
        if let Some(v) = self.by_block.get(&id) {
            return *v;
        }
        let v = self.reserve(id.bits());
        self.by_block.insert(id, v);
        v
    }

    /// The paragraph id of a comment's last paragraph (`commentsExtended` and `commentsIds` name
    /// the comment by it): its block's, or with no block id, the first free one from `fallback`.
    pub(crate) fn for_comment(&mut self, p: &Paragraph, fallback: u32) -> String {
        let v = if p.id.is_set() {
            self.for_block(p.id)
        } else {
            let addr = std::ptr::from_ref(p).addr();
            match self.by_addr.get(&addr) {
                Some(v) => *v,
                None => {
                    let v = self.reserve(u64::from(fallback));
                    self.by_addr.insert(addr, v);
                    v
                }
            }
        };
        hex(v)
    }

    /// The ids `p` is written with: none for a paragraph without a block id (other than a
    /// comment's last), or whose block was written already.
    pub(crate) fn take(&mut self, p: &Paragraph) -> Option<ParaTag> {
        if !p.id.is_set() {
            let addr = std::ptr::from_ref(p).addr();
            return self.by_addr.get(&addr).map(|v| ParaTag { para_id: hex(*v), block_id: None });
        }
        if !self.written.insert(p.id) {
            return None;
        }
        let v = self.for_block(p.id);
        let block_id = (u64::from(v) != p.id.bits()).then(|| p.id.to_string());
        Some(ParaTag { para_id: hex(v), block_id })
    }

    /// The `wc:blockId` a table is written with: none without a block id, or when its block was
    /// written already.
    pub(crate) fn take_table(&mut self, id: BlockId) -> Option<String> {
        (id.is_set() && self.written.insert(id)).then(|| id.to_string())
    }
}

fn hex(v: u32) -> String {
    format!("{v:08X}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(bits: u64) -> Paragraph {
        let mut p = Paragraph::new();
        p.id = BlockId::from_bits(bits);
        p
    }

    #[test]
    fn a_short_id_is_the_paragraph_id_and_a_long_one_rides_beside_it() {
        let mut ids = ParaIds::default();
        let t = ids.take(&para(0x1234_5678)).unwrap();
        assert_eq!((t.para_id.as_str(), t.block_id), ("12345678", None));
        let t = ids.take(&para(0xABCD_EF01_9234_5678)).unwrap();
        // Its low 31 bits are taken: a free one, and the whole id beside it.
        assert_eq!((t.para_id.as_str(), t.block_id.as_deref()), ("00000001", Some("abcdef0192345678")));
    }

    #[test]
    fn paragraph_ids_are_unique_nonzero_and_below_the_limit() {
        let mut ids = ParaIds::default();
        let tags: Vec<ParaTag> =
            [0u64, 0x8000_0000, 0x7FFF_FFFF, 0xFFFF_FFFF_FFFF_FFFF, 0x1_0000_0000].iter().map(|b| para(*b)).filter_map(|p| ids.take(&p)).collect();
        // 0 is no block id: nothing written.
        let got: Vec<&str> = tags.iter().map(|t| t.para_id.as_str()).collect();
        assert_eq!(got, ["00000001", "7FFFFFFF", "00000002", "00000003"]);
        let whole: Vec<bool> = tags.iter().map(|t| t.block_id.is_some()).collect();
        assert_eq!(whole, [true, false, true, true], "only 7FFFFFFF is its own paragraph id");
    }

    #[test]
    fn ids_sharing_their_low_bits_are_written_in_linear_time() {
        let mut ids = ParaIds::default();
        let n = 20_000u64;
        let mut seen = HashSet::new();
        for k in 1..=n {
            let t = ids.take(&para(k << 32 | 5)).unwrap();
            assert!(seen.insert(t.para_id));
        }
        // The first got 5; the others went from 1 up, skipping it.
        assert!(seen.contains("00000005") && seen.contains(&format!("{:08X}", n)));
        assert!(ids.cursor as u64 <= n + 1, "{}", ids.cursor);
    }

    #[test]
    fn a_block_written_twice_gets_its_ids_once() {
        let mut ids = ParaIds::default();
        let p = para(0x42);
        assert!(ids.take(&p).is_some());
        assert!(ids.take(&p).is_none());
        assert_eq!(ids.take_table(BlockId::from_bits(0x43)).as_deref(), Some("0000000000000043"));
        assert!(ids.take_table(BlockId::from_bits(0x43)).is_none());
        assert!(ids.take_table(BlockId::UNSET).is_none());
    }

    #[test]
    fn a_comment_paragraph_keeps_the_id_it_was_given() {
        let mut ids = ParaIds::default();
        let named = para(0x0100_0000_0000_0007);
        let unnamed = Paragraph::new();
        let a = ids.for_comment(&named, 0x1000_0001);
        let b = ids.for_comment(&unnamed, 0x1000_0002);
        assert_eq!((a.as_str(), b.as_str()), ("00000007", "10000002"));
        assert_eq!(ids.take(&named).unwrap().para_id, a);
        assert_eq!(ids.take(&unnamed).unwrap().para_id, b);
        assert_eq!(ids.take(&unnamed).unwrap().para_id, b, "a paragraph without a block id isn't tracked as written");
        assert!(ids.take(&Paragraph::new()).is_none());
    }
}

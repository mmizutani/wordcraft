use super::*;
use crate::table::MAX_TABLE_DEPTH;
use crate::{CharProps, PartKind, Pos, Table, para_block};

/// Every block's id, in document order.
fn ids(d: &Document) -> Vec<BlockId> {
    let mut out = Vec::new();
    d.each_block(&mut |_, _, b| out.push(b.id()));
    out
}

/// Whether every block has an id and no two share one.
fn all_own(d: &Document) -> bool {
    let ids = ids(d);
    let distinct: HashSet<&BlockId> = ids.iter().collect();
    ids.iter().all(|i| i.is_set()) && distinct.len() == ids.len()
}

fn para(s: &str) -> Arc<Block> {
    para_block(crate::Paragraph::with_text(s, CharProps::default()))
}

/// Blocks in every kind of place: body paragraphs, a table with a table in a cell, a header.
fn sample() -> Document {
    let mut d = Document::from_text("one\ntwo\nthree");
    let mut inner = Table::new(1, 2, 100.0);
    inner.rows[0].cells[1].blocks.push(para("deep"));
    let mut t = Table::new(2, 2, 200.0);
    t.rows[1].cells[0].blocks.push(Arc::new(Block::Table(inner)));
    d.body.insert(1, Arc::new(Block::Table(t)));
    d.add_part(PartKind::Header, vec![para("header")]);
    d
}

#[test]
fn fresh_ids_are_set_and_never_repeat() {
    let ids: Vec<BlockId> = (0..10_000).map(|_| BlockId::fresh()).collect();
    assert!(ids.iter().all(|i| i.is_set()));
    assert_eq!(ids.iter().collect::<HashSet<_>>().len(), ids.len());
    assert!(!BlockId::UNSET.is_set());
    assert_eq!(BlockId::default(), BlockId::UNSET);
    // Zero is the one number that mixes to zero, which `fresh` steps over.
    assert_eq!(mix(0), 0);
    assert!((1..10_000u64).all(|n| mix(n) != 0 && mix(n.wrapping_neg()) != 0));
}

#[test]
fn each_block_walks_every_story_in_document_order() {
    let d = sample();
    let mut seen = Vec::new();
    d.each_block(&mut |story, path, _| seen.push((story, path.to_vec())));
    // one, the table, its four cells' paragraphs, the inner table with its two and "deep", two,
    // three; then the header.
    assert_eq!(seen.len(), 13);
    assert_eq!(seen[..3], [(StoryRef::Body, vec![0]), (StoryRef::Body, vec![1]), (StoryRef::Body, vec![1, 0, 0, 0])]);
    assert!(seen.iter().filter(|(s, _)| *s == StoryRef::Body).map(|(_, p)| p).is_sorted());
    assert!(matches!(seen.last(), Some((StoryRef::Part(_), p)) if *p == [0]));
    // Each path leads to its block.
    d.each_block(&mut |story, path, b| {
        assert!(d.block(story, &Path(path.to_vec())).is_some_and(|x| std::ptr::eq(x, &**b)), "{path:?}");
    });
}

#[test]
fn every_block_in_every_story_gets_an_id_of_its_own() {
    let mut d = sample();
    assert!(ids(&d).iter().all(|i| !i.is_set()));
    assert_eq!(d.ensure_block_ids(None), 13);
    assert!(all_own(&d));
    // Nothing left to do.
    let named = ids(&d);
    assert_eq!(d.ensure_block_ids(None), 0);
    assert_eq!(ids(&d), named);
}

#[test]
fn ids_are_not_content() {
    let plain = sample();
    let mut named = plain.clone();
    named.ensure_block_ids(None);
    assert_eq!(named, plain);
    assert!(named.same_content(&plain));
    // Not saved, so the markup kept from a file (fingerprinted by its JSON) doesn't see them.
    let json = serde_json::to_string(&named).unwrap();
    assert_eq!(json, serde_json::to_string(&plain).unwrap());
    let back: Document = serde_json::from_str(&json).unwrap();
    assert!(ids(&back).iter().all(|i| !i.is_set()));
}

#[test]
fn an_edited_block_keeps_its_id() {
    let mut d = sample();
    d.ensure_block_ids(None);
    let named = ids(&d);
    let before = d.clone();
    d.insert_text(&Pos::body(0, 3), " more", &CharProps::default()).unwrap();
    // Typing in a cell copies the table; it and the blocks in it keep their ids.
    let cell = Pos::new(StoryRef::Body, Path(vec![1, 0, 0, 0]), 0);
    d.insert_text(&cell, "x", &CharProps::default()).unwrap();
    assert_eq!(d.ensure_block_ids(Some(&before)), 0);
    assert_eq!(ids(&d), named);
}

#[test]
fn a_copy_gets_a_new_id_and_the_original_keeps_its_own() {
    let mut d = sample();
    d.ensure_block_ids(None);
    let before = d.clone();
    // "two", copied id and all, pasted at the start: the copy comes first in the document.
    let original = d.body[2].clone();
    d.body.insert(0, Arc::new((*original).clone()));
    // And a copy of a block inside the table, into the header.
    let nested = d.block(StoryRef::Body, &Path(vec![2, 1, 0, 1])).cloned().unwrap();
    assert!(matches!(nested, Block::Table(_)));
    let header = *d.parts.keys().next().unwrap();
    d.parts.get_mut(&header).unwrap().blocks.insert(0, Arc::new(nested.clone()));
    assert_eq!(d.ensure_block_ids(Some(&before)), 5, "the two copies and the three blocks in the copied table");
    assert!(all_own(&d));
    // The original is the very block it was, with its id.
    assert!(Arc::ptr_eq(&d.body[3], &original));
    assert_ne!(d.body[0].id(), original.id());
    assert_eq!(d.block(StoryRef::Body, &Path(vec![2, 1, 0, 1])).map(Block::id), Some(nested.id()));
}

#[test]
fn with_nothing_to_go_by_the_first_of_blocks_sharing_an_id_keeps_it() {
    let mut d = Document::from_text("a\nb");
    d.ensure_block_ids(None);
    let a = d.body[0].id();
    // The same block twice.
    let again = d.body[0].clone();
    d.body.insert(1, again);
    assert_eq!(d.ensure_block_ids(None), 1);
    assert_eq!(d.body[0].id(), a);
    assert!(all_own(&d));
    // The same allocation `before` holds, put in a second place: the one in its old place
    // keeps the id.
    let before = d.clone();
    let b = d.body[2].clone();
    d.body.insert(0, b.clone());
    assert_eq!(d.ensure_block_ids(Some(&before)), 1);
    assert!(Arc::ptr_eq(&d.body[3], &b));
    assert_ne!(d.body[0].id(), b.id());
    assert!(all_own(&d));
}

#[test]
fn giving_ids_copies_only_the_blocks_that_get_them() {
    let mut d = sample();
    d.ensure_block_ids(None);
    let before = d.clone();
    d.body.insert(0, para("new"));
    let table = d.body.get_mut(2).unwrap();
    let Block::Table(t) = Arc::make_mut(table) else { panic!("table") };
    t.rows[0].cells[1].blocks.push(para("new in a cell"));
    assert_eq!(d.ensure_block_ids(Some(&before)), 2);
    assert!(all_own(&d));
    // The other blocks are still shared with the document before, also those in the table.
    assert_eq!(d.body[1..].iter().zip(&before.body).filter(|(a, b)| Arc::ptr_eq(a, b)).count(), 3, "all but the table");
    let (Block::Table(now), Block::Table(was)) = (&*d.body[2], &*before.body[1]) else { panic!("tables") };
    assert!(Arc::ptr_eq(&now.rows[1].cells[0].blocks[1], &was.rows[1].cells[0].blocks[1]));
    let header = |doc: &Document| doc.parts.values().next().unwrap().blocks[0].clone();
    assert!(Arc::ptr_eq(&header(&d), &header(&before)));
}

#[test]
fn a_split_paragraph_keeps_its_id_with_its_text() {
    // At the start (Enter before the text adds an empty paragraph above it), in the middle, at
    // the end; and in an empty paragraph.
    for (text, off, keeper) in [("hello", 0, 1), ("hello", 2, 0), ("hello", 5, 0), ("", 0, 0)] {
        let mut d = Document::from_text(text);
        d.ensure_block_ids(None);
        let id = d.body[0].id();
        let before = d.clone();
        d.split_paragraph(&Pos::body(0, off)).unwrap();
        assert_eq!(d.ensure_block_ids(Some(&before)), 1, "{text:?} at {off}");
        assert_eq!(d.body[keeper].id(), id, "{text:?} at {off}");
        assert!(all_own(&d));
    }
}

#[test]
fn a_paragraph_joined_to_an_empty_one_before_it_keeps_its_id() {
    // Backspace at the start of "world", after an empty paragraph.
    let mut d = Document::from_text("\nworld");
    d.ensure_block_ids(None);
    let world = d.body[1].id();
    d.delete_range(&Pos::body(0, 0), &Pos::body(1, 0)).unwrap();
    assert_eq!((d.body.len(), d.body[0].id()), (1, world));
    // Delete at the end of a paragraph with text: that one keeps its id.
    let mut d = Document::from_text("hello\nworld");
    d.ensure_block_ids(None);
    let hello = d.body[0].id();
    d.delete_range(&Pos::body(0, 5), &Pos::body(1, 0)).unwrap();
    assert_eq!((d.body.len(), d.body[0].id()), (1, hello));
}

/// Tables nested `n` deep around an empty paragraph, in a document.
fn nested(n: usize) -> Document {
    let mut t = Table::new(1, 1, 100.0);
    for _ in 1..n {
        let mut outer = Table::new(1, 1, 100.0);
        outer.rows[0].cells[0].blocks.insert(0, Arc::new(Block::Table(t)));
        t = outer;
    }
    let mut d = Document::from_text("x");
    d.body.insert(0, Arc::new(Block::Table(t)));
    d
}

#[test]
fn blocks_in_tables_nested_as_deep_as_edits_go_get_ids() {
    let mut d = nested(MAX_TABLE_DEPTH);
    assert_eq!(d.ensure_block_ids(None), 1 + 2 * MAX_TABLE_DEPTH);
    assert!(all_own(&d));
}

#[test]
fn a_hostile_nesting_stops_the_walk() {
    let mut d = nested(MAX_ID_DEPTH + 10);
    // The body and the cells down to the bound are walked: "x", the outermost tables and the
    // paragraphs beside them, and the table in the deepest cell walked.
    assert_eq!(d.ensure_block_ids(None), 2 * (MAX_ID_DEPTH + 1));
}

#[test]
fn a_paragraph_deleted_and_made_again_gets_its_id_back() {
    // A selection across the mark between two paragraphs replaced by a table: the paragraphs
    // join, then split again at the same place around the table.
    let mut d = Document::from_text("first\nsecond");
    d.ensure_block_ids(None);
    let (first, second) = (d.body[0].id(), d.body[1].id());
    let before = d.clone();
    d.delete_range(&Pos::body(0, 5), &Pos::body(1, 0)).unwrap();
    d.split_paragraph(&Pos::body(0, 5)).unwrap();
    d.insert_block(StoryRef::Body, &Path::top(1), Block::Table(Table::new(1, 1, 100.0))).unwrap();
    assert_eq!(d.ensure_block_ids(Some(&before)), 3, "the second paragraph, the table and its cell");
    assert_eq!((d.body[0].id(), d.body[2].id()), (first, second));
    assert!(all_own(&d));
    // An empty paragraph made again: Enter over a selection from an empty paragraph into the
    // next one keeps both.
    let mut d = Document::from_text("\nnext");
    d.ensure_block_ids(None);
    let (empty, next) = (d.body[0].id(), d.body[1].id());
    let before = d.clone();
    d.delete_range(&Pos::body(0, 0), &Pos::body(1, 0)).unwrap();
    d.split_paragraph(&Pos::body(0, 0)).unwrap();
    d.ensure_block_ids(Some(&before));
    assert_eq!((d.body[0].id(), d.body[1].id()), (empty, next));
}

#[test]
fn an_id_still_in_use_is_never_given_again() {
    let mut d = Document::from_text("same\nother");
    d.ensure_block_ids(None);
    let before = d.clone();
    // A new paragraph holding what "same" holds while "same" is still there: a new id.
    d.body.insert(1, para("same"));
    // Two paragraphs holding what one deleted paragraph held: one of them gets its id.
    let other = d.body.remove(2).id();
    d.body.insert(0, para("other"));
    d.body.insert(0, para("other"));
    assert_eq!(d.ensure_block_ids(Some(&before)), 3);
    assert!(all_own(&d));
    assert_eq!((d.body[0].id(), d.body[2].id()), (other, before.body[0].id()));
    assert!(![other, d.body[2].id()].contains(&d.body[3].id()));
}

#[test]
fn text_moved_to_another_story_keeps_its_id() {
    let mut d = Document::from_text("boxed\nstays");
    d.ensure_block_ids(None);
    let boxed = d.body[0].id();
    let before = d.clone();
    // Moved into a new text box as a new paragraph holding the same.
    d.body.remove(0);
    let part = d.add_part(PartKind::TextBox, vec![para("boxed")]);
    d.ensure_block_ids(Some(&before));
    assert_eq!(d.parts[&part].blocks[0].id(), boxed);
    assert!(all_own(&d));
}

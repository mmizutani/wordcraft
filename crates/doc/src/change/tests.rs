use std::sync::Arc;

use super::*;
use crate::{CharProps, DocError, Paragraph, Path, Pos, Rgb, para_block};

/// A document of three paragraphs, a 2×2 table and a last paragraph, with ids.
fn sample() -> Document {
    let mut d = Document::from_text("one\ntwo\nthree");
    d.body.push(Arc::new(Block::Table(Table::new(2, 2, 400.0))));
    d.body.push(para_block(Paragraph::new()));
    d.ensure_block_ids(None);
    d
}

/// `d` edited by `f` (with ids given as after a command), and the change between them.
fn edit(d: &Document, f: impl FnOnce(&mut Document)) -> (Document, ChangeSet) {
    let mut after = d.clone();
    f(&mut after);
    after.ensure_block_ids(Some(d));
    let cs = ChangeSet::between(d, &after);
    (after, cs)
}

/// A copy of `d` sharing nothing with it: its JSON, and its ids.
fn copy(d: &Document) -> Document {
    let json = serde_json::to_string(d).unwrap();
    let mut c: Document = serde_json::from_str(&json).unwrap();
    c.set_block_ids(&d.block_ids()).unwrap();
    c
}

/// The change, sent as JSON.
fn sent(cs: &ChangeSet) -> ChangeSet {
    serde_json::from_str(&serde_json::to_string(cs).unwrap()).unwrap()
}

fn assert_same(a: &Document, b: &Document) {
    assert!(a == b, "the copies hold different content");
    assert_eq!(a.block_ids(), b.block_ids());
}

fn type_at(block: usize, off: usize, text: &str) -> impl FnOnce(&mut Document) {
    move |d| {
        d.insert_text(&Pos::body(block, off), text, &CharProps::default()).unwrap();
    }
}

#[test]
fn typing_makes_one_splice_of_one_paragraph_keeping_its_id() {
    let d = sample();
    let (after, cs) = edit(&d, type_at(1, 3, "!"));
    assert_eq!(cs.splices.len(), 1, "{cs:?}");
    let s = &cs.splices[0];
    assert_eq!(s.container, Container::Story(StoryRef::Body));
    assert_eq!((s.after, s.before), (Some(d.body[0].id()), Some(d.body[2].id())));
    assert_eq!(s.removed.iter().map(|b| b.id).collect::<Vec<_>>(), vec![d.body[1].id()]);
    assert_eq!(s.inserted.len(), 1);
    assert_eq!(s.inserted[0].0.id(), d.body[1].id());
    assert_eq!(s.text, TextSplice { at: 3, removed: 0, inserted: "!".into() });
    assert!(cs.tables.is_empty() && cs.parts.is_empty() && cs.fields.is_empty());
    let mut other = copy(&d);
    other.apply(&sent(&cs)).unwrap();
    assert_same(&other, &after);
}

#[test]
fn nothing_changed_is_an_empty_change() {
    let d = sample();
    assert!(ChangeSet::between(&d, &d.clone()).is_empty());
}

#[test]
fn enter_splits_one_paragraph_into_two_and_positions_follow_their_text() {
    let d = sample();
    let (after, cs) = edit(&d, |d| {
        d.split_paragraph(&Pos::body(2, 2)).unwrap();
    });
    let s = &cs.splices[0];
    assert_eq!(s.inserted.len(), 2);
    assert_eq!(s.inserted[0].0.id(), d.body[2].id(), "the first half keeps the id");
    assert_eq!(s.text, TextSplice { at: 2, removed: 0, inserted: MARK.to_string() });
    let three = d.body[2].id();
    let new = after.body[3].id();
    assert_eq!(cs.map(Anchor { block: three, off: 1 }), Mapped::At(Anchor { block: three, off: 1 }));
    assert_eq!(cs.map(Anchor { block: three, off: 2 }), Mapped::At(Anchor { block: three, off: 2 }));
    assert_eq!(cs.map(Anchor { block: three, off: 4 }), Mapped::At(Anchor { block: new, off: 2 }));
    // An anchor in a paragraph the change didn't touch stays.
    let one = d.body[0].id();
    assert_eq!(cs.map(Anchor { block: one, off: 2 }), Mapped::At(Anchor { block: one, off: 2 }));
    let mut other = copy(&d);
    other.apply(&sent(&cs)).unwrap();
    assert_same(&other, &after);
}

#[test]
fn deleting_across_paragraphs_joins_them_and_maps_into_the_join() {
    let d = sample();
    // "on|e", "two", "th|ree" → "onree".
    let (after, cs) = edit(&d, |d| {
        d.delete_range(&Pos::body(0, 2), &Pos::body(2, 2)).unwrap();
    });
    assert_eq!(cs.splices.len(), 1);
    let s = &cs.splices[0];
    assert_eq!(s.removed.len(), 3);
    assert_eq!(s.inserted.len(), 1);
    let one = d.body[0].id();
    assert_eq!(after.body[0].as_para().map(|p| p.text.as_str()), Some("onree"));
    // In the deleted text: where the deletion was. After it: shifted into the joined paragraph.
    assert_eq!(cs.map(Anchor { block: d.body[1].id(), off: 1 }), Mapped::At(Anchor { block: one, off: 2 }));
    assert_eq!(cs.map(Anchor { block: d.body[2].id(), off: 4 }), Mapped::At(Anchor { block: one, off: 4 }));
    let mut other = copy(&d);
    other.apply(&sent(&cs)).unwrap();
    assert_same(&other, &after);
}

#[test]
fn typing_in_a_table_cell_splices_only_that_cell() {
    let d = sample();
    let table = d.body[3].id();
    let (after, cs) = edit(&d, |d| {
        d.insert_text(&Pos::new(StoryRef::Body, Path(vec![3, 1, 0, 0]), 0), "cell", &CharProps::default()).unwrap();
    });
    assert_eq!(cs.splices.len(), 1, "{cs:?}");
    assert_eq!(cs.splices[0].container, Container::Cell { table, row: 1, col: 0 });
    assert!(cs.tables.is_empty());
    let mut other = copy(&d);
    other.apply(&sent(&cs)).unwrap();
    assert_same(&other, &after);
}

#[test]
fn a_table_edited_in_place_sends_its_properties() {
    let d = sample();
    let (after, cs) = edit(&d, |d| {
        let t = d.table_mut(StoryRef::Body, &Path(vec![3])).unwrap();
        t.rows[0].cells[1].props.shading = Some(Rgb(0x11, 0x22, 0x33));
    });
    assert!(cs.splices.is_empty());
    assert_eq!(cs.tables.len(), 1);
    let mut other = copy(&d);
    other.apply(&sent(&cs)).unwrap();
    assert_same(&other, &after);
}

#[test]
fn a_new_table_row_sends_the_table_whole_under_its_id() {
    let d = sample();
    let (after, cs) = edit(&d, |d| {
        let t = d.table_mut(StoryRef::Body, &Path(vec![3])).unwrap();
        let row = t.rows[0].clone();
        t.rows.push(row);
    });
    assert_eq!(cs.splices.len(), 1);
    assert_eq!(cs.splices[0].inserted[0].0.id(), d.body[3].id());
    let mut other = copy(&d);
    other.apply(&sent(&cs)).unwrap();
    assert_same(&other, &after);
}

#[test]
fn parts_and_fields_travel_too() {
    let d = sample();
    let (after, cs) = edit(&d, |d| {
        d.parts.insert(
            7,
            Part { kind: PartKind::Header, blocks: vec![para_block(Paragraph::with_text("Header", CharProps::default()))], body: Default::default() },
        );
        d.comments.insert(3, Comment { author: "Ann".into(), ..Default::default() });
        d.media.insert("image1.png".into(), Arc::new(vec![1, 2, 3]));
        Arc::make_mut(&mut d.styles).default_chr.bold = Some(true);
        d.core.title = "Plan".into();
    });
    assert_eq!(cs.parts.len(), 1);
    assert_eq!(cs.fields.len(), 4, "{:?}", cs.fields);
    let mut other = copy(&d);
    other.apply(&sent(&cs)).unwrap();
    assert_same(&other, &after);
    // And back: the part, the comment and the picture go away again.
    let back = ChangeSet::between(&after, &d);
    let mut again = copy(&after);
    again.apply(&sent(&back)).unwrap();
    assert_same(&again, &d);
}

#[test]
fn a_change_applies_where_other_blocks_moved_meanwhile() {
    let d = sample();
    let (_, cs) = edit(&d, type_at(2, 0, "and "));
    // Meanwhile, in another copy, someone added a paragraph at the start.
    let (mut other, _) = edit(&copy(&d), |d| d.body.insert(0, para_block(Paragraph::with_text("first", CharProps::default()))));
    other.apply(&sent(&cs)).unwrap();
    assert_eq!(other.body[3].as_para().map(|p| p.text.as_str()), Some("and three"));
    assert_eq!(other.body[0].as_para().map(|p| p.text.as_str()), Some("first"));
}

#[test]
fn a_change_made_on_other_content_is_refused_and_changes_nothing() {
    let d = sample();
    let (after, cs) = edit(&d, type_at(1, 0, "x"));
    // Twice: the paragraph no longer holds what the change was made on.
    let mut other = copy(&d);
    other.apply(&cs).unwrap();
    assert!(matches!(other.apply(&cs), Err(DocError::Stale(_))));
    assert_same(&other, &after);
    // Someone else typed in the same paragraph meanwhile.
    let (mut other, _) = edit(&copy(&d), type_at(1, 3, "?"));
    let was = other.clone();
    assert!(matches!(other.apply(&cs), Err(DocError::Stale(_))));
    assert_same(&other, &was);
    // The paragraph was deleted meanwhile.
    let (mut other, _) = edit(&copy(&d), |d| {
        d.body.remove(1);
    });
    assert!(matches!(other.apply(&cs), Err(DocError::Stale(_))));
}

#[test]
fn a_part_taken_away_must_hold_what_it_held() {
    let (d, _) = edit(&sample(), |d| {
        let header = para_block(Paragraph::with_text("Header", CharProps::default()));
        d.parts.insert(7, Part { kind: PartKind::Header, blocks: vec![header], body: Default::default() });
    });
    let (gone, cs) = edit(&d, |d| {
        d.parts.remove(&7);
    });
    // Meanwhile, someone typed in the header: its paragraph keeps its id, and the removal made
    // without their words is refused rather than taking them away with the part.
    let (mut other, _) = edit(&copy(&d), |d| {
        d.insert_text(&Pos::new(StoryRef::Part(7), Path::top(0), 0), "My ", &CharProps::default()).unwrap();
    });
    let was = other.clone();
    assert!(matches!(other.apply(&sent(&cs)), Err(DocError::Stale(_))));
    assert_same(&other, &was);
    // Or pressed Enter in it.
    let (mut other, _) = edit(&copy(&d), |d| {
        d.split_paragraph(&Pos::new(StoryRef::Part(7), Path::top(0), 3)).unwrap();
    });
    assert!(matches!(other.apply(&sent(&cs)), Err(DocError::Stale(_))));
    // A copy holding what the part held takes it away.
    let mut other = copy(&d);
    other.apply(&sent(&cs)).unwrap();
    assert_same(&other, &gone);
}

#[test]
fn a_change_that_would_empty_a_story_is_refused() {
    let d = sample();
    let mut cs = ChangeSet::between(&d, &d);
    cs.splices.push(Splice {
        container: Container::Story(StoryRef::Body),
        after: None,
        before: None,
        removed: d.body.iter().map(|b| OldBlock::of(b)).collect(),
        inserted: Vec::new(),
        text: TextSplice::default(),
    });
    let mut other = copy(&d);
    assert!(matches!(other.apply(&cs), Err(DocError::Invalid(_))));
    assert_same(&other, &d);
}

#[test]
fn junk_changes_are_errors_not_panics() {
    for json in [
        r#"{"splices": [{"container": {"cell": {"table": "00000000000000ff", "row": 9, "col": 9}}, "removed": [], "inserted": [], "text": {"at": 0, "removed": 0, "inserted": ""}}]}"#,
        r#"{"splices": [{"container": {"story": {"part": 99}}, "after": "0000000000000001", "removed": [], "inserted": [], "text": {"at": 0, "removed": 0, "inserted": ""}}]}"#,
        r#"{"tables": [{"id": "0000000000000001", "props": {}, "grid": [], "rows": []}]}"#,
        r#"{"parts": [{"removed": {"id": 4, "blocks": []}}]}"#,
        r#"{"fields": [{"revisions": {"base": 7, "keep": 9, "append": []}}]}"#,
    ] {
        let cs: ChangeSet = serde_json::from_str(json).unwrap_or_else(|e| panic!("{json}: {e}"));
        let mut d = sample();
        assert!(d.apply(&cs).is_err(), "{json}");
        assert_same(&d, &sample_ids(&d));
    }
    for json in [
        r#"{"splices": [{"container": 5}]}"#,
        r#"{"parts": [{"added": {"id": 1, "kind": "header", "body": {}, "blocks": [{"id": "zz", "block": {}}]}}]}"#,
    ] {
        assert!(serde_json::from_str::<ChangeSet>(json).is_err(), "{json}");
    }
}

/// `d` itself (for comparing a document with its unchanged self).
fn sample_ids(d: &Document) -> Document {
    d.clone()
}

#[test]
fn block_ids_travel_beside_the_json() {
    let d = sample();
    let c = copy(&d);
    assert_same(&c, &d);
    let mut wrong = c.clone();
    assert!(wrong.set_block_ids(&d.block_ids()[1..]).is_err());
    assert_same(&wrong, &c);
}

#[test]
fn ids_and_fingerprints_are_written_as_hex() {
    let d = sample();
    let (_, cs) = edit(&d, type_at(0, 0, "x"));
    let v = serde_json::to_value(&cs).unwrap();
    let id = v["splices"][0]["removed"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(id, d.body[0].id().to_string());
    assert_eq!(id.len(), 16);
    assert_eq!(v["splices"][0]["removed"][0]["hash"].as_str().map(str::len), Some(16));
}

#[test]
fn text_splices_cut_at_character_boundaries() {
    assert_eq!(text_splice("abc", "abc"), TextSplice { at: 3, removed: 0, inserted: String::new() });
    assert_eq!(text_splice("aéc", "aèc"), TextSplice { at: 1, removed: 2, inserted: "è".into() });
    assert_eq!(text_splice("aaa", "aa"), TextSplice { at: 2, removed: 1, inserted: String::new() });
    assert_eq!(text_splice("", "日本"), TextSplice { at: 0, removed: 0, inserted: "日本".into() });
}

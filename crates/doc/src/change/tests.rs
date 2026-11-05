use std::sync::Arc;

use super::*;
use crate::para::OBJ;
use crate::{Align, CharProps, DocError, InlineObject, Paragraph, Path, Pos, Rgb, para_block};

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
fn typing_sends_only_the_characters_typed() {
    let d = sample();
    let (after, cs) = edit(&d, type_at(1, 3, "!"));
    assert!(cs.splices.is_empty() && cs.tables.is_empty() && cs.parts.is_empty() && cs.fields.is_empty(), "{cs:?}");
    assert_eq!(cs.edits.len(), 1, "{cs:?}");
    let e = &cs.edits[0];
    assert_eq!((e.container, e.id), (Container::Story(StoryRef::Body), d.body[1].id()));
    assert_eq!(e.was, Some(fingerprint(&d.body[1])));
    let c = e.chars.as_ref().unwrap();
    assert_eq!((c.at, c.removed, c.inserted.text.as_str(), c.inserted.runs.len()), (3, 0, "!", 1));
    assert!(e.shell.is_none());
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
fn typing_in_a_table_cell_edits_only_that_cell() {
    let d = sample();
    let table = d.body[3].id();
    let (after, cs) = edit(&d, |d| {
        d.insert_text(&Pos::new(StoryRef::Body, Path(vec![3, 1, 0, 0]), 0), "cell", &CharProps::default()).unwrap();
    });
    assert!(cs.splices.is_empty() && cs.tables.is_empty(), "{cs:?}");
    assert_eq!(cs.edits.len(), 1, "{cs:?}");
    assert_eq!(cs.edits[0].container, Container::Cell { table, row: 1, col: 0 });
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
    let (_, cs) = edit(&d, |d| {
        d.body.remove(0);
    });
    let v = serde_json::to_value(&cs).unwrap();
    let id = v["splices"][0]["removed"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(id, d.body[0].id().to_string());
    assert_eq!(id.len(), 16);
    assert_eq!(v["splices"][0]["removed"][0]["hash"].as_str().map(str::len), Some(16));
    let (_, cs) = edit(&d, type_at(0, 0, "x"));
    let v = serde_json::to_value(&cs).unwrap();
    assert_eq!(v["edits"][0]["id"].as_str(), Some(d.body[0].id().to_string().as_str()));
    assert_eq!(v["edits"][0]["was"].as_str().map(str::len), Some(16));
    assert_eq!(v["edits"][0]["chars"]["hash"].as_str().map(str::len), Some(16));
}

#[test]
fn text_splices_cut_at_character_boundaries() {
    assert_eq!(text_splice("abc", "abc"), TextSplice { at: 3, removed: 0, inserted: String::new() });
    assert_eq!(text_splice("aéc", "aèc"), TextSplice { at: 1, removed: 2, inserted: "è".into() });
    assert_eq!(text_splice("aaa", "aa"), TextSplice { at: 2, removed: 1, inserted: String::new() });
    assert_eq!(text_splice("", "日本"), TextSplice { at: 0, removed: 0, inserted: "日本".into() });
}

/// The text of block `i` of the body (none for a table).
fn text(d: &Document, i: usize) -> &str {
    d.body.get(i).and_then(|b| b.as_para()).map_or("", |p| p.text.as_str())
}

/// Changes `a` and `b`, both made on `d`, in both orders, each sent as JSON: `b` then `a` moved
/// past it (a hub that took `b` first puts `a` after it), and `a` then `b` moved past it. Both
/// give the same document under the same ids, which is returned.
fn both_orders(d: &Document, a: &ChangeSet, b: &ChangeSet) -> Document {
    let a_after_b = sent(&a.rebase(b, Side::After).unwrap());
    let b_before_a = sent(&b.rebase(a, Side::Before).unwrap());
    let mut x = copy(d);
    x.apply(&sent(b)).unwrap();
    x.apply(&a_after_b).unwrap();
    let mut y = copy(d);
    y.apply(&sent(a)).unwrap();
    y.apply(&b_before_a).unwrap();
    assert_same(&x, &y);
    x
}

/// Whether changes `a` and `b`, made at once, clash: the same whichever is moved past the other.
fn clashes(a: &ChangeSet, b: &ChangeSet) -> bool {
    let (x, y) = (a.rebase(b, Side::After), b.rebase(a, Side::Before));
    assert_eq!(x.is_err(), y.is_err(), "a clash is a clash whichever change moves: {x:?} / {y:?}");
    for r in [&x, &y] {
        assert!(matches!(r, Ok(_) | Err(DocError::Stale(_))), "{r:?}");
    }
    x.is_err()
}

fn bold(d: &mut Document, block: usize, a: usize, b: usize) {
    d.format_range(&Pos::body(block, a), &Pos::body(block, b), &|c: &mut CharProps| c.bold = Some(true)).unwrap();
}

fn align(block: usize, to: Align) -> impl FnOnce(&mut Document) {
    move |d| {
        if let Some(Block::Para(p)) = d.body.get_mut(block).map(Arc::make_mut) {
            p.props.align = Some(to);
        }
    }
}

#[test]
fn formatting_a_word_sends_that_word_with_its_new_formatting() {
    let d = sample();
    let (after, cs) = edit(&d, |d| bold(d, 1, 1, 3));
    assert_eq!(cs.edits.len(), 1, "{cs:?}");
    let c = cs.edits[0].chars.as_ref().unwrap();
    assert_eq!((c.at, c.removed, c.inserted.text.as_str()), (1, 2, "wo"));
    assert_eq!(c.inserted.runs.iter().map(|r| (r.len, r.props.bold)).collect::<Vec<_>>(), vec![(2, Some(true))]);
    let mut other = copy(&d);
    other.apply(&sent(&cs)).unwrap();
    assert_same(&other, &after);
}

#[test]
fn a_paragraph_s_own_properties_travel_with_a_fingerprint_of_the_old_ones() {
    let d = sample();
    let (after, cs) = edit(&d, align(1, Align::Center));
    assert_eq!(cs.edits.len(), 1, "{cs:?}");
    assert!(cs.edits[0].chars.is_none());
    assert_eq!(cs.edits[0].shell.as_ref().map(|s| s.props.align), Some(Some(Align::Center)));
    let mut other = copy(&d);
    other.apply(&sent(&cs)).unwrap();
    assert_same(&other, &after);
    // Someone typing in it meanwhile is no clash; someone aligning it otherwise is.
    let (_, typed) = edit(&d, type_at(1, 0, "x"));
    assert_eq!(text(&both_orders(&d, &cs, &typed), 1), "xtwo");
    let (_, right) = edit(&d, align(1, Align::Right));
    assert!(clashes(&cs, &right));
}

#[test]
fn objects_travel_with_the_characters_around_them() {
    let d = sample();
    let mark = |name: &str| InlineObject::BookmarkStart { name: name.into() };
    let put = |at: usize, name: &'static str| {
        move |d: &mut Document| {
            if let Some(Block::Para(p)) = d.body.get_mut(0).map(Arc::make_mut) {
                p.insert_object(at, mark(name), &CharProps::default()).unwrap();
            }
        }
    };
    let (after, cs) = edit(&d, put(1, "here"));
    let c = cs.edits[0].chars.as_ref().unwrap();
    assert_eq!((c.at, c.inserted.text.as_str(), c.inserted.objects.len()), (1, "\u{FFFC}", 1));
    let mut other = copy(&d);
    other.apply(&sent(&cs)).unwrap();
    assert_same(&other, &after);
    // Another one before it, and the copy holds them in their order.
    let (again, cs) = edit(&after, put(0, "there"));
    other.apply(&sent(&cs)).unwrap();
    assert_same(&other, &again);
    assert!(
        matches!(&again.body[0].as_para().unwrap().objects[..], [InlineObject::BookmarkStart { name: a }, InlineObject::BookmarkStart { name: b }] if a == "there" && b == "here")
    );
}

#[test]
fn anchors_move_with_the_characters_put_in_before_them() {
    let d = sample();
    let (_, cs) = edit(&d, type_at(2, 1, "xy"));
    let id = d.body[2].id();
    let at = |off| Anchor { block: id, off };
    assert_eq!(cs.map(at(0)), Mapped::At(at(0)));
    assert_eq!(cs.map(at(1)), Mapped::At(at(1)), "an anchor where text was put in stays before it");
    assert_eq!(cs.map(at(4)), Mapped::At(at(6)));
}

#[test]
fn anchors_stay_with_characters_whose_formatting_alone_changed() {
    let d = sample();
    let at = |block: usize, off| Anchor { block: d.body[block].id(), off };
    // Bolding "wo" of "two" sends "wo" again, and moves no character.
    let (_, cs) = edit(&d, |d| bold(d, 1, 1, 3));
    let c = cs.edits[0].chars.as_ref().unwrap();
    assert_eq!((c.at, c.removed, c.kept), (1, 2, KeptChars { start: 2, end: 0 }));
    for off in 0..=3 {
        assert_eq!(cs.map(at(1, off)), Mapped::At(at(1, off)), "{off}");
    }
    // Bolding "three" and typing an X after "th": the places around the X keep their
    // characters, as in a change of the text alone.
    let (after, cs) = edit(&d, |d| {
        bold(d, 2, 0, 5);
        d.insert_text(&Pos::body(2, 2), "X", &CharProps { bold: Some(true), ..CharProps::default() }).unwrap();
    });
    assert_eq!(text(&after, 2), "thXree");
    let c = cs.edits[0].chars.as_ref().unwrap();
    assert_eq!((c.at, c.removed, c.inserted.text.as_str(), c.kept), (0, 5, "thXree", KeptChars { start: 2, end: 3 }));
    for (off, now) in [(0, 0), (1, 1), (2, 2), (3, 4), (5, 6)] {
        assert_eq!(cs.map(at(2, off)), Mapped::At(at(2, now)), "{off}");
    }
    // The kept characters travel with the change.
    assert_eq!(sent(&cs), cs);
    // Kept counts larger than the characters, from elsewhere, are cut to fit them.
    let mut wild = cs.clone();
    if let Some(c) = wild.edits[0].chars.as_mut() {
        c.kept = KeptChars { start: usize::MAX, end: usize::MAX };
    }
    for off in [0, 3, 5, usize::MAX] {
        assert!(matches!(wild.map(at(2, off)), Mapped::At(a) if a.block == d.body[2].id()), "{off}");
    }
}

#[test]
fn two_people_typing_in_one_paragraph_both_get_their_words_in() {
    let d = sample();
    let (_, a) = edit(&d, type_at(2, 0, "and "));
    let (_, b) = edit(&d, type_at(2, 5, "!"));
    assert!(!clashes(&a, &b));
    assert_eq!(text(&both_orders(&d, &a, &b), 2), "and three!");
    // At the same place: the change the hub took first goes first.
    let (_, b) = edit(&d, type_at(2, 0, "but "));
    assert_eq!(text(&both_orders(&d, &a, &b), 2), "but and three");
}

#[test]
fn edits_of_the_same_characters_clash_whichever_comes_first() {
    let d = sample();
    let delete = |a, b| move |d: &mut Document| _ = d.delete_range(&Pos::body(2, a), &Pos::body(2, b)).unwrap();
    // "three" without "hre".
    let (_, del) = edit(&d, delete(1, 4));
    let (_, inside) = edit(&d, type_at(2, 2, "x"));
    assert!(clashes(&del, &inside));
    let (_, overlapping) = edit(&d, delete(3, 5));
    assert!(clashes(&del, &overlapping));
    // Typing where the deleted characters were, before or after them, or deleting the next
    // ones, is no clash.
    let (_, start) = edit(&d, type_at(2, 1, "x"));
    assert_eq!(text(&both_orders(&d, &del, &start), 2), "txe");
    let (_, end) = edit(&d, type_at(2, 4, "y"));
    assert_eq!(text(&both_orders(&d, &del, &end), 2), "tye");
    let (_, next) = edit(&d, delete(4, 5));
    assert_eq!(text(&both_orders(&d, &del, &next), 2), "t");
}

#[test]
fn formatting_and_typing_in_one_paragraph_both_go_through() {
    let d = sample();
    let (_, bolded) = edit(&d, |d| bold(d, 1, 0, 3));
    let (_, typed) = edit(&d, type_at(1, 3, "!"));
    let after = both_orders(&d, &bolded, &typed);
    let p = after.body[1].as_para().unwrap();
    assert_eq!(p.text, "two!");
    assert_eq!(p.runs.iter().map(|r| (r.len, r.props.bold)).collect::<Vec<_>>(), vec![(3, Some(true)), (1, None)]);
}

#[test]
fn an_edit_of_a_paragraph_another_change_takes_away_clashes() {
    let d = sample();
    let (_, typed) = edit(&d, type_at(1, 0, "x"));
    let (_, removed) = edit(&d, |d| _ = d.body.remove(1));
    assert!(clashes(&typed, &removed));
    let (_, joined) = edit(&d, |d| _ = d.delete_range(&Pos::body(0, 3), &Pos::body(1, 0)).unwrap());
    assert!(clashes(&typed, &joined));
    // Enter still replaces the paragraph it splits: typing in it meanwhile is made again.
    let (_, split) = edit(&d, |d| _ = d.split_paragraph(&Pos::body(1, 1)).unwrap());
    assert!(clashes(&typed, &split));
    // Typing in a cell of a table taken away.
    let (_, in_cell) = edit(&d, |d| _ = d.insert_text(&Pos::new(StoryRef::Body, Path(vec![3, 0, 1, 0]), 0), "x", &CharProps::default()).unwrap());
    let (_, no_table) = edit(&d, |d| _ = d.body.remove(3));
    assert!(clashes(&in_cell, &no_table));
    // Typing in the next paragraph is no clash.
    let (_, next) = edit(&d, type_at(2, 0, "y"));
    let after = both_orders(&d, &removed, &next);
    assert_eq!((text(&after, 0), text(&after, 1)), ("one", "ythree"));
}

#[test]
fn paragraphs_put_in_at_the_same_place_both_go_in() {
    let d = sample();
    let put = |t: &'static str| move |d: &mut Document| d.body.insert(1, para_block(Paragraph::with_text(t, CharProps::default())));
    let (_, a) = edit(&d, put("A"));
    let (_, b) = edit(&d, put("B"));
    let after = both_orders(&d, &a, &b);
    assert_eq!((text(&after, 1), text(&after, 2)), ("B", "A"));
    // A change moved past another names the blocks it now sits beside.
    assert_eq!(a.rebase(&b, Side::After).unwrap().splices[0].after, Some(after.body[1].id()));
    assert_eq!(b.rebase(&a, Side::Before).unwrap().splices[0].before, Some(after.body[2].id()));
}

#[test]
fn blocks_put_in_after_blocks_taken_away_go_where_those_were() {
    let d = sample();
    let (_, removed) = edit(&d, |d| _ = d.body.remove(1));
    let (_, put) = edit(&d, |d| d.body.insert(2, para_block(Paragraph::with_text("new", CharProps::default()))));
    let after = both_orders(&d, &put, &removed);
    assert_eq!((text(&after, 0), text(&after, 1), text(&after, 2)), ("one", "new", "three"));
    assert_eq!(put.rebase(&removed, Side::After).unwrap().splices[0].after, Some(d.body[0].id()));
    // Between two blocks taken away, they would have no place: a clash.
    let (_, two) = edit(&d, |d| _ = d.body.drain(1..3).count());
    assert!(clashes(&put, &two));
}

#[test]
fn a_change_moved_past_another_still_checks_what_it_replaces() {
    let d = sample();
    // "three" without "ree", and an "x" typed before it meanwhile.
    let (_, del) = edit(&d, |d| _ = d.delete_range(&Pos::body(2, 2), &Pos::body(2, 5)).unwrap());
    let (with_x, x) = edit(&d, type_at(2, 0, "x"));
    let moved = del.rebase(&x, Side::After).unwrap();
    assert_eq!(moved.edits[0].was, None);
    assert_eq!(moved.edits[0].chars.as_ref().map(|c| c.at), Some(3));
    let mut there = copy(&with_x);
    there.apply(&sent(&moved)).unwrap();
    assert_eq!(text(&there, 2), "xth");
    // Where the characters it replaces aren't, it is refused.
    let mut here = copy(&d);
    assert!(matches!(here.apply(&sent(&moved)), Err(DocError::Stale(_))));
    assert_same(&here, &d);
}

#[test]
fn junk_paragraph_edits_are_errors_not_panics() {
    let d = sample();
    let (_, cs) = edit(&d, type_at(1, 1, "é"));
    let table = d.body[3].id();
    let junk: Vec<Box<dyn Fn(&mut ParaEdit)>> = vec![
        Box::new(|e| e.chars.as_mut().unwrap().at = 99),
        Box::new(|e| e.chars.as_mut().unwrap().at = usize::MAX),
        Box::new(|e| {
            let c = e.chars.as_mut().unwrap();
            (c.at, c.removed) = (usize::MAX, 2);
        }),
        // Other characters than it was made on.
        Box::new(|e| e.chars.as_mut().unwrap().removed = 1),
        Box::new(|e| e.chars.as_mut().unwrap().inserted.runs.clear()),
        Box::new(|e| e.chars.as_mut().unwrap().inserted.runs[0].len = 1),
        Box::new(|e| {
            let c = e.chars.as_mut().unwrap();
            c.inserted.text.push(OBJ);
            c.inserted.runs[0].len += OBJ.len_utf8();
        }),
        Box::new(move |e| e.id = table),
        Box::new(|e| e.id = BlockId::UNSET),
        Box::new(|e| e.container = Container::Cell { table: BlockId::UNSET, row: 0, col: 0 }),
        Box::new(|e| e.container = Container::Story(StoryRef::Part(99))),
        Box::new(|e| e.shell = Some(ParaShell { was: Fingerprint(1), props: Default::default(), mark: Default::default(), section: None })),
        Box::new(|e| e.was = Some(Fingerprint(1))),
    ];
    for (i, f) in junk.iter().enumerate() {
        let mut bad = cs.clone();
        f(&mut bad.edits[0]);
        let mut other = copy(&d);
        assert!(other.apply(&bad).is_err(), "junk {i}: {bad:?}");
        assert_same(&other, &d);
        // Moving junk past a change, or a change past junk, doesn't panic either.
        let _ = bad.rebase(&cs, Side::After);
        let _ = cs.rebase(&bad, Side::Before);
    }
}

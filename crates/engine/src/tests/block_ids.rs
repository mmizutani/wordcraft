//! Block ids through commands (`wordcraft_doc::BlockId`): after every command each block has an
//! id of its own, a block a command leaves as it was keeps its id, the paragraph being typed in
//! keeps its id, and undo and redo bring the ids back with the text.

use std::collections::{HashMap, HashSet};

use proptest::prelude::*;
use serde_json::{Value, json};
use wordcraft_doc::{BlockId, Document, Path, StoryRef};

use crate::Session;
use crate::sample::sample_document;

/// Every block's id, in document order.
fn ids(d: &Document) -> Vec<BlockId> {
    let mut out = Vec::new();
    d.each_block(&mut |_, _, b| out.push(b.id()));
    out
}

/// A block without an id or with one another block has, if any.
fn id_problem(d: &Document) -> Option<String> {
    let mut seen = HashSet::new();
    let mut problem = None;
    d.each_block(&mut |story, path, b| {
        if problem.is_none() && (!b.id().is_set() || !seen.insert(b.id())) {
            problem = Some(format!("{story:?} {path:?} {:?}", b.id()));
        }
    });
    problem
}

/// Blocks that lost their id although what they hold is still there: the only block holding it
/// before and after, under a new id now, with the old one gone. A block a command left as it
/// was, or deleted and made again, must keep its id. (Which of two joined paragraphs keeps its
/// id is the edit's call, so a block that took another one's existing id isn't counted, and
/// neither is a new block that happens to hold what a changed one held.)
fn renamed(before: &Document, after: &Document) -> Vec<String> {
    let by_content = |d: &Document| {
        let mut m: HashMap<String, Vec<BlockId>> = HashMap::new();
        d.each_block(&mut |_, _, b| m.entry(serde_json::to_string(&**b).unwrap_or_default()).or_default().push(b.id()));
        m
    };
    let now = by_content(after);
    let (ids_before, ids_now): (HashSet<BlockId>, HashSet<BlockId>) = (ids(before).into_iter().collect(), ids(after).into_iter().collect());
    by_content(before)
        .into_iter()
        .filter(|(content, was)| {
            matches!((was.as_slice(), now.get(content).map(Vec::as_slice)),
                ([a], Some([b])) if a != b && !ids_now.contains(a) && !ids_before.contains(b))
        })
        .map(|(content, _)| content.chars().take(120).collect())
        .collect()
}

fn run(s: &mut Session, id: &str, v: Value) {
    s.run(id, &v).unwrap_or_else(|e| panic!("{id}: {e}"));
}

fn type_text(s: &mut Session, text: &str) {
    for c in text.chars() {
        run(s, "text.insert", json!({"text": c.to_string()}));
    }
}

fn body_id(s: &Session, i: usize) -> BlockId {
    s.doc.body.get(i).map(|b| b.id()).unwrap_or_default()
}

#[test]
fn opened_documents_have_an_id_for_every_block() {
    let s = Session::new(sample_document());
    assert_eq!(id_problem(&s.doc), None);
    let mut s = Session::new(Document::new());
    s.set_document(sample_document());
    assert_eq!(id_problem(&s.doc), None);
    s.add_document(Document::from_text("another\ndocument"));
    assert_eq!(id_problem(&s.doc), None);
}

#[test]
fn typing_keeps_the_paragraph_its_id_and_enter_makes_a_new_one() {
    let mut s = Session::new(Document::new());
    type_text(&mut s, "Hello");
    let hello = body_id(&s, 0);
    assert!(hello.is_set());
    // Enter at the end: the new paragraph after it is new.
    run(&mut s, "text.newParagraph", json!({}));
    type_text(&mut s, "world");
    let world = body_id(&s, 1);
    assert!(world.is_set() && world != hello);
    assert_eq!(body_id(&s, 0), hello);
    // Enter at the start of "Hello" adds an empty paragraph above it; "Hello" keeps its id.
    run(&mut s, "caret.docStart", json!({}));
    run(&mut s, "text.newParagraph", json!({}));
    assert_eq!(s.doc.body.len(), 3);
    assert_eq!((body_id(&s, 1), body_id(&s, 2)), (hello, world));
    assert!(![hello, world].contains(&body_id(&s, 0)));
    // Backspace takes the empty paragraph away again: still "Hello", still its id.
    run(&mut s, "text.backspace", json!({}));
    assert_eq!(s.doc.body.len(), 2);
    assert_eq!((body_id(&s, 0), body_id(&s, 1)), (hello, world));
    // Enter in the middle: the first half keeps the id.
    run(&mut s, "caret.right", json!({}));
    run(&mut s, "caret.right", json!({}));
    run(&mut s, "text.newParagraph", json!({}));
    assert_eq!(body_id(&s, 0), hello);
    assert_eq!(id_problem(&s.doc), None);
}

#[test]
fn typing_in_a_table_cell_keeps_the_ids_of_the_table_and_its_blocks() {
    let mut s = Session::new(Document::from_text("before"));
    run(&mut s, "insert.table", json!({"rows": 2, "cols": 2}));
    let before = ids(&s.doc);
    let table = s.doc.body.iter().position(|b| b.as_table().is_some()).unwrap();
    s.sel = crate::Selection::caret(wordcraft_doc::Pos::new(StoryRef::Body, Path(vec![table as u32, 1, 1, 0]), 0));
    type_text(&mut s, "cell");
    assert_eq!(ids(&s.doc), before);
    assert_eq!(s.doc.para(StoryRef::Body, &Path(vec![table as u32, 1, 1, 0])).map(|p| p.text.as_str()), Some("cell"));
}

#[test]
fn undo_and_redo_bring_the_ids_back() {
    let mut s = Session::new(sample_document());
    let start = ids(&s.doc);
    run(&mut s, "caret.docEnd", json!({}));
    run(&mut s, "text.newParagraph", json!({}));
    run(&mut s, "insert.table", json!({"rows": 2, "cols": 3}));
    // Out of the table (where Select All takes the table), then everything.
    run(&mut s, "caret.docStart", json!({}));
    run(&mut s, "select.all", json!({}));
    run(&mut s, "edit.copy", json!({}));
    run(&mut s, "caret.docEnd", json!({}));
    let blocks = s.doc.body.len();
    run(&mut s, "edit.paste", json!({}));
    let end = ids(&s.doc);
    assert_eq!(id_problem(&s.doc), None);
    assert!(s.doc.body.len() > blocks + 10, "the copy is in");
    // Undo to the start brings the first ids back, redo the last ones.
    while s.can_undo() {
        run(&mut s, "edit.undo", json!({}));
    }
    assert_eq!(ids(&s.doc), start);
    while s.can_redo() {
        run(&mut s, "edit.redo", json!({}));
    }
    assert_eq!(ids(&s.doc), end);
}

#[test]
fn every_command_leaves_each_block_an_id_of_its_own() {
    let reg = crate::cmd::registry();
    for spec in reg.all() {
        // These reach outside the session (files), as in `hostile_params_never_panic`.
        if spec.id.starts_with("file.") || spec.id == "insert.picture" || spec.id == "insert.textFromFile" {
            continue;
        }
        // At the caret, then on everything.
        for select_all in [false, true] {
            let mut s = Session::new(sample_document());
            if select_all {
                run(&mut s, "select.all", json!({}));
            }
            let _ = s.run(spec.id, &json!({}));
            assert_eq!(id_problem(&s.doc), None, "{} (select all: {select_all})", spec.id);
        }
    }
}

/// Edits the random test below found where a paragraph lost its id: Enter over everything,
/// starting in a paragraph with text and ending in an empty one after a table; a table over a
/// selection across a paragraph mark; a text box around everything.
#[test]
fn paragraphs_deleted_and_made_again_keep_their_ids() {
    let cases: [&[(&str, Value)]; 3] = [
        &[
            ("select.all", json!({})),
            ("text.insert", json!({"text": "a"})),
            ("insert.table", json!({"rows": 2, "cols": 2})),
            ("caret.left", json!({})),
            ("select.all", json!({})),
            ("text.newParagraph", json!({})),
        ],
        &[
            ("caret.docEnd", json!({})),
            ("caret.paraUp", json!({})),
            ("caret.left", json!({})),
            ("caret.paraDown", json!({"extend": true})),
            ("text.insert", json!({"text": " "})),
            ("edit.undo", json!({})),
            ("insert.table", json!({"rows": 2, "cols": 2})),
        ],
        &[("caret.docEnd", json!({})), ("text.newParagraph", json!({})), ("select.all", json!({})), ("insert.textBox", json!({}))],
    ];
    for steps in cases {
        let mut s = Session::new(sample_document());
        for (id, v) in steps {
            let before = s.doc.clone();
            let _ = s.run(id, v);
            assert_eq!(id_problem(&s.doc), None, "{id} in {steps:?}");
            assert_eq!(renamed(&before, &s.doc), Vec::<String>::new(), "{id} in {steps:?}");
        }
    }
}

/// One step of the random edits below (and of those in `changes.rs`).
#[derive(Clone, Debug)]
pub(super) enum Step {
    Type(String),
    Command(&'static str, Value),
    /// Undo, then redo: the ids are those before the undo.
    UndoRedo,
}

pub(super) fn step() -> impl Strategy<Value = Step> {
    let commands = [
        ("text.newParagraph", json!({})),
        ("text.backspace", json!({})),
        ("text.delete", json!({})),
        ("caret.left", json!({})),
        ("caret.right", json!({"extend": true})),
        ("caret.paraUp", json!({})),
        ("caret.paraDown", json!({"extend": true})),
        ("caret.docStart", json!({})),
        ("caret.docEnd", json!({})),
        ("select.all", json!({})),
        ("edit.copy", json!({})),
        ("edit.cut", json!({})),
        ("edit.paste", json!({})),
        ("edit.undo", json!({})),
        ("edit.redo", json!({})),
        ("insert.table", json!({"rows": 2, "cols": 2})),
        ("table.insertRowBelow", json!({})),
        ("table.deleteRow", json!({})),
        ("references.footnote", json!({})),
        ("review.newComment", json!({"text": "note"})),
        ("insert.textBox", json!({})),
    ];
    prop_oneof![
        3 => "[a-z ]{1,6}".prop_map(Step::Type),
        8 => proptest::sample::select(commands.to_vec()).prop_map(|(id, v)| Step::Command(id, v)),
        1 => Just(Step::UndoRedo),
    ]
}

proptest! {
    /// Random typing, paragraph breaks, deletions, cut, copy and paste, tables, notes, comments,
    /// text boxes, undo and redo: every block always has an id of its own, and blocks the step
    /// left as they were keep theirs.
    #[test]
    fn random_edits_keep_every_block_an_id_of_its_own(steps in proptest::collection::vec(step(), 1..40)) {
        let mut s = Session::new(sample_document());
        for st in &steps {
            let before = s.doc.clone();
            match st {
                Step::Type(t) => {
                    let _ = s.run("text.insert", &json!({"text": t}));
                }
                Step::Command(id, v) => {
                    let _ = s.run(id, v);
                }
                Step::UndoRedo => {
                    let was = ids(&s.doc);
                    if s.run("edit.undo", &json!({})).is_ok() && !before.same_content(&s.doc) && s.run("edit.redo", &json!({})).is_ok() {
                        prop_assert_eq!(ids(&s.doc), was, "undo, redo");
                    }
                }
            }
            prop_assert_eq!(id_problem(&s.doc), None, "after {:?}", st);
            prop_assert_eq!(renamed(&before, &s.doc), Vec::<String>::new(), "after {:?}", st);
        }
    }
}

#[test]
fn a_saved_word_file_opens_with_the_ids_it_was_saved_with() {
    // #38: the docx keeps block ids, so a copy saved and opened again names its blocks as the
    // room does, including those made after the document was opened.
    let dir = std::env::temp_dir().join(format!("wordcraft-engine-block-ids-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ids.docx");
    let mut s = Session::new(sample_document());
    run(&mut s, "caret.docEnd", json!({}));
    run(&mut s, "text.newParagraph", json!({}));
    type_text(&mut s, "made here");
    assert_eq!(id_problem(&s.doc), None);
    let before = ids(&s.doc);
    run(&mut s, "file.saveAs", json!({"path": path.to_string_lossy()}));
    let mut again = Session::new(Document::new());
    run(&mut again, "file.open", json!({"path": path.to_string_lossy()}));
    assert_eq!(id_problem(&again.doc), None);
    let after = ids(&again.doc);
    let lost: Vec<&BlockId> = before.iter().filter(|id| !after.contains(id)).collect();
    assert!(lost.is_empty(), "{} of {} ids lost: {lost:?}", lost.len(), before.len());
    let _ = std::fs::remove_dir_all(&dir);
}

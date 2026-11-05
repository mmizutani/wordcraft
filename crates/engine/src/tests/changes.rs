//! Changes through commands ([`crate::ChangeLog`], `document.changes`): every command that
//! changes the document records one change, and another copy of the document that applies the
//! changes, sent as JSON, holds what the document holds under the same block ids.

use proptest::prelude::*;
use serde_json::{Value, json};
use wordcraft_doc::{ChangeSet, Document};

use super::block_ids::{Step, step};
use crate::sample::sample_document;
use crate::{CmdError, MAX_CHANGES, Session};

/// Another copy of a session's active document, kept up to date from its changes alone.
struct Copy {
    doc: Document,
    document: u64,
    version: u64,
}

impl Copy {
    fn of(s: &Session) -> Copy {
        Copy { doc: s.changes().document().clone(), document: s.document_id(), version: s.changes().version() }
    }

    /// Apply the changes `s` recorded since the copy last looked, each sent as JSON, and check
    /// that the copy then holds what `s` holds. A command that made another document active
    /// starts a copy of that one.
    fn follow(&mut self, s: &Session) -> Result<(), String> {
        if s.document_id() != self.document {
            *self = Copy::of(s);
            return Ok(());
        }
        for (v, cs) in s.changes().since(self.version)? {
            let wire = serde_json::to_string(&*cs).map_err(|e| format!("change {v} can't be written: {e}"))?;
            let sent: ChangeSet = serde_json::from_str(&wire).map_err(|e| format!("change {v} can't be read back: {e}"))?;
            if sent != *cs {
                return Err(format!("change {v} reads back as another change"));
            }
            self.doc.apply(&sent).map_err(|e| format!("change {v} doesn't apply: {e}"))?;
            self.version = v;
        }
        if self.doc != s.doc {
            return Err(format!("the copy holds other content: {}", difference(&self.doc, &s.doc)));
        }
        if self.doc.block_ids() != s.doc.block_ids() {
            return Err("the copy holds the same content under other block ids".into());
        }
        Ok(())
    }
}

/// Where two documents differ, in short: the first field whose JSON differs.
fn difference(a: &Document, b: &Document) -> String {
    let (Ok(Value::Object(x)), Ok(Value::Object(y))) = (serde_json::to_value(a), serde_json::to_value(b)) else {
        return "(no JSON)".into();
    };
    let short = |v: Option<&Value>| v.map(|v| v.to_string().chars().take(300).collect::<String>()).unwrap_or_default();
    x.iter()
        .find(|(k, v)| y.get(*k) != Some(*v))
        .map(|(k, v)| format!("{k}: {} / {}", short(Some(v)), short(y.get(k))))
        .unwrap_or_else(|| "in something the JSON leaves out".into())
}

/// Whether the active document changed from `before`: its content or its block ids.
fn changed(before: &Document, s: &Session) -> bool {
    *before != s.doc || before.block_ids() != s.doc.block_ids()
}

/// Run `id` and check what it recorded: one change when it changed the document (whatever it
/// ran inside), none when it didn't; and that the copy, applying it, holds what `s` holds.
fn run_and_follow(s: &mut Session, copy: &mut Copy, id: &str, v: &Value) -> Result<Result<Value, CmdError>, String> {
    let (before, version, document) = (s.doc.clone(), s.changes().version(), s.document_id());
    let r = s.run(id, v);
    if s.document_id() == document {
        let want = version + u64::from(changed(&before, s));
        if s.changes().version() != want {
            return Err(format!("{id} {v}: version {} after it, {want} expected", s.changes().version()));
        }
    }
    copy.follow(s).map_err(|e| format!("{id} {v}: {e}"))?;
    Ok(r)
}

/// Typing, paragraph breaks and joins, formatting, styles, lists, settings, page setup, tables
/// and their rows, columns and merged cells, notes, comments, text boxes, headers, bookmarks,
/// tracked changes, undo, redo, cut and paste: a copy that applies only the changes holds the
/// same document, under the same ids, after each.
#[test]
fn a_copy_that_applies_the_changes_of_every_kind_of_edit_stays_the_same() {
    let steps = [
        ("caret.docStart", json!({})),
        ("text.insert", json!({"text": "Hello "})),
        ("text.newParagraph", json!({})),
        ("text.insert", json!({"text": "a line of its own"})),
        ("text.backspace", json!({})),
        ("caret.docStart", json!({})),
        ("caret.paraDown", json!({"extend": true})),
        ("text.delete", json!({})),
        ("select.text", json!({"text": "Studio"})),
        ("format.bold", json!({})),
        ("para.style", json!({"style": "Heading 2"})),
        ("para.bullets", json!({})),
        ("styles.modify", json!({"style": "Normal", "chr": {"italic": true}})),
        ("design.pageColor", json!({"color": "EEF2FF"})),
        ("layout.orientation", json!({"value": "landscape"})),
        ("caret.docEnd", json!({})),
        ("insert.table", json!({"rows": 2, "cols": 3})),
        ("text.insert", json!({"text": "in a cell"})),
        ("table.insertRowBelow", json!({})),
        ("table.insertColumnRight", json!({})),
        ("table.selectRow", json!({})),
        ("table.merge", json!({})),
        ("text.insert", json!({"text": "merged"})),
        ("table.deleteRow", json!({})),
        ("references.footnote", json!({})),
        ("text.insert", json!({"text": "a note"})),
        ("caret.docStart", json!({})),
        ("review.newComment", json!({"text": "a comment"})),
        ("caret.docStart", json!({})),
        ("insert.textBox", json!({})),
        ("caret.docStart", json!({})),
        ("insert.header", json!({"text": "A header"})),
        ("caret.docStart", json!({})),
        ("insert.bookmark", json!({"name": "here"})),
        ("review.trackChanges", json!({"value": true})),
        ("text.insert", json!({"text": "tracked "})),
        ("text.backspace", json!({})),
        ("edit.undo", json!({})),
        ("edit.undo", json!({})),
        ("edit.redo", json!({})),
        ("review.trackChanges", json!({"value": false})),
        ("select.all", json!({})),
        ("edit.cut", json!({})),
        ("edit.paste", json!({})),
        ("edit.undo", json!({})),
        ("edit.undo", json!({})),
        ("edit.redo", json!({})),
    ];
    let mut s = Session::new(sample_document());
    let mut copy = Copy::of(&s);
    for (id, v) in &steps {
        match run_and_follow(&mut s, &mut copy, id, v) {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => panic!("{id} {v} failed: {e}"),
            Err(e) => panic!("{e}"),
        }
    }
    assert!(s.changes().version() >= 30, "most steps changed the document: {}", s.changes().version());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    /// Random typing, paragraph breaks, deletions, cut, copy and paste, tables, notes, comments,
    /// text boxes, undo and redo: each step that changes the document records one change, and a
    /// copy that applies them stays the same document.
    #[test]
    fn a_copy_that_applies_the_changes_of_random_edits_stays_the_same(steps in proptest::collection::vec(step(), 1..40)) {
        let mut s = Session::new(sample_document());
        let mut copy = Copy::of(&s);
        for st in &steps {
            let r = match st {
                Step::Type(t) => run_and_follow(&mut s, &mut copy, "text.insert", &json!({"text": t})),
                Step::Command(id, v) => run_and_follow(&mut s, &mut copy, id, v),
                Step::UndoRedo => run_and_follow(&mut s, &mut copy, "edit.undo", &json!({}))
                    .and_then(|_| run_and_follow(&mut s, &mut copy, "edit.redo", &json!({}))),
            };
            prop_assert!(r.is_ok(), "after {:?}: {:?}", st, r.err());
        }
    }
}

/// Every command, from a few places in the sample document, with no parameters and with some
/// it commonly takes: whatever it changed is one change, and a copy that applies it stays the
/// same document. (Commands that reach files are left out, as in `hostile_params_never_panic`.)
#[test]
fn every_command_s_change_applies_to_a_copy() {
    let reg = crate::cmd::registry();
    let params =
        [json!({}), json!({"text": "Ab", "value": true, "style": "Heading 1", "rows": 2, "cols": 2, "name": "x", "color": "FF0000", "size": 14})];
    let places = [("caret.docStart", json!({})), ("select.text", json!({"text": "Studio"})), ("caret.docEnd", json!({})), ("select.all", json!({}))];
    let mut failures = Vec::new();
    let mut s = Session::new(sample_document());
    let mut copy = Copy::of(&s);
    for (i, spec) in reg.all().iter().enumerate() {
        if spec.id.starts_with("file.") || spec.id == "insert.picture" || spec.id == "insert.textFromFile" {
            continue;
        }
        // A fresh document now and then, so one command's damage doesn't hide the next ones'.
        if i % 16 == 0 {
            s = Session::new(sample_document());
            copy = Copy::of(&s);
        }
        for (k, v) in params.iter().enumerate() {
            let (place, at) = &places[(i + k) % places.len()];
            let placed = run_and_follow(&mut s, &mut copy, place, at);
            let ran = placed.and_then(|_| run_and_follow(&mut s, &mut copy, spec.id, v));
            if let Err(e) = ran {
                failures.push(e);
                s = Session::new(sample_document());
                copy = Copy::of(&s);
            }
        }
    }
    assert!(failures.is_empty(), "{} commands' changes didn't reach the copy:\n{}", failures.len(), failures.join("\n"));
}

/// A command run inside another one on the same document records nothing of its own: the
/// outer command's edit is one change, holding both.
#[test]
fn a_command_inside_another_records_one_change() {
    let outer = crate::CommandSpec::new("test.typeTwice", "Type Twice", "", |s, _| {
        s.run("text.insert", &json!({"text": "one "}))?;
        s.run("text.newParagraph", &json!({}))?;
        s.run("text.insert", &json!({"text": "two"}))
    });
    let mut specs = crate::cmd::registry().all().to_vec();
    specs.push(outer);
    let mut s = Session::new(sample_document());
    s.registry = std::sync::Arc::new(crate::Registry::new(specs));
    let mut copy = Copy::of(&s);
    let version = s.changes().version();
    run_and_follow(&mut s, &mut copy, "test.typeTwice", &json!({})).unwrap().unwrap();
    assert_eq!(s.changes().version(), version + 1);
    let changes = s.changes().since(version).unwrap();
    let inserted: String = changes[0].1.splices.iter().map(|sp| sp.text.inserted.as_str()).collect();
    assert!(inserted.contains("one ") && inserted.contains("two"), "{inserted:?}");
}

/// Putting a snapshot back outside a command (an input method, a dialog's Cancel) is a change
/// of its own; inside `edit.undo` it is part of the undo's.
#[test]
fn a_snapshot_put_back_is_a_change() {
    let mut s = Session::new(sample_document());
    let mut copy = Copy::of(&s);
    let snap = s.edit_snapshot();
    run_and_follow(&mut s, &mut copy, "text.insert", &json!({"text": "preview"})).unwrap().unwrap();
    let version = s.changes().version();
    s.restore(snap);
    assert_eq!(s.changes().version(), version + 1);
    copy.follow(&s).unwrap();
    assert_eq!(copy.doc, sample_document_as_opened());
}

/// The sample document as a session holds it, ids aside.
fn sample_document_as_opened() -> Document {
    Session::new(sample_document()).doc
}

/// Saving stamps the document's metadata, which copies follow, though saving is no edit.
#[test]
fn the_metadata_a_save_stamps_reaches_copies() {
    let mut s = Session::new(sample_document());
    let mut copy = Copy::of(&s);
    let version = s.changes().version();
    s.stamp_save();
    assert_eq!(s.changes().version(), version + 1);
    copy.follow(&s).unwrap();
    assert_eq!(copy.doc.core, s.doc.core);
}

/// A command addressed to a document in the background records its change there: the active
/// document's changes don't move.
#[test]
fn a_change_to_a_document_in_the_background_is_recorded_there() {
    let mut s = Session::new(sample_document());
    let first = s.document_id();
    s.run("file.new", &json!({})).unwrap();
    let second = s.document_id();
    assert_ne!(first, second);
    let before = s.with_document(first, |s| s.changes().version()).unwrap();
    s.with_document(first, |s| s.run("text.insert", &json!({"text": "elsewhere"}))).unwrap().unwrap();
    assert_eq!(s.document_id(), second);
    assert_eq!(s.changes().version(), 0, "the active document didn't change");
    assert_eq!(s.with_document(first, |s| s.changes().version()).unwrap(), before + 1);
    let r = s.with_document(first, |s| s.run("document.changes", &json!({"since": before}))).unwrap().unwrap();
    assert_eq!(r["document"], json!(first));
    assert_eq!(r["changes"][0]["edits"][0]["text"], "elsewhere", "{r}");
}

/// `document.changes` gives the changes after a version, at most `limit` of them, and refuses
/// a version the document never had or one older than the changes it keeps.
#[test]
fn document_changes_gives_what_followed_a_version_and_refuses_what_it_no_longer_keeps() {
    let mut s = Session::new(sample_document());
    let r = s.run("document.changes", &json!({})).unwrap();
    assert_eq!((r["version"].as_u64(), r["changes"].as_array().map(Vec::len)), (Some(0), Some(0)), "{r}");
    for _ in 0..MAX_CHANGES + 5 {
        s.run("text.insert", &json!({"text": "x"})).unwrap();
    }
    let now = s.changes().version();
    assert_eq!(now, (MAX_CHANGES + 5) as u64);
    let r = s.run("document.changes", &json!({"since": now - 2})).unwrap();
    assert_eq!(r["changes"].as_array().map(|c| c.iter().map(|c| c["version"].clone()).collect::<Vec<_>>()), Some(vec![json!(now - 1), json!(now)]));
    assert!(r.get("more").is_none(), "{r}");
    let r = s.run("document.changes", &json!({"since": now - 3, "limit": 2})).unwrap();
    assert_eq!((r["changes"].as_array().map(Vec::len), &r["more"]), (Some(2), &json!(true)), "{r}");
    let r = s.run("document.changes", &json!({"since": now - 1, "full": true})).unwrap();
    let sent: ChangeSet = serde_json::from_value(r["changes"][0]["change"].clone()).unwrap();
    assert_eq!(sent, *s.changes().since(now - 1).unwrap()[0].1);
    for bad in [json!({"since": 0}), json!({"since": now + 1}), json!({"since": -1}), json!({"since": "1"}), json!({"limit": 1.5})] {
        assert!(matches!(s.run("document.changes", &bad), Err(CmdError::Params(_))), "{bad}");
    }
    // The document read whole gives the version to follow from.
    assert_eq!(s.run("document.inspect", &json!({"text": false})).unwrap()["version"], json!(now));
}

/// Opening another document in the window's place starts its changes again: the version counts
/// that document's changes, under its own id.
#[test]
fn a_new_document_starts_its_changes_at_version_zero() {
    let mut s = Session::new(sample_document());
    s.run("text.insert", &json!({"text": "x"})).unwrap();
    assert_eq!(s.changes().version(), 1);
    let id = s.document_id();
    s.set_document(sample_document());
    assert_ne!(s.document_id(), id);
    assert_eq!(s.changes().version(), 0);
}

// The model's two findings, replayed on the real code. Not compiled: to run them, paste this at
// the end of crates/engine/src/recovery/desktop/tests.rs, then run
//   cargo test -p wordcraft-engine --lib finding_
// A test that PASSES means the finding is still there: its last assertion is the lost work.
// Whoever fixes a finding turns its test around into the regression test CLAUDE.md asks for.

#[test]
fn finding_discarding_a_held_copy_leaves_the_restored_document_without_one() {
    let t = Temp::new();
    let mut s = session(&t.0);
    s.run("text.insert", &json!({"text": "Restored draft"})).unwrap();
    save_now(&mut s);
    drop(s);
    let mut s = session(&t.0);
    let first = s.document_id();
    let id = s.recovery_documents()[0].id.clone();
    s.run("file.recover", &json!({"restore": id})).unwrap();
    s.run("file.new", &json!({})).unwrap();
    settle(&mut s);
    assert_eq!(s.recovery_documents().len(), 1, "the copy waiting in held is still listed");
    s.run("file.recover", &json!({"discard": id})).unwrap();
    settle(&mut s);
    assert!(records(&t.0).is_empty(), "the discard deleted the record");
    s.activate(first).unwrap();
    settle(&mut s);
    assert_eq!(s.doc.plain_text(StoryRef::Body), "Restored draft", "the held copy still opened");
    assert!(s.dirty);
    save_now(&mut s);
    save_now(&mut s);
    assert!(records(&t.0).is_empty(), "no copy is written for the dirty restored document");
    drop(s);
    let s = session(&t.0);
    assert!(s.recovery_documents().is_empty(), "after a crash nothing can be recovered");
}

#[test]
fn finding_a_discard_failing_late_leaves_a_dirty_document_without_a_copy() {
    let t = Temp::new();
    let mut s = session(&t.0);
    s.run("text.insert", &json!({"text": "Unsaved work"})).unwrap();
    save_now(&mut s);
    let id = records(&t.0).remove(0);
    let dir = t.0.join(&id);
    assert_eq!(Store::files(&dir).unwrap().len(), 1);
    // Store::discard deletes the generations, then fails unlinking owner.lock.
    fs::remove_file(dir.join("owner.lock")).unwrap();
    let value = "synthetic";
    assert!(s.run("file.encrypt", &json!({"password": value})).is_err(), "the password is refused");
    assert!(s.password.is_none() && s.dirty);
    assert!(Store::files(&dir).unwrap().is_empty(), "but the copy is already gone");
    save_now(&mut s);
    save_now(&mut s);
    assert!(Store::files(&dir).unwrap().is_empty(), "and no new copy is written");
    drop(s);
    let s = session(&t.0);
    assert!(s.recovery_documents().is_empty(), "after a crash nothing can be recovered");
}

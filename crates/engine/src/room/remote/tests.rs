use wordcraft_doc::{ChangeSet, Document, Pos, StoryRef, change::Side};

use super::{RemoteReplica, ReplicaError};

fn checkpoint(text: &str) -> Document {
    let mut doc = Document::from_text(text);
    doc.ensure_block_ids(None);
    doc
}

fn insert(doc: &Document, at: usize, text: &str) -> ChangeSet {
    let mut after = doc.clone();
    after.insert_text(&Pos::body(0, at), text, &Default::default()).unwrap();
    ChangeSet::between(doc, &after)
}

fn delete(doc: &Document, start: usize, end: usize) -> ChangeSet {
    let mut after = doc.clone();
    after.delete_range(&Pos::body(0, start), &Pos::body(0, end)).unwrap();
    ChangeSet::between(doc, &after)
}

fn text(doc: &Document) -> String {
    doc.plain_text(StoryRef::Body)
}

#[test]
fn lost_receipt_retries_the_exact_proposal_and_own_replay_applies_once() {
    let base = checkpoint("abcd");
    let local = insert(&base, 2, "X");
    let mut replica = RemoteReplica::new(17, base.clone());
    replica.queue(&local).unwrap();
    let proposal = replica.prepare("mine").unwrap().unwrap().clone();
    assert_eq!(proposal.base_revision, 17);
    assert_eq!(proposal.operation_id, "mine");
    assert_eq!(serde_json::from_slice::<ChangeSet>(&proposal.change_json).unwrap(), local);

    assert_eq!(replica.prepare("fresh-id").unwrap(), Some(&proposal));
    assert_eq!(text(replica.document()), "abXcd");
    assert_eq!(replica.confirmed(), &base);

    assert!(replica.integrate(18, "mine", &local).unwrap().is_none());
    assert!(replica.integrate(18, "mine", &local).unwrap().is_none());
    assert_eq!(replica.revision(), 18);
    assert_eq!(replica.pending_len(), 0);
    assert_eq!(text(replica.confirmed()), "abXcd");
    assert_eq!(replica.document(), replica.confirmed());
    assert!(replica.in_flight().is_none());
}

#[test]
fn disjoint_replay_moves_the_pending_queue_but_keeps_the_retry_unchanged() {
    let base = checkpoint("abcd");
    let first = insert(&base, 2, "X");
    let remote = insert(&base, 1, "é");
    let mut replica = RemoteReplica::new(0, base);
    replica.queue(&first).unwrap();
    let proposal = replica.prepare("first").unwrap().unwrap().clone();
    let second = insert(replica.document(), 3, "Y");
    replica.queue(&second).unwrap();
    let mut visible = replica.document().clone();

    let mapped = replica.integrate(1, "other", &remote).unwrap().unwrap();
    visible.apply(&mapped).unwrap();
    assert_eq!(replica.document(), &visible);
    assert_eq!(text(replica.document()), "aébXYcd");
    assert_eq!(text(replica.confirmed()), "aébcd");
    assert_eq!(replica.in_flight(), Some(&proposal));

    let first = first.rebase(&remote, Side::After).unwrap();
    let mut after = replica.confirmed().clone();
    after.apply(&first).unwrap();
    let canonical = ChangeSet::between(replica.confirmed(), &after);
    assert_ne!(canonical, first);
    assert!(replica.integrate(2, "first", &canonical).unwrap().is_none());
    let next = replica.prepare("second").unwrap().unwrap().clone();
    assert_eq!(next.base_revision, 2);
    let second = serde_json::from_slice(&next.change_json).unwrap();
    assert!(replica.integrate(3, "second", &second).unwrap().is_none());
    assert_eq!(text(replica.confirmed()), "aébXYcd");
    assert_eq!(replica.document(), replica.confirmed());
    assert_eq!(replica.pending_len(), 0);
}

#[test]
fn conflicting_replay_freezes_the_local_recovery_copy_and_retains_the_uncertain_submission() {
    let base = checkpoint("abcd");
    let local = delete(&base, 0, 2);
    let remote = delete(&base, 1, 3);
    let mut replica = RemoteReplica::new(0, base.clone());
    replica.queue(&local).unwrap();
    let proposal = replica.prepare("mine").unwrap().unwrap().clone();
    let later = insert(replica.document(), 2, "!");
    replica.queue(&later).unwrap();

    assert!(matches!(replica.integrate(1, "other", &remote), Err(ReplicaError::Conflict(_))));
    assert!(replica.needs_recovery());
    assert_eq!(replica.revision(), 0);
    assert_eq!(replica.confirmed(), &base);
    assert_eq!(text(replica.document()), "cd!");
    assert_eq!(replica.pending_len(), 2);
    assert_eq!(replica.in_flight(), Some(&proposal));
    assert!(matches!(replica.queue(&local), Err(ReplicaError::RecoveryRequired)));
}

#[test]
fn revision_gap_leaves_the_replica_ready_to_replay_the_missing_entry() {
    let base = checkpoint("abcd");
    let first = insert(&base, 0, "A");
    let mut after = base.clone();
    after.apply(&first).unwrap();
    let second = insert(&after, 5, "Z");
    let mut replica = RemoteReplica::new(10, base.clone());

    assert!(matches!(replica.integrate(12, "other-two", &second), Err(ReplicaError::Gap { expected: 11, received: 12 })));
    assert!(!replica.needs_recovery());
    assert_eq!(replica.revision(), 10);
    assert_eq!(replica.document(), &base);
    replica.integrate(11, "other-one", &first).unwrap();
    replica.integrate(12, "other-two", &second).unwrap();
    assert_eq!(replica.revision(), 12);
    assert_eq!(text(replica.confirmed()), "AabcdZ");
}

#[test]
fn pending_queue_refuses_large_changes_and_stops_at_thirty_two_operations() {
    let base = checkpoint("");
    let mut replica = RemoteReplica::new(0, base);
    let oversized = insert(replica.document(), 0, &"x".repeat(256 * 1024));
    assert!(matches!(replica.queue(&oversized), Err(ReplicaError::PendingLimit)));
    assert_eq!(text(replica.document()), "");
    assert_eq!(replica.pending_len(), 0);

    for at in 0..32 {
        let change = insert(replica.document(), at, "x");
        replica.queue(&change).unwrap();
    }
    let extra = insert(replica.document(), 32, "y");
    assert!(matches!(replica.queue(&extra), Err(ReplicaError::PendingLimit)));
    assert_eq!(text(replica.document()), "x".repeat(32));
    assert_eq!(replica.pending_len(), 32);
}

#[test]
fn pending_byte_limit_preserves_the_visible_work() {
    let mut replica = RemoteReplica::new(0, checkpoint(""));
    let piece = "x".repeat(128 * 1024);
    for index in 0..15 {
        let change = insert(replica.document(), index * piece.len(), &piece);
        replica.queue(&change).unwrap();
    }
    let change = insert(replica.document(), 15 * piece.len(), &piece);
    assert!(matches!(replica.queue(&change), Err(ReplicaError::PendingLimit)));
    assert_eq!(text(replica.document()).len(), 15 * piece.len());
    assert_eq!(replica.pending_len(), 15);
}

#[test]
fn a_mismatched_own_acceptance_preserves_local_work_for_recovery() {
    let base = checkpoint("abcd");
    let mine = insert(&base, 0, "A");
    let unexpected = insert(&base, 0, "Z");
    let mut replica = RemoteReplica::new(3, base.clone());
    replica.queue(&mine).unwrap();
    let proposal = replica.prepare("mine").unwrap().unwrap().clone();

    assert!(matches!(replica.integrate(4, "mine", &unexpected), Err(ReplicaError::AcceptedMismatch)));
    assert!(replica.needs_recovery());
    assert_eq!(replica.revision(), 3);
    assert_eq!(replica.confirmed(), &base);
    assert_eq!(text(replica.document()), "Aabcd");
    assert_eq!(replica.in_flight(), Some(&proposal));
}

//! Roles in a room ([`crate::room::Role`]): invites bound to a name and a role, and what each
//! role lets a participant change, in their copy and at the hub.

use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::TestRunner;
use serde_json::{Value, json};
use wordcraft_doc::{ChangeSet, Document, RevisionKind, StoryRef};

use super::block_ids::{Step, step};
use super::changes::{edit_step, run_step};
use super::room::{join, run, same_everywhere, settle, smooth, text};
use crate::room::{Bounce, Hub, ParticipantId, Replica, Role, Room, RoomError, rules};
use crate::sample::sample_document;
use crate::{CmdError, Session};

/// Join `room` as `name` with `role`.
fn join_as(room: &std::sync::Arc<Room>, name: &str, role: Role) -> Replica {
    match room.join(name, role) {
        Ok(r) => r,
        Err(e) => panic!("{name} can't join: {e}"),
    }
}

/// `r` refuses command `id`, and its document is as it was.
fn refused(r: &mut Replica, id: &str, v: Value) {
    let before = r.session().doc.clone();
    match r.run(id, &v) {
        Err(CmdError::Disabled(_)) => {}
        other => panic!("{} ran {id} {v}: {other:?}", r.participant().name),
    }
    assert_eq!(r.session().doc, before, "{id} {v} left a change behind");
}

/// The text of `doc` once every tracked change is accepted (`accept`) or rejected.
fn resolved(doc: &Document, accept: bool) -> String {
    let mut s = Session::new(doc.clone());
    let id = if accept { "review.acceptAll" } else { "review.rejectAll" };
    s.run(id, &json!({})).unwrap();
    s.doc.plain_text(StoryRef::Body)
}

/// The change `f` makes to `base` in a session of `author`'s, with Track Changes on (`track`)
/// while it runs: the document's setting is as it was.
fn change_by(base: &Document, author: &str, track: bool, f: impl FnOnce(&mut Session)) -> ChangeSet {
    let mut s = Session::new(base.clone());
    s.author = author.into();
    let was = s.doc.settings.track_changes;
    s.doc.settings.track_changes = track;
    f(&mut s);
    s.doc.settings.track_changes = was;
    ChangeSet::between(base, &s.doc)
}

#[test]
fn an_invite_lets_whoever_holds_its_key_in_under_its_name_and_role() {
    let room = Room::new(Document::from_text("Hello"));
    let key = room.invite("Ana", Role::Suggester).unwrap();
    assert_eq!(key.len(), 32);
    assert!(key.bytes().all(|b| b.is_ascii_hexdigit()));
    let other = room.invite("Ben", Role::Viewer).unwrap();
    assert_ne!(key, other);
    let ana = room.join_with(&key).unwrap();
    assert_eq!((ana.participant().name.as_str(), ana.participant().role), ("Ana", Role::Suggester));
    assert_eq!((ana.session().author.as_str(), ana.session().role), ("Ana", Some(Role::Suggester)));
    // The same key lets them in again (another copy of theirs), under the same name.
    let again = room.join_with(&key).unwrap();
    assert_eq!(again.participant().name, "Ana");
    assert_ne!(again.id(), ana.id());
    // A key no invite has, or one taken back, lets no one in.
    for wrong in ["", "nonsense", &key[..31], &key.to_uppercase(), &"0".repeat(32)] {
        assert!(matches!(room.join_with(wrong), Err(RoomError::UnknownKey)), "{wrong:?}");
    }
    let names = |room: &Room| room.participants().into_iter().map(|p| (p.id.0, p.name, p.role)).collect::<Vec<_>>();
    assert_eq!(names(&room), [(1, "Ana".to_string(), Role::Suggester), (2, "Ana".into(), Role::Suggester)]);
    // Taken back, the key lets no one in, and the copies that joined with it are out.
    assert!(room.revoke(&key));
    assert!(!room.revoke(&key));
    assert!(matches!(room.join_with(&key), Err(RoomError::UnknownKey)));
    assert!(!room.is_in(ana.id()) && !room.is_in(again.id()));
    let ben = room.join_with(&other).unwrap();
    assert_eq!(names(&room), [(3, "Ben".to_string(), Role::Viewer)]);
    assert!(room.is_in(ben.id()));
    // Keys stay out of the hub's debug output.
    let mut hub = Hub::new(Document::from_text("Hello"));
    let key = hub.invite("Eve", Role::Editor).unwrap();
    assert!(!format!("{hub:?}").contains(&key));
}

#[test]
fn no_one_takes_another_s_name() {
    let room = Room::new(Document::from_text("Hello"));
    room.invite("Ana", Role::Suggester).unwrap();
    // No one claims an invited name, or one a participant has, without the key.
    assert!(matches!(room.join("ana", Role::Owner), Err(RoomError::Name(_))));
    assert!(matches!(room.invite("ANA", Role::Owner), Err(RoomError::Name(_))));
    let cy = join(&room, "Cy");
    assert!(matches!(room.invite("Cy", Role::Owner), Err(RoomError::Name(_))));
    assert_eq!(cy.participant().role, Role::Editor);
    // Names: not empty, not too long, no control characters; spaces around them go.
    for bad in ["", "   ", "a\nb", "tab\there", &"x".repeat(129)] {
        assert!(matches!(room.invite(bad, Role::Viewer), Err(RoomError::Name(_))), "{bad:?}");
        assert!(matches!(room.join(bad, Role::Viewer), Err(RoomError::Name(_))), "{bad:?}");
    }
    assert_eq!(join(&room, "  Dee  ").participant().name, "Dee");
    assert_eq!(join(&room, &"é".repeat(128)).participant().name.chars().count(), 128);
}

#[test]
fn roles_have_names_and_an_order() {
    for role in Role::ALL {
        assert_eq!(Role::named(role.name()), Some(role));
        assert_eq!(role.to_string(), role.name());
    }
    assert_eq!(Role::named("admin"), None);
    assert!(Role::ALL.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(serde_json::to_value(Role::Suggester).unwrap(), json!("suggester"));
}

#[test]
fn a_viewer_reads_and_moves_about_but_changes_nothing() {
    let room = Room::new(Document::from_text("Hello world"));
    let mut ed = join(&room, "Ed");
    let mut vi = join_as(&room, "Vi", Role::Viewer);
    run(&mut vi, "select.text", json!({"text": "world"}));
    run(&mut vi, "view.zoom", json!({"value": 150}));
    assert!(run(&mut vi, "styles.list", json!({})).is_array());
    assert!(run(&mut vi, "review.comments", json!({})).is_array());
    for (id, v) in [
        ("text.insert", json!({"text": "x"})),
        ("text.backspace", json!({})),
        ("format.bold", json!({})),
        ("review.newComment", json!({"text": "Hi"})),
        ("design.pageColor", json!({"color": "FF0000"})),
    ] {
        refused(&mut vi, id, v);
    }
    assert!(!vi.has_unconfirmed());
    // The hub takes nothing from a viewer.
    let base = room.document().1;
    let change = change_by(&base, "Vi", false, |s| {
        s.run("text.insert", &json!({"text": "x"})).unwrap();
    });
    let mut hub = Hub::new(base);
    let id = hub.admit("Vi", Role::Viewer).unwrap().id;
    assert!(matches!(hub.submit(0, &change, id), Err(Bounce::Refused(_))));
    // They see everyone else's changes.
    run(&mut ed, "caret.docEnd", json!({}));
    run(&mut ed, "text.insert", json!({"text": "!"}));
    let mut copies = [ed, vi];
    settle(&mut copies).unwrap();
    same_everywhere(&copies).unwrap();
    smooth(&copies);
    assert_eq!(text(&copies[1]), "Hello world!");
}

#[test]
fn a_commenter_comments_and_changes_nothing_else() {
    let room = Room::new(Document::from_text("Hello world"));
    let mut ed = join(&room, "Ed");
    run(&mut ed, "select.text", json!({"text": "Hello"}));
    run(&mut ed, "review.newComment", json!({"text": "Ed's note"}));
    let cy = join_as(&room, "Cy", Role::Commenter);
    let mut copies = [ed, cy];
    settle(&mut copies).unwrap();
    let [_, cy] = &mut copies;
    let eds = cy.session().doc.comments.keys().copied().next().unwrap();
    run(cy, "select.all", json!({}));
    for (id, v) in [
        ("text.insert", json!({"text": "x"})),
        ("format.bold", json!({})),
        ("review.trackChanges", json!({"value": true})),
        ("design.pageColor", json!({"color": "FF0000"})),
        ("review.editComment", json!({"id": eds, "text": "Mine now"})),
        ("review.deleteComment", json!({"id": eds})),
    ] {
        refused(cy, id, v);
    }
    // Comments and replies of their own, which they edit and delete; and any discussion
    // they resolve, as in Word.
    run(cy, "review.resolveComment", json!({"id": eds, "value": true}));
    run(cy, "review.reply", json!({"id": eds, "text": "Agreed"}));
    run(cy, "select.text", json!({"text": "world"}));
    run(cy, "review.newComment", json!({"text": "Whose world?"}));
    run(cy, "review.newComment", json!({"text": "Second thoughts"}));
    let theirs: Vec<u32> = cy.session().doc.comments.iter().filter(|(_, c)| c.author == "Cy").map(|(id, _)| *id).collect();
    assert_eq!(theirs.len(), 3);
    let (first, last) = (theirs[1], theirs[2]);
    run(cy, "review.editComment", json!({"id": first, "text": "Which world?"}));
    run(cy, "review.deleteComment", json!({"id": last}));
    settle(&mut copies).unwrap();
    same_everywhere(&copies).unwrap();
    smooth(&copies);
    let doc = &copies[0].session().doc;
    assert_eq!(doc.plain_text(StoryRef::Body), "Hello world");
    assert!(doc.comments.get(&eds).is_some_and(|c| c.resolved && c.author == "Ed"));
    let mut notes: Vec<(String, String)> = doc.comments.values().map(|c| (c.author.clone(), doc.plain_text(StoryRef::Part(c.part)))).collect();
    notes.sort();
    assert_eq!(notes, [("Cy".into(), "Agreed".into()), ("Cy".into(), "Which world?".into()), ("Ed".into(), "Ed's note".into())]);
}

#[test]
fn a_suggester_s_edits_are_tracked_changes_in_their_name() {
    let room = Room::new(Document::from_text("Hello world"));
    let ed = join(&room, "Ed");
    let mut sam = join_as(&room, "Sam", Role::Suggester);
    run(&mut sam, "select.text", json!({"text": "world"}));
    run(&mut sam, "text.insert", json!({"text": "there"}));
    run(&mut sam, "select.text", json!({"text": "Hello"}));
    run(&mut sam, "format.bold", json!({}));
    run(&mut sam, "caret.docEnd", json!({}));
    run(&mut sam, "text.newParagraph", json!({}));
    // AutoCorrect's fix is tracked too.
    for t in ["teh", " ", "end"] {
        run(&mut sam, "text.insert", json!({"text": t}));
    }
    // Their own settings: tracking is theirs whatever these say.
    assert!(!sam.session().doc.settings.track_changes);
    let mut copies = [ed, sam];
    settle(&mut copies).unwrap();
    same_everywhere(&copies).unwrap();
    smooth(&copies);
    let (_, doc) = room.document();
    assert!(!doc.settings.track_changes, "the document's Track Changes setting is as it was");
    assert!(doc.revisions.iter().all(|r| r.author == "Sam"));
    for kind in [RevisionKind::Insert, RevisionKind::Delete, RevisionKind::Format] {
        assert!(doc.revisions.iter().any(|r| r.kind == kind), "{kind:?}");
    }
    // One formatting revision for one formatting command.
    assert_eq!(doc.revisions.iter().filter(|r| r.kind == RevisionKind::Format).count(), 1);
    assert_eq!(resolved(&doc, false), "Hello world");
    assert_eq!(resolved(&doc, true), "Hello there\nThe end");
    // An editor accepts them.
    let [ed, _] = &mut copies;
    run(ed, "review.acceptAll", json!({}));
    settle(&mut copies).unwrap();
    same_everywhere(&copies).unwrap();
    let doc = room.document().1;
    assert!(doc.revisions.is_empty());
    assert_eq!(doc.plain_text(StoryRef::Body), "Hello there\nThe end");
    let first = doc.body.first().and_then(|b| b.as_para()).unwrap();
    assert_eq!(first.runs.first().map(|r| (r.len, r.props.bold)), Some((5, Some(true))));
}

#[test]
fn a_suggester_leaves_others_tracked_changes_for_someone_else() {
    let room = Room::new(Document::from_text("Hello world"));
    let mut ed = join(&room, "Ed");
    run(&mut ed, "review.trackChanges", json!({"value": true}));
    run(&mut ed, "caret.docEnd", json!({}));
    run(&mut ed, "text.insert", json!({"text": "!"}));
    let sam = join_as(&room, "Sam", Role::Suggester);
    let mut copies = [ed, sam];
    settle(&mut copies).unwrap();
    let [_, sam] = &mut copies;
    for id in ["review.acceptAll", "review.rejectAll"] {
        refused(sam, id, json!({}));
    }
    run(sam, "caret.docEnd", json!({}));
    run(sam, "caret.left", json!({"extend": true}));
    for id in ["review.accept", "review.reject"] {
        refused(sam, id, json!({}));
    }
    // Typing over Ed's insertion suggests deleting it; it stays Ed's to accept or reject.
    run(sam, "text.insert", json!({"text": "?"}));
    // Track Changes stays on for them, and the document's setting is the editors'.
    refused(sam, "review.trackChanges", json!({"value": false}));
    assert!(sam.session().doc.settings.track_changes);
    // Their own suggestions they undo, or withdraw.
    run(sam, "caret.docStart", json!({}));
    run(sam, "text.insert", json!({"text": "Oh, "}));
    run(sam, "edit.undo", json!({}));
    run(sam, "text.insert", json!({"text": "Well, "}));
    run(sam, "select.text", json!({"text": "Well, "}));
    run(sam, "review.reject", json!({}));
    // Formatting the page, or a style, isn't a tracked change: not theirs to do.
    refused(sam, "design.pageColor", json!({"color": "FF0000"}));
    settle(&mut copies).unwrap();
    same_everywhere(&copies).unwrap();
    smooth(&copies);
    let doc = room.document().1;
    assert_eq!(resolved(&doc, false), "Hello world");
    assert_eq!(resolved(&doc, true), "Hello world?");
    assert!(doc.revisions.iter().any(|r| r.author == "Ed" && r.kind == RevisionKind::Insert));
    assert!(!doc.revisions.iter().any(|r| r.author == "Sam" && r.kind == RevisionKind::Insert && resolved(&doc, true).contains("Well")));
}

#[test]
fn a_suggester_and_an_editor_typing_in_one_paragraph_both_get_through() {
    let room = Room::new(Document::from_text("Hello world"));
    let mut ed = join(&room, "Ed");
    let mut sam = join_as(&room, "Sam", Role::Suggester);
    run(&mut ed, "caret.docEnd", json!({}));
    run(&mut sam, "caret.docStart", json!({}));
    for i in 0..6 {
        run(&mut ed, "text.insert", json!({"text": format!("e{i}")}));
        run(&mut sam, "text.insert", json!({"text": format!("s{i}")}));
        if i % 2 == 0 {
            ed.sync();
        } else {
            sam.sync();
        }
    }
    let mut copies = [ed, sam];
    settle(&mut copies).unwrap();
    same_everywhere(&copies).unwrap();
    smooth(&copies);
    let doc = room.document().1;
    assert_eq!(resolved(&doc, false), "Hello worlde0e1e2e3e4e5");
    assert_eq!(resolved(&doc, true), "s0s1s2s3s4s5Hello worlde0e1e2e3e4e5");
}

#[test]
fn the_hub_holds_each_change_to_its_participant_s_role() {
    let mut hub = Hub::new(Document::from_text("Hello world"));
    let base = hub.document().clone();
    let cy = hub.admit("Cy", Role::Commenter).unwrap().id;
    let sam = hub.admit("Sam", Role::Suggester).unwrap().id;
    let ed = hub.admit("Ed", Role::Editor).unwrap().id;
    let type_at_end = |s: &mut Session| {
        s.run("caret.docEnd", &json!({})).unwrap();
        s.run("text.insert", &json!({"text": "!"})).unwrap();
    };
    // An edit made without Track Changes: an editor's only.
    let plain = change_by(&base, "Sam", false, type_at_end);
    for by in [cy, sam, ParticipantId(99), ParticipantId(0)] {
        assert!(matches!(hub.submit(0, &plain, by), Err(Bounce::Refused(_))), "{by}");
    }
    // A tracked change in another's name.
    let as_ed = change_by(&base, "Ed", true, type_at_end);
    assert!(matches!(hub.submit(0, &as_ed, sam), Err(Bounce::Refused(_))));
    // In their own name.
    let as_sam = change_by(&base, "Sam", true, type_at_end);
    assert_eq!(hub.submit(0, &as_sam, sam).map(|t| t.version), Ok(1));
    // Accepting it is not for them, nor for a commenter.
    let base = hub.document().clone();
    let accept = change_by(&base, "Sam", true, |s| {
        s.run("review.acceptAll", &json!({})).unwrap();
    });
    assert!(matches!(hub.submit(1, &accept, sam), Err(Bounce::Refused(_))));
    assert!(matches!(hub.submit(1, &accept, cy), Err(Bounce::Refused(_))));
    // A comment: in the commenter's name, not another's.
    let comment = |author: &str| {
        change_by(&base, author, false, |s| {
            s.run("select.text", &json!({"text": "Hello"})).unwrap();
            s.run("review.newComment", &json!({"text": "Hm"})).unwrap();
        })
    };
    assert!(matches!(hub.submit(1, &comment("Ed"), cy), Err(Bounce::Refused(_))));
    assert_eq!(hub.submit(1, &comment("Cy"), cy).map(|t| t.version), Ok(2));
    // An editor changes anything: here they accept Sam's suggestion.
    assert_eq!(hub.submit(1, &accept, ed).map(|t| (t.version, t.moved_past)), Ok((3, 1)));
    assert!(hub.document().revisions.is_empty());
    assert_eq!(hub.document().plain_text(StoryRef::Body), "Hello world!");
    // What another's suggestion covers stays theirs: a suggester changes neither its text nor
    // its formatting, though rejecting it would take either away.
    let base = hub.document().clone();
    let by_ed = change_by(&base, "Ed", true, |s| {
        s.run("caret.docStart", &json!({})).unwrap();
        s.run("text.insert", &json!({"text": "Oh "})).unwrap();
    });
    assert_eq!(hub.submit(3, &by_ed, ed).map(|t| t.version), Ok(4));
    let base = hub.document().clone();
    let inside_eds = |f: &dyn Fn(&mut wordcraft_doc::Paragraph)| {
        let mut d = base.clone();
        let path = d.para_paths(StoryRef::Body).into_iter().next().unwrap();
        f(d.para_mut(StoryRef::Body, &path).unwrap());
        ChangeSet::between(&base, &d)
    };
    let rewritten = inside_eds(&|p| {
        p.text.replace_range(0..2, "Ah");
        p.touch();
    });
    let reformatted = inside_eds(&|p| p.format(0, 2, &|c| c.bold = Some(true)).unwrap());
    for crafted in [&rewritten, &reformatted] {
        let took = hub.submit(4, crafted, sam);
        assert!(matches!(&took, Err(Bounce::Refused(why)) if why.contains("leaves Ed's tracked changes")), "{took:?}");
    }
    // Their own tracked changes on it are theirs to make: deleting some of it, formatting it.
    let tracked = change_by(&base, "Sam", true, |s| {
        s.run("caret.docStart", &json!({})).unwrap();
        s.run("caret.right", &json!({"extend": true})).unwrap();
        s.run("format.bold", &json!({})).unwrap();
        s.run("caret.right", &json!({})).unwrap();
        s.run("text.delete", &json!({})).unwrap();
    });
    assert_eq!(hub.submit(4, &tracked, sam).map(|t| t.version), Ok(5));
    let doc = hub.document();
    assert_eq!(resolved(doc, false), "Hello world!");
    assert_eq!(resolved(doc, true), "O Hello world!");
}

/// Enter copies a paragraph's properties, with someone else's tracked change on them, to the
/// paragraph it makes. A suggester splits paragraphs an editor restyled with Track Changes on
/// (after a heading, inside a paragraph, at its start, and AutoCorrect leaves the editor's text
/// alone), and the editor's changes cover what they did: rejecting the splits joins the
/// paragraphs back. A paragraph they split off is theirs to restyle; restyling or reformatting
/// what the editor did, or a crafted split whose new paragraph isn't a copy, is refused.
#[test]
fn a_suggester_splits_paragraphs_someone_else_restyled() {
    let room = Room::new(Document::from_text("Title\nbody text"));
    let mut ed = join(&room, "Ed");
    run(&mut ed, "review.trackChanges", json!({"value": true}));
    run(&mut ed, "select.text", json!({"text": "Title"}));
    run(&mut ed, "para.heading1", json!({}));
    run(&mut ed, "select.text", json!({"text": "body"}));
    run(&mut ed, "para.alignCenter", json!({}));
    run(&mut ed, "format.bold", json!({}));
    run(&mut ed, "review.trackChanges", json!({"value": false}));
    let sam = join_as(&room, "Sam", Role::Suggester);
    let mut copies = [ed, sam];
    settle(&mut copies).unwrap();
    let [_, sam] = &mut copies;
    run(sam, "select.text", json!({"text": "Title"}));
    run(sam, "caret.right", json!({}));
    run(sam, "text.newParagraph", json!({}));
    run(sam, "text.insert", json!({"text": "Intro"}));
    // Enter after "body" would capitalise it (it starts a sentence), untracked: it stays.
    run(sam, "select.text", json!({"text": "body"}));
    run(sam, "caret.right", json!({}));
    run(sam, "text.newParagraph", json!({}));
    run(sam, "select.text", json!({"text": "body"}));
    run(sam, "caret.left", json!({}));
    run(sam, "text.newParagraph", json!({}));
    // The paragraph they split off is theirs to restyle (as a tracked change); the ones Ed
    // restyled aren't.
    run(sam, "select.text", json!({"text": " text"}));
    run(sam, "para.align", json!({"value": "right"}));
    run(sam, "select.text", json!({"text": "body"}));
    refused(sam, "para.align", json!({"value": "right"}));
    run(sam, "select.text", json!({"text": "Title"}));
    refused(sam, "para.heading2", json!({}));
    run(sam, "select.text", json!({"text": "body"}));
    refused(sam, "format.italic", json!({}));
    settle(&mut copies).unwrap();
    same_everywhere(&copies).unwrap();
    smooth(&copies);
    let doc = room.document().1;
    let kinds: std::collections::BTreeSet<_> = doc.revisions.iter().map(|r| (r.author.as_str(), format!("{:?}", r.kind))).collect();
    assert_eq!(kinds, [("Ed", "Format".into()), ("Sam", "Format".into()), ("Sam", "Insert".into())].into_iter().collect());
    let resolve = |id: &str| {
        let mut s = Session::new(doc.clone());
        s.run(id, &json!({})).unwrap();
        let paras = s.doc.body.iter().filter_map(|b| b.as_para());
        paras.map(|p| (p.text.clone(), p.props.style.clone(), p.props.align, p.props_at(0).bold)).collect::<Vec<_>>()
    };
    let (normal, center) = (Some("Normal".to_string()), Some(wordcraft_doc::props::Align::Center));
    let para = |t: &str, style: Option<String>, align, bold| (t.to_string(), style, align, bold);
    assert_eq!(resolve("review.rejectAll"), [para("Title", None, None, None), para("body text", None, None, None)]);
    assert_eq!(
        resolve("review.acceptAll"),
        [
            para("Title", Some("Heading1".into()), None, None),
            para("Intro", normal, None, None),
            para("", None, center, None),
            para("body", None, center, Some(true)),
            para(" text", None, Some(wordcraft_doc::props::Align::Right), None),
        ]
    );
    // At the hub: a split whose new paragraph keeps Ed's change but isn't a copy of the one it
    // was split from is refused; the split Enter makes goes through.
    let mut hub = Hub::new(copies[1].session().doc.clone());
    let sam = hub.admit("Sam", Role::Suggester).unwrap().id;
    let base = hub.document().clone();
    let split = |crafted: bool| {
        change_by(&base, "Sam", true, |s| {
            s.run("select.text", &json!({"text": "bod"})).unwrap();
            s.run("caret.right", &json!({})).unwrap();
            s.run("text.newParagraph", &json!({})).unwrap();
            if crafted {
                let path = s.sel.focus.path.clone();
                let p = s.doc.para_mut(StoryRef::Body, &path).unwrap();
                p.props.align = Some(wordcraft_doc::props::Align::Right);
                p.touch();
            }
        })
    };
    let took = hub.submit(0, &split(true), sam);
    assert!(matches!(&took, Err(Bounce::Refused(why)) if why.contains("leaves Ed's tracked changes")), "{took:?}");
    assert_eq!(hub.submit(0, &split(false), sam).map(|t| t.version), Ok(1));
}

#[test]
fn the_application_s_settings_are_the_owner_s() {
    let room = Room::new(Document::from_text("Hello"));
    let mut ed = join(&room, "Ed");
    let mut olu = join_as(&room, "Olu", Role::Owner);
    refused(&mut ed, "tools.autocorrect", json!({"capSentences": false}));
    assert!(ed.session().prefs.autocorrect.cap_sentences);
    run(&mut olu, "tools.autocorrect", json!({"capSentences": false}));
    assert!(!olu.session().prefs.autocorrect.cap_sentences);
    assert!(!olu.has_unconfirmed());
}

#[test]
fn a_session_with_a_role_holds_its_commands_to_it() {
    let mut s = Session::new(Document::from_text("Hello world"));
    s.author = "Sam".into();
    s.role = Some(Role::Suggester);
    s.run("select.text", &json!({"text": "world"})).unwrap();
    s.run("format.italic", &json!({})).unwrap();
    s.run("text.insert", &json!({"text": "you"})).unwrap();
    assert!(!s.doc.settings.track_changes);
    assert!(s.doc.revisions.iter().all(|r| r.author == "Sam"));
    assert_eq!(resolved(&s.doc, false), "Hello world");
    // Undo and redo as for anyone.
    s.run("edit.undo", &json!({})).unwrap();
    s.run("edit.redo", &json!({})).unwrap();
    assert_eq!(resolved(&s.doc, true), "Hello you");
    // A role below editor can't change the document's settings.
    assert!(matches!(s.run("design.pageColor", &json!({"color": "FF0000"})), Err(CmdError::Disabled(_))));
    assert_eq!(s.doc.settings.page_color, None);
    s.role = Some(Role::Viewer);
    assert!(matches!(s.run("text.insert", &json!({"text": "x"})), Err(CmdError::Disabled(_))));
    s.role = Some(Role::Editor);
    s.run("review.acceptAll", &json!({})).unwrap();
    assert!(s.doc.revisions.is_empty());
}

/// An edit for the comparison below: by Sam, the participant checked, or by Ed; with Track
/// Changes on or off.
fn checked_step() -> impl Strategy<Value = (bool, bool, Step)> {
    let review = [
        ("review.accept", json!({})),
        ("review.reject", json!({})),
        ("review.acceptAll", json!({})),
        ("review.rejectAll", json!({})),
        ("review.resolveComment", json!({})),
        ("review.deleteComment", json!({})),
        ("review.editComment", json!({"id": 1, "text": "changed"})),
        ("caret.paraDown", json!({})),
        ("caret.paraUp", json!({"extend": true})),
    ];
    let edit = prop_oneof![
        3 => edit_step(),
        3 => step(),
        2 => proptest::sample::select(review.to_vec()).prop_map(|(id, v)| Step::Command(id, v)),
    ];
    (proptest::bool::weighted(0.7), proptest::bool::weighted(0.7), edit)
}

/// The check of a participant's change compares only what the change touched: for random
/// edits by them and by others (typing, deleting, formatting, paragraphs, tables, notes,
/// comments, text boxes, accepting and rejecting, undo and redo; with and without Track
/// Changes; 300 cases, the same each run), it finds what the check of the whole documents
/// finds, for every role.
#[test]
fn a_role_check_of_what_a_change_touched_finds_what_one_of_the_whole_document_does() {
    const CASES: usize = 300;
    let mut runner = TestRunner::deterministic();
    let steps = proptest::collection::vec(checked_step(), 1..12);
    let (mut allowed, mut refused) = ([0usize; 3], [0usize; 3]);
    for case in 0..CASES {
        let Ok(tree) = steps.new_tree(&mut runner) else { continue };
        let mut s = Session::new(sample_document());
        for (k, (sam, track, st)) in tree.current().into_iter().enumerate() {
            s.author = if sam { "Sam" } else { "Ed" }.into();
            s.doc.settings.track_changes = track;
            let before = s.doc.clone();
            run_step(&mut s, &st);
            for (r, role) in [Role::Viewer, Role::Commenter, Role::Suggester].into_iter().enumerate() {
                let (part, whole) = (rules::check(role, "Sam", &before, &s.doc), rules::check_whole(role, "Sam", &before, &s.doc));
                assert_eq!(part.is_ok(), whole.is_ok(), "case {case}, step {k} ({st:?} by {}): {role} {part:?} / {whole:?}", s.author);
                if before != s.doc {
                    if part.is_ok() {
                        allowed[r] += 1;
                    } else {
                        refused[r] += 1;
                    }
                }
            }
        }
    }
    // Every role had changes both ways (a viewer's are all refused).
    assert_eq!(allowed[0], 0);
    assert!(refused.iter().all(|n| *n > 50), "{refused:?}");
    assert!(allowed[1] >= 10 && allowed[2] >= 100, "{allowed:?}");
}

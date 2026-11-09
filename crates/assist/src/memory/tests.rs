//! The file: its limits, what a damaged one does, two writers, and the budget.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;

use super::{ASK, BUDGET, DAMAGED_SUFFIX, FILE, ForModel, MAX_ITEMS, MAX_TEXT, Memory, NO_LIST, PREFACE};

const NOW: &str = "2026-11-08T10:00:00Z";

/// A settings folder of its own.
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wordcraft-memory-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn add(dir: &std::path::Path, text: &str) -> String {
    Memory::update(dir, |m| m.add(text, NOW)).unwrap().id
}

#[test]
fn what_is_kept_is_there_after_a_restart() {
    let dir = folder("kept");
    let id = add(&dir, "Use numbered lists in summaries.");
    assert_eq!(id, "m1");
    let m = Memory::load(&dir);
    assert!(m.enabled && !m.damaged);
    assert_eq!(m.items[0].text, "Use numbered lists in summaries.");
    assert_eq!(m.items[0].kept, NOW);
    // Saying the same thing again keeps one; an id isn't used twice.
    assert_eq!(add(&dir, "  use NUMBERED   lists in summaries. "), "m1");
    assert_eq!(add(&dir, "Second."), "m2");
    assert_eq!(Memory::load(&dir).items.len(), 2);
}

#[cfg(unix)]
#[test]
fn only_its_owner_can_read_the_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = folder("mode");
    add(&dir, "Private.");
    let mode = std::fs::metadata(dir.join(FILE)).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn a_change_that_fails_writes_nothing() {
    let dir = folder("failed");
    assert!(Memory::update(&dir, |m| m.add(" \n\t\u{202E}", NOW)).is_err(), "nothing to keep");
    assert!(!dir.join(FILE).exists());
    add(&dir, "One.");
    assert!(Memory::update(&dir, |m| m.remove("m999")).is_err());
    assert!(Memory::update(&dir, |m| m.edit("m999", Some("x"), None)).is_err());
    assert_eq!(Memory::load(&dir).items.len(), 1);
}

#[test]
fn a_removed_text_counts_as_dismissed_and_clear_all_empties_both() {
    let dir = folder("remove");
    add(&dir, "Gone soon.");
    Memory::update(&dir, |m| m.remove("m1")).unwrap();
    let m = Memory::load(&dir);
    assert!(m.items.is_empty());
    assert_eq!(m.dismissed.len(), 1);
    assert!(!std::fs::read_to_string(dir.join(FILE)).unwrap().contains("Gone soon"), "the file keeps a hash, not the text");
    Memory::update(&dir, |m| {
        m.clear();
        Ok(())
    })
    .unwrap();
    assert!(Memory::load(&dir).dismissed.is_empty());
}

/// Each damaged file reads within the limits without a panic, and the next write keeps it aside.
#[test]
fn a_hostile_file_is_read_within_the_limits_and_kept_aside() {
    let many: Vec<_> = (0..100_000).map(|n| json!({"id": format!("m{n}"), "text": "x"})).collect();
    let few_of_many: Vec<_> = (0..500).map(|n| json!({"id": format!("m{n}"), "text": "x"})).collect();
    let cases: Vec<(&str, Vec<u8>, usize)> = vec![
        ("junk", b"{ not json \xff".to_vec(), 0),
        ("hundred thousand items", json!({"version": 1, "items": many}).to_string().into_bytes(), 0),
        ("over the item limit", json!({"version": 1, "items": few_of_many}).to_string().into_bytes(), MAX_ITEMS),
        ("ten megabytes of text", json!({"version": 1, "items": [{"id": "m1", "text": "x".repeat(10 << 20)}]}).to_string().into_bytes(), 0),
        ("a later version", json!({"version": 2, "items": [{"id": "m1", "text": "x"}]}).to_string().into_bytes(), 0),
        ("no version", json!({"items": [{"id": "m1", "text": "x"}]}).to_string().into_bytes(), 0),
        ("items that aren't a list", json!({"version": 1, "items": {"id": "m1", "text": "Use British English."}}).to_string().into_bytes(), 0),
        ("dismissed that isn't a list", json!({"version": 1, "items": [], "dismissed": "abc"}).to_string().into_bytes(), 0),
        ("wrong types", json!({"version": 1, "enabled": "yes", "items": [5, {"text": 5}], "dismissed": [1, "abc"]}).to_string().into_bytes(), 0),
    ];
    for (name, bytes, items) in cases {
        let dir = folder("hostile");
        std::fs::write(dir.join(FILE), &bytes).unwrap();
        let m = Memory::load(&dir);
        assert!(m.damaged, "{name}");
        assert_eq!(m.items.len(), items, "{name}");
        // Reading leaves it alone; a write moves it aside whole.
        assert_eq!(std::fs::read(dir.join(FILE)).unwrap(), bytes, "{name}");
        Memory::update(&dir, |m| {
            m.enabled = false;
            Ok(())
        })
        .unwrap();
        assert_eq!(std::fs::read(dir.join(format!("{FILE}{DAMAGED_SUFFIX}"))).unwrap(), bytes, "{name}");
        let after = Memory::load(&dir);
        assert!(!after.damaged && !after.enabled, "{name}");
        assert_eq!(after.items.len(), items, "{name}");
    }
}

#[test]
fn control_characters_and_long_texts_are_cleaned_when_read() {
    let dir = folder("clean");
    let text = format!("a\u{202E}b\nc\u{0}d{}", "y".repeat(1000));
    let file = json!({"version": 1, "items": [
        {"id": "m1", "text": text, "kept": "\u{202E}".repeat(100), "used": -1},
        {"id": "m1", "text": "The same id."},
        {"id": "../evil", "text": "A bad id."},
    ]});
    std::fs::write(dir.join(FILE), file.to_string()).unwrap();
    let m = Memory::load(&dir);
    assert!(m.damaged, "the next write would lose what was cut, so it moves the file aside first");
    assert!(m.items[0].text.starts_with("ab cd"), "{}", m.items[0].text);
    assert_eq!(m.items[0].text.chars().count(), MAX_TEXT);
    assert_eq!((m.items[0].kept.as_str(), m.items[0].used), ("", 0));
    let ids: Vec<&str> = m.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["m1", "m2", "m3"]);
}

/// An edit waiting in another window can't land on a different preference: an id isn't used again.
#[test]
fn an_id_is_not_used_again() {
    let dir = folder("ids");
    add(&dir, "One.");
    add(&dir, "Two.");
    Memory::update(&dir, |m| m.remove("m2")).unwrap();
    assert_eq!(add(&dir, "Three."), "m3", "after the highest was removed");
    Memory::update(&dir, |m| {
        m.clear();
        Ok(())
    })
    .unwrap();
    assert_eq!(add(&dir, "Four."), "m4", "after everything was cleared");
    assert!(!Memory::load(&dir).damaged);
}

/// Two windows, each saving in turn many times: neither loses the other's items.
#[test]
fn two_writers_keep_both_items() {
    let dir = Arc::new(folder("writers"));
    let threads: Vec<_> = (0..4)
        .map(|w| {
            let dir = dir.clone();
            std::thread::spawn(move || {
                for n in 0..25 {
                    // The lock waits a bounded time, as a window's click may: refused, it says to
                    // try again, as the person would.
                    let mut tries = 0;
                    while let Err(e) = Memory::update(&dir, |m| m.add(&format!("writer {w} item {n}"), NOW)) {
                        assert!(e.starts_with("another WordCraft") && tries < 200, "{e}");
                        tries += 1;
                    }
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let m = Memory::load(&dir);
    assert_eq!(m.items.len(), 100);
    let mut ids: Vec<&str> = m.items.iter().map(|i| i.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 100, "each with an id of its own");
}

fn memory(texts: &[&str]) -> Memory {
    let mut m = Memory::default();
    for t in texts {
        m.add(t, NOW).unwrap();
    }
    m
}

#[test]
fn off_or_disabled_sends_nothing_and_most_recently_used_goes_first() {
    let empty = Memory::default().for_model();
    assert_eq!(empty.text, Some(format!("{ASK}\n\n{NO_LIST}")), "on with nothing kept, it may still propose");
    let mut m = memory(&["One.", "Two.", "Three."]);
    m.edit("m2", None, Some(true)).unwrap();
    m.items[0].last_used = Some("2026-11-09T00:00:00Z".into());
    let sent = m.for_model();
    assert_eq!(sent.sent, ["m1", "m3"]);
    assert_eq!(sent.left_out, Vec::<String>::new());
    let text = sent.text.unwrap();
    assert!(text.starts_with(ASK) && text.contains(PREFACE), "{text}");
    assert!(text.ends_with(r#"[{"id":"m1","text":"One."},{"id":"m3","text":"Three."}]"#), "{text}");
    m.enabled = false;
    assert_eq!(m.for_model(), ForModel::default(), "off asks for nothing and sends nothing");
    m.enabled = true;
    for id in ["m1", "m3"] {
        m.edit(id, None, Some(true)).unwrap();
    }
    assert_eq!(m.for_model().sent, Vec::<String>::new(), "every item off");
    assert!(m.for_model().text.unwrap().ends_with(NO_LIST));
}

#[test]
fn a_proposal_of_an_item_or_a_dismissed_text_is_known() {
    let mut m = memory(&["Use numbered lists."]);
    m.dismiss("Never use  Oxford commas");
    assert!(m.known("  use NUMBERED lists. "));
    assert!(m.known("never use oxford commas"));
    assert!(!m.known("Something else."));
}

/// For any text, what is sent stays in the budget, escaped as it goes: quotes double in JSON.
#[test]
fn what_is_sent_stays_within_the_budget() {
    for one in ["x", "\"", "\\", "é", "語"] {
        let mut m = Memory::default();
        for n in 0..MAX_ITEMS {
            m.add(&format!("{n} {}", one.repeat(MAX_TEXT)), NOW).unwrap();
        }
        let sent = m.for_model();
        let text = sent.text.unwrap();
        assert!(text.len() <= BUDGET, "{one}: {}", text.len());
        assert!(!sent.left_out.is_empty(), "{one}: the rest didn't fit");
        assert_eq!(sent.sent.len() + sent.left_out.len(), MAX_ITEMS, "{one}");
        // What is listed is what went: the list is valid JSON naming exactly those ids.
        let list = text.lines().last().unwrap();
        let v: serde_json::Value = serde_json::from_str(list).unwrap();
        let ids: Vec<&str> = v.as_array().unwrap().iter().map(|i| i["id"].as_str().unwrap()).collect();
        assert_eq!(ids, sent.sent.iter().map(String::as_str).collect::<Vec<_>>(), "{one}");
    }
}

/// A big item that doesn't fit leaves room for a small one after it.
#[test]
fn a_big_item_doesnt_stop_a_small_one() {
    let mut m = Memory::default();
    for n in 0..30 {
        m.add(&format!("{n} {}", "\"".repeat(MAX_TEXT)), NOW).unwrap();
    }
    m.add("Tiny.", NOW).unwrap();
    m.items.last_mut().unwrap().kept = "2020-01-01T00:00:00Z".into();
    let sent = m.for_model();
    assert!(sent.sent.last().is_some_and(|id| m.items.last().is_some_and(|i| i.id == *id)), "{:?}", sent.sent);
}

fn item_with(accepted: u64, rejected: u64) -> super::Item {
    let mut m = Memory::default();
    let mut item = m.add("Use numbered lists.", NOW).unwrap();
    item.accepted = accepted;
    item.rejected = rejected;
    item
}

#[test]
fn it_asks_at_three_rejections_more_than_accepted() {
    assert!(!item_with(0, 2).asks(), "rejected 2");
    assert!(!item_with(3, 3).asks(), "rejected 3, accepted 3");
    assert!(item_with(2, 3).asks(), "rejected 3, accepted 2");
    assert!(item_with(0, 3).asks(), "rejected 3, accepted 0");
    assert!(!item_with(9, 4).asks(), "accepted more");
}

#[test]
fn one_that_is_off_asks_nothing() {
    let mut item = item_with(0, 3);
    item.off = true;
    assert!(!item.asks());
}

#[test]
fn what_a_marker_names_counts_and_comes_first_next_turn() {
    let mut m = memory(&["First.", "Second."]);
    m.items[0].kept = "2026-01-01T00:00:00Z".into();
    m.items[1].kept = "2026-01-02T00:00:00Z".into();
    assert_eq!(m.for_model().sent, ["m2", "m1"], "the newest kept goes first");
    m.note_used(&["m1".into(), "m999".into()], NOW);
    assert_eq!((m.items[0].used, m.items[0].last_used.as_deref()), (1, Some(NOW)));
    assert_eq!((m.items[1].used, m.items[1].last_used.as_deref()), (0, None), "only what was named");
    assert_eq!(m.for_model().sent, ["m1", "m2"], "what was used last goes first");
}

#[test]
fn decisions_add_up_without_overflowing() {
    use crate::feedback::Decision;
    let mut m = memory(&["First."]);
    let first = m.preferences(&["m1".into()]).remove(0);
    m.note_decided(&first, Decision::Accepted, 2);
    m.note_decided(&first, Decision::Rejected, 3);
    m.note_decided(&super::SentPreference { id: "m999".into(), text: "First.".into() }, Decision::Rejected, 3);
    assert_eq!((m.items[0].accepted, m.items[0].rejected), (2, 3));
    m.note_decided(&first, Decision::Rejected, u64::MAX);
    assert_eq!(m.items[0].rejected, u64::MAX);
}

#[test]
fn a_change_made_under_an_old_wording_doesn_t_count_for_a_new_one() {
    use crate::feedback::Decision;
    let mut m = memory(&["First."]);
    let sent = m.preferences(&["m1".into()]).remove(0);
    m.edit("m1", Some("  FIRST. "), None).unwrap();
    m.note_decided(&sent, Decision::Rejected, 1);
    assert_eq!(m.items[0].rejected, 1, "the same wording");
    m.edit("m1", Some("Another."), None).unwrap();
    m.note_decided(&sent, Decision::Rejected, 1);
    assert_eq!(m.items[0].rejected, 0);
}

#[test]
fn keeping_what_it_asked_about_starts_the_counts_again() {
    let dir = folder("keep-asked");
    let id = add(&dir, "Use numbered lists.");
    Memory::update(&dir, |m| {
        m.items[0].used = 5;
        m.items[0].accepted = 1;
        m.items[0].rejected = 3;
        Ok(())
    })
    .unwrap();
    assert!(Memory::load(&dir).items[0].asks());
    Memory::update(&dir, |m| m.keep_asked(&id)).unwrap();
    let item = &Memory::load(&dir).items[0];
    assert!(!item.asks());
    assert_eq!((item.used, item.accepted, item.rejected), (5, 0, 0), "what it was used for stays");
    assert!(Memory::update(&dir, |m| m.keep_asked("m999")).is_err());
}

#[test]
fn a_new_wording_starts_the_counts_again_and_the_same_one_doesn_t() {
    let mut m = memory(&["First."]);
    m.items[0].rejected = 3;
    m.edit("m1", Some("  FIRST. "), None).unwrap();
    assert_eq!(m.items[0].rejected, 3);
    m.edit("m1", Some("Another."), None).unwrap();
    assert_eq!(m.items[0].rejected, 0);
}

#[test]
fn counting_leaves_a_damaged_file_as_it_is() {
    let dir = folder("count-damaged");
    let junk = b"{ not json";
    std::fs::write(dir.join(FILE), junk).unwrap();
    Memory::count(&dir, |m| m.note_used(&["m1".into()], NOW)).unwrap();
    assert_eq!(std::fs::read(dir.join(FILE)).unwrap(), junk);
    assert!(!dir.join(format!("{FILE}{DAMAGED_SUFFIX}")).exists());
}

#[test]
fn counting_keeps_what_another_window_wrote() {
    let dir = folder("count-writers");
    add(&dir, "First.");
    let id = add(&dir, "Second.");
    Memory::count(&dir, |m| m.note_used(std::slice::from_ref(&id), NOW)).unwrap();
    let m = Memory::load(&dir);
    assert_eq!(m.items.len(), 2);
    assert_eq!((m.items[0].used, m.items[1].used), (0, 1));
}

#[test]
fn the_model_is_asked_to_say_when_one_conflicts_and_to_mark_what_it_used() {
    let text = memory(&["First."]).for_model().text.unwrap();
    assert!(text.contains("conflicts with the current request") && text.contains("say so"), "{text}");
    assert!(text.contains("[[used:"), "{text}");
}

#[test]
fn what_was_sent_reads_by_id_in_the_order_sent() {
    let m = memory(&["First.", "Second."]);
    let sent = m.preferences(&["m2".into(), "m999".into(), "m1".into()]);
    assert_eq!(sent.iter().map(|p| (p.id.as_str(), p.text.as_str())).collect::<Vec<_>>(), [("m2", "Second."), ("m1", "First.")]);
}

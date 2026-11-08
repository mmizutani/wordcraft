//! The file: its limits, what a damaged one does, two writers, and the budget.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;

use super::{BUDGET, DAMAGED_SUFFIX, FILE, ForModel, MAX_ITEMS, MAX_TEXT, Memory, NONE_APPLY, PREFACE};

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
    assert_eq!(Memory::default().for_model(), ForModel::default());
    let mut m = memory(&["One.", "Two.", "Three."]);
    m.edit("m2", None, Some(true)).unwrap();
    m.items[0].last_used = Some("2026-11-09T00:00:00Z".into());
    let sent = m.for_model();
    assert_eq!(sent.sent, ["m1", "m3"]);
    assert_eq!(sent.left_out, Vec::<String>::new());
    let text = sent.text.unwrap();
    assert!(text.starts_with(PREFACE) && text.ends_with(r#"[{"id":"m1","text":"One."},{"id":"m3","text":"Three."}]"#), "{text}");
    m.enabled = false;
    assert_eq!(m.for_model(), ForModel::default());
    m.enabled = true;
    for id in ["m1", "m3"] {
        m.edit(id, None, Some(true)).unwrap();
    }
    assert_eq!(m.for_model(), ForModel::default(), "every item off");
    assert!(!NONE_APPLY.is_empty());
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
        let list = text.lines().nth(1).unwrap();
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

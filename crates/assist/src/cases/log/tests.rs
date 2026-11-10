//! One conversation's record: the writer thread, the queue, the caps, and a reader that trusts
//! nothing in the file.

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};

use super::{CaseLog, Decision, LogSearch, MAX_CASE_BYTES, MAX_FILE_BYTES, Record, delete_log, log_size, search_log};
use crate::cases::record::{Click, Quoted};
use crate::cases::store::{CaseRef, Shelf};

const TODAY: &str = "2026-11-10";

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wordcraft-case-log-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn shelf_with_case(name: &str) -> (Shelf, CaseRef) {
    let click = Click::by_person(true).unwrap();
    let shelf = Shelf::at(&folder(name));
    let wall = shelf.new_wall(click, "Kestrel Freight KK", TODAY, 1).unwrap();
    let at = shelf.new_case(click, &wall, "Kestrel MSA", TODAY, 2).unwrap();
    (shelf, at)
}

fn decided(text: &str) -> Record {
    Record::Decided {
        at: TODAY.into(),
        decision: Decision::Rejected,
        kind: "insert".into(),
        author: Quoted::parse("Bluefin Systems Ltd"),
        text: Quoted::parse(text),
    }
}

fn lines(shelf: &Shelf, at: &CaseRef) -> Vec<String> {
    let dir = shelf.log_dir(at);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    files.iter().flat_map(|f| std::fs::read_to_string(f).unwrap().lines().map(str::to_string).collect::<Vec<_>>()).collect()
}

fn search(shelf: &Shelf, at: &CaseRef, query: &str) -> LogSearch {
    search_log(shelf, at, query, 100).unwrap()
}

#[test]
fn a_record_is_one_line_and_search_finds_it() {
    let (shelf, at) = shelf_with_case("one-line");
    let mut log = CaseLog::open(&shelf, &at, "20261110T090000-1").unwrap();
    log.write(decided("Supplier may raise fees\nat any time"));
    log.close();
    let lines = lines(&shelf, &at);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let back: Record = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(back, decided("Supplier may raise fees\nat any time"));
    let found = search(&shelf, &at, "raise FEES");
    assert_eq!(found.hits.len(), 1);
    assert_eq!(found.skipped, 0);
    assert!(search(&shelf, &at, "indemnity").hits.is_empty());
}

/// A crash truncates at most the last line: the reader skips it and counts it, and keeps the rest.
#[test]
fn a_truncated_last_line_is_skipped_and_counted() {
    let (shelf, at) = shelf_with_case("truncated");
    let mut log = CaseLog::open(&shelf, &at, "20261110T090000-1").unwrap();
    log.write(decided("first"));
    log.write(decided("second"));
    log.close();
    let file = std::fs::read_dir(shelf.log_dir(&at)).unwrap().next().unwrap().unwrap().path();
    let text = std::fs::read_to_string(&file).unwrap();
    let cut = text.len() - 7;
    std::fs::write(&file, &text.as_bytes()[..cut]).unwrap();
    let found = search(&shelf, &at, "");
    assert_eq!(found.hits.len(), 1, "{found:?}");
    assert_eq!(found.skipped, 1);
    assert!(matches!(&found.hits[0].record, Record::Decided { text, .. } if text.for_person() == "first"));
    let found = search(&shelf, &at, "second");
    assert!(found.hits.is_empty());
    assert_eq!(found.skipped, 1, "a line that can't be read is counted on every search");
}

/// The writer stuck on the disk, the queue fills: every write still returns at once, and what
/// didn't fit becomes one `Dropped` record once there is room.
#[test]
fn a_full_queue_never_blocks_and_becomes_a_dropped_record() {
    let (shelf, at) = shelf_with_case("full-queue");
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let mut log = CaseLog::open_gated(&shelf, &at, "20261110T090000-1", 2, gate.clone()).unwrap();
    let started = std::time::Instant::now();
    for n in 0..8 {
        log.write(decided(&format!("change {n}")));
    }
    // A write that waited for the writer would wait for the gate, which stays shut: forever.
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "a write waited for the writer");
    // The writer took one record off the queue before the gate, so two of the eight waited there.
    assert!(log.dropped() >= 4, "{}", log.dropped());
    {
        let (open, wake) = &*gate;
        *open.lock().unwrap() = true;
        wake.notify_all();
    }
    // Writes go on while the writer drains; the count of what was dropped rides in front of the
    // first record that fits.
    let mut attempts = 0usize;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        log.write(decided("after"));
        attempts += 1;
        if log.dropped() == 0 {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "the writer never drained");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    log.close();
    let records: Vec<Record> = lines(&shelf, &at).iter().map(|l| serde_json::from_str(l).unwrap()).collect();
    let dropped: u32 = records.iter().filter_map(|r| if let Record::Dropped { n } = r { Some(*n) } else { None }).sum();
    let kept = records.iter().filter(|r| matches!(r, Record::Decided { .. })).count();
    assert!(dropped >= 4, "{records:?}");
    assert_eq!(dropped as usize + kept, 8 + attempts, "every record is written or counted: {records:?}");
    assert!(matches!(records.last(), Some(Record::Decided { text, .. }) if text.for_person() == "after"), "{records:?}");
}

/// The log dropped on the frame with the queue full (the record turned off, the window closing):
/// the writer still ends the file with a `Dropped` counting every record that never fit.
#[test]
fn a_dropped_log_still_counts_what_the_queue_had_no_room_for() {
    let (shelf, at) = shelf_with_case("dropped-log");
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let mut log = CaseLog::open_gated(&shelf, &at, "20261110T090000-1", 2, gate.clone()).unwrap();
    for n in 0..8 {
        log.write(decided(&format!("change {n}")));
    }
    assert!(log.dropped() >= 4, "{}", log.dropped());
    drop(log);
    {
        let (open, wake) = &*gate;
        *open.lock().unwrap() = true;
        wake.notify_all();
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let records = loop {
        let records: Vec<Record> = lines(&shelf, &at).iter().filter_map(|l| serde_json::from_str(l).ok()).collect();
        if matches!(records.last(), Some(Record::Dropped { .. })) {
            break records;
        }
        assert!(std::time::Instant::now() < deadline, "the writer never wrote the count: {records:?}");
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    let dropped: u32 = records.iter().filter_map(|r| if let Record::Dropped { n } = r { Some(*n) } else { None }).sum();
    let kept = records.iter().filter(|r| matches!(r, Record::Decided { .. })).count();
    assert_eq!(dropped as usize + kept, 8, "every record is written or counted: {records:?}");
}

/// A `Redlines` snapshot whose texts are each within their cap can still pass the reader's line
/// cap together: the writer drops its last comments and changes into `more` until the line
/// fits, so the search reads it.
#[test]
fn a_snapshot_too_long_for_a_line_is_shrunk_into_more() {
    let (shelf, at) = shelf_with_case("long-snapshot");
    let wide = "𝒜".repeat(2048);
    let comments: Vec<super::CommentLine> = (0..64)
        .map(|_| super::CommentLine { author: Quoted::parse(&wide), date: TODAY.into(), text: Quoted::parse(&wide), at: None, resolved: false })
        .collect();
    let changes: Vec<super::RedlineLine> = (0..4)
        .map(|n| super::RedlineLine {
            kind: "insert".into(),
            author: Quoted::parse("Bluefin Systems Ltd"),
            date: TODAY.into(),
            text: Quoted::parse(&format!("change {n}")),
            at: None,
        })
        .collect();
    let snapshot = Record::Redlines { at: TODAY.into(), draft: Quoted::parse("Kestrel MSA"), changes, comments, more: 3 };
    assert!(serde_json::to_vec(&snapshot).unwrap().len() > super::MAX_LINE_BYTES, "the fixture is within every text cap yet over the line cap");
    let mut log = CaseLog::open(&shelf, &at, "20261110T090000-1").unwrap();
    log.write(snapshot);
    log.write(decided("after"));
    log.close();
    let lines = lines(&shelf, &at);
    assert_eq!(lines.len(), 2, "{}", lines.len());
    assert!(lines[0].len() <= super::MAX_LINE_BYTES, "{}", lines[0].len());
    let found = search(&shelf, &at, "");
    assert_eq!(found.skipped, 0, "{found:?}");
    assert_eq!(found.hits.len(), 2);
    let Record::Redlines { changes, comments, more, .. } = &found.hits[0].record else { panic!("{:?}", found.hits[0]) };
    assert_eq!(changes.len(), 4, "the changes stay while the comments make room");
    assert!(comments.len() < 64 && !comments.is_empty(), "{}", comments.len());
    assert_eq!(*more as usize, 3 + (64 - comments.len()), "what went is counted");
}

/// A file past its cap takes no more records and counts them; a case past its cap loses its
/// oldest conversation's file when the next one opens.
#[test]
fn the_caps_hold_and_the_oldest_conversation_goes_first() {
    let (shelf, at) = shelf_with_case("caps");
    let big = "x".repeat(60 << 10);
    // A queue nothing drops from, so every count here is the file cap's: a loaded machine
    // schedules the writer late.
    let open_gate = Arc::new((Mutex::new(true), Condvar::new()));
    let mut log = CaseLog::open_gated(&shelf, &at, "20261110T090000-1", 1 << 16, open_gate).unwrap();
    let mut clamped = decided(&big);
    clamped.clamp();
    let per_line = serde_json::to_string(&clamped).unwrap().len() + 1;
    let fits = MAX_FILE_BYTES as usize / per_line;
    for _ in 0..fits + 3 {
        log.write(decided(&big));
    }
    log.close();
    let size = log_size(&shelf, &at);
    assert!(size <= MAX_FILE_BYTES + 1024, "{size}");
    let records: Vec<Record> = lines(&shelf, &at).iter().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(records.iter().filter(|r| matches!(r, Record::Decided { .. })).count(), fits);
    assert!(records.iter().any(|r| matches!(r, Record::Dropped { n: 3 })), "{:?}", records.last());

    // Older files (by modification time) bring the case past its cap: opening a new conversation
    // removes them oldest first until the new file has room.
    let dir = shelf.log_dir(&at);
    let filler = vec![b'{'; (MAX_FILE_BYTES / 2) as usize];
    let older = (MAX_CASE_BYTES / (MAX_FILE_BYTES / 2)) as usize;
    for n in 0..older {
        let path = dir.join(format!("20261101T0000{n:02}-9.jsonl"));
        std::fs::write(&path, &filler).unwrap();
        set_mtime(&path, (10_000 - n) as u64);
    }
    assert!(log_size(&shelf, &at) > MAX_CASE_BYTES);
    let log = CaseLog::open(&shelf, &at, "20261111T090000-2").unwrap();
    log.close();
    let left = log_size(&shelf, &at);
    assert!(left + MAX_FILE_BYTES <= MAX_CASE_BYTES, "{left}");
    assert!(dir.join("20261110T090000-1.jsonl").exists(), "the newest full file stays");
    assert!(!dir.join("20261101T000000-9.jsonl").exists(), "the oldest went first");
}

/// Set `path`'s modification time to `ago` seconds before now.
fn set_mtime(path: &std::path::Path, ago: u64) {
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(ago)).unwrap();
}

/// Which conversation is older is its file's modification time, not its name: a name starts
/// with the process id, so names order files by process before time across restarts. Retention
/// removes the file written least recently, and a search reads the newest first.
#[test]
fn files_are_ordered_by_modification_time_not_name() {
    let (shelf, at) = shelf_with_case("mtime-order");
    let dir = shelf.log_dir(&at);
    std::fs::create_dir_all(&dir).unwrap();
    // Each file is one record and then a line too long to read, so two of them pass the case cap.
    let filler = vec![b'{'; 25 << 20];
    for (name, text, ago) in [("a-newer.jsonl", "alpha", 10), ("z-older.jsonl", "zulu", 1000)] {
        let mut bytes = serde_json::to_vec(&decided(text)).unwrap();
        bytes.push(b'\n');
        bytes.extend_from_slice(&filler);
        std::fs::write(dir.join(name), bytes).unwrap();
        set_mtime(&dir.join(name), ago);
    }
    let found = search(&shelf, &at, "");
    let texts: Vec<&str> = found.hits.iter().map(|h| h.file.as_str()).collect();
    assert_eq!(texts, ["a-newer.jsonl", "z-older.jsonl"], "the newest file's hit comes first");
    assert!(log_size(&shelf, &at) + MAX_FILE_BYTES > MAX_CASE_BYTES, "the fixture is past the cap");
    CaseLog::open(&shelf, &at, "20261111T090000-2").unwrap().close();
    assert!(dir.join("a-newer.jsonl").exists(), "the file written last went");
    assert!(!dir.join("z-older.jsonl").exists(), "the file written least recently stayed");
}

/// Past `MAX_FILES` conversations, the oldest files go when the next opens, and every file left
/// is seen by the size line and the search: none vanishes from a listing.
#[test]
fn more_files_than_the_cap_lose_the_oldest_and_none_is_hidden() {
    let (shelf, at) = shelf_with_case("file-count");
    let dir = shelf.log_dir(&at);
    std::fs::create_dir_all(&dir).unwrap();
    let extra = super::MAX_FILES + 3;
    let mut bytes = 0u64;
    for n in 0..extra {
        let path = dir.join(format!("c-{n:04}.jsonl"));
        let mut line = serde_json::to_vec(&decided(&format!("file {n}"))).unwrap();
        line.push(b'\n');
        bytes += line.len() as u64;
        std::fs::write(&path, line).unwrap();
        set_mtime(&path, (extra - n) as u64 * 10);
    }
    assert_eq!(log_size(&shelf, &at), bytes, "every file counts before the cap holds");
    let log = CaseLog::open(&shelf, &at, "20261111T090000-2").unwrap();
    log.close();
    let left: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert!(left.len() <= super::MAX_FILES, "{} files are left", left.len());
    assert_eq!(left.len(), super::MAX_FILES);
    for n in 0..4 {
        assert!(!left.contains(&format!("c-{n:04}.jsonl")), "c-{n:04} is the oldest and stayed");
    }
    assert!(left.contains(&format!("c-{:04}.jsonl", extra - 1)));
    let on_disk: u64 = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().metadata().unwrap().len()).sum();
    assert_eq!(log_size(&shelf, &at), on_disk, "the size line sees every file left");
    let found = search_log(&shelf, &at, "file", usize::MAX).unwrap();
    assert_eq!(found.hits.len(), super::MAX_FILES - 1, "the search sees every record left: {}", found.hits.len());
    assert_eq!(found.skipped, 0);
}

#[test]
fn delete_removes_the_record_and_a_missing_one_is_nothing_to_do() {
    let (shelf, at) = shelf_with_case("delete");
    let mut log = CaseLog::open(&shelf, &at, "20261110T090000-1").unwrap();
    log.write(decided("gone"));
    log.close();
    assert!(log_size(&shelf, &at) > 0);
    delete_log(&shelf, &at).unwrap();
    assert!(!shelf.log_dir(&at).exists());
    assert_eq!(log_size(&shelf, &at), 0);
    delete_log(&shelf, &at).unwrap();
    assert!(shelf.exists(&at), "the case stays");
}

/// A line of junk, a huge line, and a record this build doesn't know are skipped and counted,
/// never a panic.
#[test]
fn hostile_lines_are_skipped_and_counted() {
    let (shelf, at) = shelf_with_case("hostile");
    let mut log = CaseLog::open(&shelf, &at, "20261110T090000-1").unwrap();
    log.write(decided("kept"));
    log.close();
    let file = std::fs::read_dir(shelf.log_dir(&at)).unwrap().next().unwrap().unwrap().path();
    let mut text = std::fs::read_to_string(&file).unwrap();
    text.push_str("not json\n");
    // A whole `Turn`, valid but for its length: only the line guard can refuse it.
    let long = Record::Turn {
        at: TODAY.into(),
        n: 2,
        asked: Quoted::parse(&"y".repeat(super::MAX_LINE_BYTES)),
        answer: None,
        tools: Vec::new(),
        model: None,
        stop: None,
        recalled: Vec::new(),
        used: Vec::new(),
    };
    let long = serde_json::to_string(&long).unwrap();
    assert!(long.len() > super::MAX_LINE_BYTES);
    assert!(serde_json::from_str::<Record>(&long).is_ok(), "the fixture parses");
    text.push_str(&long);
    text.push('\n');
    text.push_str("{\"t\":\"unknownKind\"}\n");
    text.push_str("[1,2,3]\n");
    std::fs::write(&file, text).unwrap();
    let found = search(&shelf, &at, "");
    assert_eq!(found.hits.len(), 1);
    assert_eq!(found.skipped, 4);
    assert_eq!(search_log(&shelf, &at, "kept", 0).unwrap().hits.len(), 0, "a limit of none");
}

/// A link where `log/` should be, or a `.jsonl` link inside it, points at another client's files:
/// nothing is read through it, nothing is written through it, and nothing outside `log/` goes.
#[test]
#[cfg(unix)]
fn a_link_in_or_as_the_record_folder_is_never_followed() {
    let click = Click::by_person(true).unwrap();
    let shelf = Shelf::at(&folder("links"));
    let ours = shelf.new_wall(click, "Kestrel Freight KK", TODAY, 1).unwrap();
    let theirs = shelf.new_wall(click, "Bluefin Systems Ltd", TODAY, 2).unwrap();
    let here = shelf.new_case(click, &ours, "Kestrel MSA", TODAY, 3).unwrap();
    let other = shelf.new_case(click, &theirs, "Bluefin licence", TODAY, 4).unwrap();
    // The other client's case has a record of its own.
    let mut log = CaseLog::open(&shelf, &other, "20261110T090000-1").unwrap();
    log.write(decided("Canary: Bluefin's clause"));
    log.close();
    let other_dir = shelf.log_dir(&other);
    let other_file = std::fs::read_dir(&other_dir).unwrap().next().unwrap().unwrap().path();

    // `log/` is a link to the other client's record.
    std::os::unix::fs::symlink(&other_dir, shelf.log_dir(&here)).unwrap();
    assert!(CaseLog::open(&shelf, &here, "20261110T100000-2").is_err(), "opened through a link");
    assert!(search(&shelf, &here, "").hits.is_empty(), "read through a link");
    assert_eq!(log_size(&shelf, &here), 0);
    assert_eq!(std::fs::read_dir(&other_dir).unwrap().count(), 1, "the other client's record changed");
    std::fs::remove_file(shelf.log_dir(&here)).unwrap();

    // A `.jsonl` link inside a real `log/`, pointing at the other client's file, beside enough
    // filler that `make_room` would remove the oldest.
    let mut log = CaseLog::open(&shelf, &here, "20261110T100000-2").unwrap();
    log.write(decided("ours"));
    log.close();
    let dir = shelf.log_dir(&here);
    std::os::unix::fs::symlink(&other_file, dir.join("20261101T000000-0.jsonl")).unwrap();
    let filler = vec![b'{'; (MAX_FILE_BYTES / 2) as usize];
    for n in 0..(MAX_CASE_BYTES / (MAX_FILE_BYTES / 2)) as usize {
        std::fs::write(dir.join(format!("20261102T0000{n:02}-9.jsonl")), &filler).unwrap();
    }
    let found = search(&shelf, &here, "Canary");
    assert!(found.hits.is_empty(), "read the other client's file through a link: {found:?}");
    let log = CaseLog::open(&shelf, &here, "20261111T090000-3").unwrap();
    log.close();
    assert!(other_file.exists(), "make_room removed the link's target");
    assert!(dir.join("20261101T000000-0.jsonl").symlink_metadata().is_ok(), "the link itself is left, never followed");
    assert!(!dir.join("20261102T000000-9.jsonl").exists(), "the oldest plain file went");
}

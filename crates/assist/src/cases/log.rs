//! A case's record (#107): the transcript layer of its memory, kept only when the person turned
//! it on for that case. One conversation is one file, `log/<stamp>-<id>.jsonl` in the case's
//! folder, with one [`Record`] a line: a whole turn when it ends, the person's Accept or Reject of
//! anyone's change, the person's own edit of what the assistant wrote, and a snapshot of the other
//! side's redlines. Nothing in a turn reads it; the review pass does.
//!
//! The file has one writer, the conversation's own thread, fed by [`CaseLog::write`] through a
//! bounded queue with `try_send`, so a frame never waits for the disk and a full queue becomes a
//! [`Record::Dropped`] line. No lock: no two writers share a file. It doesn't fsync, so a power cut
//! can lose recent lines, and a crash truncates at most the last line, which the reader skips and
//! counts. `Record` is its own type and never the pane's [`crate::Entry`]: an entry holds internal
//! markers and is rewritten in place, and a record must read back the same in a later build.

use std::fs;
use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::feedback::Decision;

use super::record::{CaseError, Quoted};
use super::store::{CaseRef, Shelf};
use crate::backend::{StopReason, ToolStatus};

/// The folder inside a case's.
pub const FOLDER: &str = "log";

/// Most bytes one conversation's file takes; a record past it is counted, not written.
pub const MAX_FILE_BYTES: u64 = 16 << 20;

/// Most bytes a case's record takes in all. Opening a conversation past it removes the oldest
/// conversations' files first until a whole new file has room.
pub const MAX_CASE_BYTES: u64 = 64 << 20;

/// Most bytes of one line the reader takes; a longer one is skipped and counted.
pub const MAX_LINE_BYTES: usize = 1 << 20;

/// Most records waiting for the writer. A frame writes at most a handful; a writer stuck this far
/// behind is stuck on the disk.
const QUEUE: usize = 256;

/// Most files a search reads: a case has at most this many conversations' files before the oldest
/// go (far fewer at [`MAX_CASE_BYTES`] unless each is tiny).
const MAX_FILES: usize = 1024;

/// Most characters of an answer a turn's record keeps.
const MAX_ANSWER: usize = 64 << 10;

/// Most characters of any other text a record keeps.
const MAX_TEXT: usize = 2 << 10;

/// Most tool lines a turn's record keeps.
const MAX_TOOLS: usize = 64;

/// Most ids a turn's record keeps of what was recalled or used.
const MAX_IDS: usize = 64;

/// One line of the record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum Record {
    /// A whole turn, written when it ends: the answer with internal markers taken out.
    Turn {
        at: String,
        n: u32,
        asked: Quoted,
        answer: Option<Quoted>,
        tools: Vec<ToolLine>,
        model: Option<String>,
        stop: Option<StopReason>,
        /// The ids of what the turn recalled, and of what its answer marked as used.
        recalled: Vec<String>,
        used: Vec<String>,
    },
    /// The person's Accept or Reject of anyone's change: the only place this survives.
    Decided { at: String, decision: Decision, kind: String, author: Quoted, text: Quoted },
    /// The person changed what the assistant wrote.
    Edited { at: String, was: Quoted, now: Quoted },
    /// What the other side's draft held when the conversation first took it up. Scrubbed
    /// metadata shows as it is: an author of "Author" and no dates is itself worth knowing.
    Redlines { at: String, draft: Quoted, changes: Vec<RedlineLine>, comments: Vec<CommentLine>, more: u32 },
    /// Records that didn't fit the queue or the file.
    Dropped { n: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolLine {
    pub title: Quoted,
    pub status: ToolStatus,
    pub detail: Quoted,
}

/// One of the other side's tracked changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RedlineLine {
    /// `insert`, `delete`, `format`…, as the Reviewing Pane names it.
    pub kind: String,
    pub author: Quoted,
    pub date: String,
    pub text: Quoted,
    /// Where it starts, as the engine writes a position.
    pub at: Option<Value>,
}

/// One of the other side's comments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentLine {
    pub author: Quoted,
    pub date: String,
    pub text: Quoted,
    pub at: Option<Value>,
    pub resolved: bool,
}

impl Record {
    /// Hold every text to its cap, so a record's line is bounded whatever a turn produced.
    pub(crate) fn clamp(&mut self) {
        match self {
            Record::Turn { asked, answer, tools, recalled, used, .. } => {
                asked.cut(MAX_TEXT);
                if let Some(a) = answer {
                    a.cut(MAX_ANSWER);
                }
                tools.truncate(MAX_TOOLS);
                for t in tools {
                    t.title.cut(MAX_TEXT);
                    t.detail.cut(MAX_TEXT);
                }
                recalled.truncate(MAX_IDS);
                used.truncate(MAX_IDS);
            }
            Record::Decided { author, text, .. } => {
                author.cut(MAX_TEXT);
                text.cut(MAX_TEXT);
            }
            Record::Edited { was, now, .. } => {
                was.cut(MAX_TEXT);
                now.cut(MAX_TEXT);
            }
            Record::Redlines { draft, changes, comments, .. } => {
                draft.cut(MAX_TEXT);
                for c in changes {
                    c.author.cut(MAX_TEXT);
                    c.text.cut(MAX_TEXT);
                }
                for c in comments {
                    c.author.cut(MAX_TEXT);
                    c.text.cut(MAX_TEXT);
                }
            }
            Record::Dropped { .. } => {}
        }
    }

    /// The texts a search looks through: what people wrote, not field names.
    fn texts(&self) -> Vec<&str> {
        match self {
            Record::Turn { asked, answer, tools, .. } => {
                let mut out = vec![asked.for_person()];
                out.extend(answer.iter().map(Quoted::for_person));
                out.extend(tools.iter().flat_map(|t| [t.title.for_person(), t.detail.for_person()]));
                out
            }
            Record::Decided { author, text, .. } => vec![author.for_person(), text.for_person()],
            Record::Edited { was, now, .. } => vec![was.for_person(), now.for_person()],
            Record::Redlines { draft, changes, comments, .. } => {
                let mut out = vec![draft.for_person()];
                out.extend(changes.iter().flat_map(|c| [c.author.for_person(), c.text.for_person()]));
                out.extend(comments.iter().flat_map(|c| [c.author.for_person(), c.text.for_person()]));
                out
            }
            Record::Dropped { .. } => Vec::new(),
        }
    }

    /// Whether `query`, folded, is in any of its texts. An empty query matches every record.
    fn matches(&self, query: &str) -> bool {
        query.is_empty() || self.texts().iter().any(|t| t.to_lowercase().contains(query))
    }
}

/// The writer waits at this before each line: a test's stand-in for a slow disk.
type Gate = Arc<(Mutex<bool>, Condvar)>;

/// One conversation's record: its own file, its own writer thread. The window never waits.
pub struct CaseLog {
    tx: Option<SyncSender<Record>>,
    thread: Option<JoinHandle<()>>,
    /// Records the queue had no room for, not yet written as a [`Record::Dropped`].
    dropped: u32,
}

impl CaseLog {
    /// Open a new file for a conversation in `at`'s record, named from `stamp` (a time string;
    /// anything but letters, digits and hyphens is dropped). Removes the case's oldest files
    /// first when the record is past [`MAX_CASE_BYTES`]. [`CaseError::Gone`] when the case isn't
    /// there, and an error on the web, which has no thread.
    pub fn open(shelf: &Shelf, at: &CaseRef, stamp: &str) -> Result<CaseLog, CaseError> {
        CaseLog::open_with(shelf, at, stamp, QUEUE, None)
    }

    /// [`CaseLog::open`] with a queue of `queue` records and a writer that waits at `gate`
    /// before each line, so a test can fill the queue.
    #[doc(hidden)]
    pub fn open_gated(shelf: &Shelf, at: &CaseRef, stamp: &str, queue: usize, gate: Gate) -> Result<CaseLog, CaseError> {
        CaseLog::open_with(shelf, at, stamp, queue, Some(gate))
    }

    fn open_with(shelf: &Shelf, at: &CaseRef, stamp: &str, queue: usize, gate: Option<Gate>) -> Result<CaseLog, CaseError> {
        let dir = shelf.real_case_dir(at)?.join(FOLDER);
        super::store::make_dir(&dir, false)
            .or_else(|e| if e.kind() == std::io::ErrorKind::AlreadyExists { Ok(()) } else { Err(e) })
            .map_err(|e| CaseError::Io(format!("{} can't be made: {e}", dir.display())))?;
        // A link where `log/` should be could point into another client's case: the folder is
        // the wall, as `store` keeps it, so a link is refused and never written through.
        if !super::store::real_dir(&dir) {
            return Err(CaseError::Gone);
        }
        make_room(&dir);
        let stamp: String = stamp.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(64).collect();
        let stamp = if stamp.is_empty() { "conversation".to_string() } else { stamp };
        let mut file = None;
        for n in 0..8u32 {
            let name = if n == 0 { format!("{stamp}.jsonl") } else { format!("{stamp}-{n}.jsonl") };
            let path = dir.join(name);
            // `create_new` fails on a link as on a file (O_EXCL doesn't follow), so no line is
            // ever appended through one.
            let mut open = fs::OpenOptions::new();
            open.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
            match open.open(&path) {
                Ok(f) => {
                    file = Some(f);
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(CaseError::Io(format!("{} can't be made: {e}", path.display()))),
            }
        }
        let file = file.ok_or_else(|| CaseError::Io("the record's file can't be named".into()))?;
        let (tx, rx) = sync_channel::<Record>(queue.max(1));
        let thread = std::thread::Builder::new()
            .name("case record".into())
            .spawn(move || writer(file, rx, gate))
            .map_err(|e| CaseError::Io(format!("the record's writer can't start: {e}")))?;
        Ok(CaseLog { tx: Some(tx), thread: Some(thread), dropped: 0 })
    }

    /// Queue `record` for the writer. Never waits: a full queue counts it, and the count goes in
    /// front of the next record that fits.
    pub fn write(&mut self, mut record: Record) {
        record.clamp();
        let Some(tx) = &self.tx else { return };
        if self.dropped > 0 {
            match tx.try_send(Record::Dropped { n: self.dropped }) {
                Ok(()) => self.dropped = 0,
                Err(_) => {
                    self.dropped = self.dropped.saturating_add(1);
                    return;
                }
            }
        }
        match tx.try_send(record) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => self.dropped = self.dropped.saturating_add(1),
            // The writer is gone (its file failed): nothing more can be kept.
            Err(TrySendError::Disconnected(_)) => self.tx = None,
        }
    }

    /// Records the queue had no room for since the last `Dropped` line.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }

    /// Wait for the writer to write what is queued and close the file. Off the frame: a delete
    /// needs the file closed before the folder is renamed.
    pub fn close(mut self) {
        if let Some(tx) = self.tx.take() {
            if self.dropped > 0 {
                let _ = tx.send(Record::Dropped { n: self.dropped });
                self.dropped = 0;
            }
            drop(tx);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for CaseLog {
    /// Dropped on the frame, so it doesn't wait: the writer finishes what is queued on its own.
    fn drop(&mut self) {
        if let (Some(tx), true) = (&self.tx, self.dropped > 0) {
            let _ = tx.try_send(Record::Dropped { n: self.dropped });
        }
    }
}

/// The writer thread: one line a record, up to [`MAX_FILE_BYTES`]; what didn't fit is one
/// `Dropped` line at the end.
fn writer(mut file: fs::File, rx: std::sync::mpsc::Receiver<Record>, gate: Option<Gate>) {
    let mut written = 0u64;
    let mut over = 0u32;
    for record in rx {
        if let Some(gate) = &gate {
            let (open, wake) = &**gate;
            if let Ok(mut guard) = open.lock() {
                while !*guard {
                    guard = match wake.wait(guard) {
                        Ok(g) => g,
                        Err(_) => return,
                    };
                }
            }
        }
        let Ok(mut line) = serde_json::to_vec(&record) else { continue };
        line.push(b'\n');
        let len = u64::try_from(line.len()).unwrap_or(u64::MAX);
        if written.saturating_add(len) > MAX_FILE_BYTES {
            over = over.saturating_add(1);
            continue;
        }
        if file.write_all(&line).is_err() {
            return;
        }
        written = written.saturating_add(len);
    }
    if over > 0
        && let Ok(mut line) = serde_json::to_vec(&Record::Dropped { n: over })
    {
        line.push(b'\n');
        let _ = file.write_all(&line);
    }
}

/// The record's plain `.jsonl` files in `dir`, oldest first by name (the stamp is a time), each
/// with its size. `dir` must be a folder and not a link to one, and a link inside it is left out:
/// one could point at another client's file, and everything here reads or removes what it lists.
fn files_in(dir: &std::path::Path) -> Vec<(PathBuf, u64)> {
    if !super::store::real_dir(dir) {
        return Vec::new();
    }
    let mut out: Vec<(PathBuf, u64)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .take(MAX_FILES)
        .filter_map(|e| {
            if !e.file_type().is_ok_and(|t| t.is_file()) {
                return None;
            }
            let path = e.path();
            let meta = fs::symlink_metadata(&path).ok().filter(fs::Metadata::is_file)?;
            (path.extension().is_some_and(|x| x == "jsonl")).then_some((path, meta.len()))
        })
        .collect();
    out.sort();
    out
}

/// Remove the oldest files until a whole new file fits under [`MAX_CASE_BYTES`].
fn make_room(dir: &std::path::Path) {
    let files = files_in(dir);
    let mut total: u64 = files.iter().map(|(_, len)| *len).sum();
    for (path, len) in files {
        if total.saturating_add(MAX_FILE_BYTES) <= MAX_CASE_BYTES {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

/// Bytes the record of `at` takes on disk: a listing of its folder, a few stats.
pub fn log_size(shelf: &Shelf, at: &CaseRef) -> u64 {
    files_in(&shelf.log_dir(at)).iter().map(|(_, len)| *len).sum()
}

/// Remove the record of `at`, whole: the folder is renamed into the shelf's trash and removed, so
/// a crash between the two is finished by the next listing. A missing record is nothing to do.
pub fn delete_log(shelf: &Shelf, at: &CaseRef) -> Result<(), CaseError> {
    let dir = shelf.real_case_dir(at)?.join(FOLDER);
    if fs::symlink_metadata(&dir).is_err() {
        return Ok(());
    }
    shelf.trash(&dir, &format!("{}-log", at.case.as_str()))
}

/// One record a search found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogHit {
    /// The file's name: which conversation.
    pub file: String,
    pub line: u32,
    pub record: Record,
}

/// What a search found, and how many lines it couldn't read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogSearch {
    pub hits: Vec<LogHit>,
    pub skipped: u32,
}

/// The records of `at` whose texts hold `query`, folded, at most `limit` of them, newest file
/// first. A line that can't be read, is longer than [`MAX_LINE_BYTES`], or was cut by a crash is
/// skipped and counted. Reads every file: off the frame.
pub fn search_log(shelf: &Shelf, at: &CaseRef, query: &str, limit: usize) -> Result<LogSearch, CaseError> {
    let dir = shelf.real_case_dir(at)?.join(FOLDER);
    let query = query.trim().to_lowercase();
    let mut out = LogSearch::default();
    for (path, _) in files_in(&dir).into_iter().rev() {
        // Listed as a plain file a moment ago; a link put there since is refused here too.
        let Ok(file) = open_plain(&path) else {
            out.skipped = out.skipped.saturating_add(1);
            continue;
        };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut reader = std::io::BufReader::new(file);
        let mut line_no = 0u32;
        loop {
            line_no = line_no.saturating_add(1);
            let mut buf = Vec::new();
            let n = match (&mut reader).take(MAX_LINE_BYTES as u64 + 1).read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let ended = buf.last() == Some(&b'\n');
            if !ended {
                // Cut by a crash at the end, or longer than a line may be: skip to the next line.
                out.skipped = out.skipped.saturating_add(1);
                if n > MAX_LINE_BYTES && !skip_line(&mut reader) {
                    break;
                }
                if n <= MAX_LINE_BYTES {
                    break;
                }
                continue;
            }
            match serde_json::from_slice::<Record>(&buf) {
                Ok(record) if record.matches(&query) => {
                    if out.hits.len() < limit {
                        out.hits.push(LogHit { file: name.clone(), line: line_no, record });
                    }
                }
                Ok(_) => {}
                Err(_) => out.skipped = out.skipped.saturating_add(1),
            }
        }
    }
    Ok(out)
}

/// `path` open for reading, if it is a plain file and not a link: checked first, and on Unix the
/// open itself refuses a link too (`O_NOFOLLOW`), so nothing swapped in between is followed.
fn open_plain(path: &std::path::Path) -> std::io::Result<fs::File> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(std::io::Error::other("not a plain file"));
    }
    let mut open = fs::OpenOptions::new();
    open.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut open, rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    open.open(path)
}

/// Read past the rest of an over-long line; whether a line end was found.
fn skip_line(reader: &mut impl BufRead) -> bool {
    let mut scratch = Vec::new();
    loop {
        scratch.clear();
        match (&mut *reader).take(64 << 10).read_until(b'\n', &mut scratch) {
            Ok(0) | Err(_) => return false,
            Ok(_) if scratch.last() == Some(&b'\n') => return true,
            Ok(_) => {}
        }
    }
}

#[cfg(test)]
mod tests;

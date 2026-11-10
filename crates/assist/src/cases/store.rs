//! The shelf: the cases in a settings folder, one owner for every file under `assistant-cases/`.
//!
//! ```text
//! assistant-cases/
//!   .trash-<id>/            a delete in progress, swept at each listing
//!   <wall id>/              one client, or "My cases"
//!     wall.json             its name
//!     <case id>.lock        beside the folder, not in it, so a delete can rename the folder while holding it
//!     <case id>/case.json   the typed record, at most 1 MiB, mode 0600
//! ```
//!
//! A wall is a folder, so recall can open one wall and nothing else. There is no index, so a
//! delete can't leave a shadow in one. Every write locks, reads fresh, applies one typed op and
//! replaces the file in a rename, and never creates a folder: a case deleted in another window
//! returns [`CaseError::Gone`] and stays deleted. A symlink where a wall or case folder should be
//! is never followed: a listing skips it and a write finds it `Gone`; one where `case.json` should
//! be reads as damaged. So nothing here can touch, or recall from, a file outside the case's own
//! folder.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::record::{Applied, Case, CaseError, Click, Day, ModelWrite, PersonOp, Quoted, VERSION};
use crate::atomic::{self, Lock, LockError};

/// The folder, in the settings folder.
pub const FOLDER: &str = "assistant-cases";

/// Most bytes of `case.json`. A case holding the most of everything fits (a test builds one).
pub const MAX_FILE: u64 = 1 << 20;

/// What a file that can't be read whole is moved aside as.
pub const DAMAGED_SUFFIX: &str = ".damaged";

const CASE_FILE: &str = "case.json";
const WALL_FILE: &str = "wall.json";

/// How many times a case lock has slept waiting for another WordCraft, in this process. A window
/// test reads it twice around frames that must not wait.
#[doc(hidden)]
pub fn lock_sleeps() -> u64 {
    atomic::LOCK_SLEEPS.load(std::sync::atomic::Ordering::Relaxed)
}
const TRASH: &str = ".trash-";

/// Most times a new id is tried before giving up: a collision is one in 2^64.
const ID_TRIES: u32 = 8;

/// 16 hex digits, parsed at every boundary, so an id can't name a path outside the folder.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct WallId(String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CaseId(String);

fn is_id(text: &str) -> bool {
    text.len() == 16 && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

macro_rules! id {
    ($name:ident) => {
        impl $name {
            pub fn parse(text: &str) -> Option<$name> {
                is_id(text).then(|| $name(text.to_string()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = String;
            fn try_from(text: String) -> Result<$name, String> {
                $name::parse(&text).ok_or_else(|| "an id is 16 hex digits".to_string())
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> String {
                id.0
            }
        }
    };
}
id!(WallId);
id!(CaseId);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CaseRef {
    pub wall: WallId,
    pub case: CaseId,
}

/// A new id from a time string and a counter the caller passes in, so this module needs no
/// clock and no random numbers: the crate builds for the web. `create_dir` makes it unique.
fn id_from(stamp: &str, counter: u64, attempt: u32) -> String {
    let digest = Sha256::digest(format!("{stamp}\n{counter}\n{attempt}").as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// One wall as the pane lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wall {
    pub id: WallId,
    pub name: Quoted,
    pub cases: Vec<CaseHeader>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaseHeader {
    pub id: CaseId,
    pub title: Quoted,
    pub damaged: bool,
}

/// Walls and case headers, swept of what an interrupted delete left.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub walls: Vec<Wall>,
    /// Folders that weren't a wall or a case, left as they are.
    pub skipped: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WallFile {
    version: u32,
    name: Quoted,
}

/// The cases in a settings folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shelf {
    root: PathBuf,
}

impl Shelf {
    pub fn at(settings: &Path) -> Shelf {
        Shelf { root: settings.join(FOLDER) }
    }

    fn wall_dir(&self, wall: &WallId) -> PathBuf {
        self.root.join(&wall.0)
    }

    fn case_dir(&self, at: &CaseRef) -> PathBuf {
        self.wall_dir(&at.wall).join(&at.case.0)
    }

    fn lock_path(&self, at: &CaseRef) -> PathBuf {
        self.wall_dir(&at.wall).join(format!("{}.lock", at.case.0))
    }

    /// Walls and cases, swept first of `.trash-*` folders an interrupted delete left. A scan,
    /// so off the frame.
    pub fn list(&self) -> Listing {
        let mut out = Listing::default();
        let Ok(entries) = fs::read_dir(&self.root) else { return out };
        let mut walls: Vec<(WallId, PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                out.skipped = out.skipped.saturating_add(1);
                continue;
            };
            if name.starts_with(TRASH) {
                if is_real_dir(&entry) {
                    let _ = fs::remove_dir_all(entry.path());
                }
                continue;
            }
            match WallId::parse(name) {
                Some(id) if is_real_dir(&entry) => walls.push((id, entry.path())),
                _ => out.skipped = out.skipped.saturating_add(1),
            }
        }
        walls.sort();
        for (id, dir) in walls {
            let name = read_whole(&dir.join(WALL_FILE))
                .and_then(|bytes| serde_json::from_slice::<WallFile>(&bytes).ok())
                .filter(|w| w.version == VERSION)
                .map_or_else(|| Quoted::parse(""), |w| w.name);
            let mut cases = Vec::new();
            for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
                let file = entry.file_name();
                let Some(file) = file.to_str() else { continue };
                if file.starts_with(TRASH) {
                    if is_real_dir(&entry) {
                        let _ = fs::remove_dir_all(entry.path());
                    }
                    continue;
                }
                let Some(case) = CaseId::parse(file).filter(|_| is_real_dir(&entry)) else { continue };
                let read = read_case(&entry.path().join(CASE_FILE));
                cases.push(CaseHeader { id: case, title: read.title, damaged: read.damaged });
            }
            cases.sort_by(|a, b| a.id.cmp(&b.id));
            out.walls.push(Wall { id, name, cases });
        }
        out
    }

    /// The case as its file holds it. A file of at most 1 MiB: tolerated on a frame.
    pub fn open(&self, at: &CaseRef) -> Result<Case, CaseError> {
        let dir = self.real_case_dir(at)?;
        let path = dir.join(CASE_FILE);
        if fs::symlink_metadata(&path).is_err() {
            return Err(CaseError::Gone);
        }
        Ok(read_case(&path))
    }

    /// The wall's folder, if it is a folder and not a link to one.
    fn real_wall_dir(&self, wall: &WallId) -> Result<PathBuf, CaseError> {
        let dir = self.wall_dir(wall);
        real_dir(&dir).then_some(dir).ok_or(CaseError::Gone)
    }

    /// The case's folder, if it and its wall are folders and not links to one.
    fn real_case_dir(&self, at: &CaseRef) -> Result<PathBuf, CaseError> {
        let dir = self.real_wall_dir(&at.wall)?.join(&at.case.0);
        real_dir(&dir).then_some(dir).ok_or(CaseError::Gone)
    }

    /// The other cases of `at`'s wall, most recently changed first, at most `max_cases` of them and
    /// `max_bytes` of their files, and how many were skipped as damaged or unreadable. A scan of one
    /// wall's folder and nothing else: off the frame. A case whose file is damaged contributes
    /// nothing, since what it holds may be cut.
    pub(crate) fn recent_cases(&self, at: &CaseRef, max_cases: usize, max_bytes: u64) -> (Vec<(CaseId, Case)>, u32) {
        let Ok(dir) = self.real_wall_dir(&at.wall) else { return (Vec::new(), 0) };
        let mut found: Vec<(std::time::SystemTime, u64, CaseId, PathBuf)> = Vec::new();
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = entry.file_name();
            let Some(id) = name.to_str().and_then(CaseId::parse).filter(|id| *id != at.case && is_real_dir(&entry)) else { continue };
            let file = entry.path().join(CASE_FILE);
            let Ok(meta) = fs::symlink_metadata(&file) else { continue };
            found.push((meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH), meta.len(), id, file));
        }
        found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.2.cmp(&b.2)));
        let mut out = Vec::new();
        let mut skipped = 0u32;
        let mut bytes = 0u64;
        for (_, len, id, file) in found.into_iter().take(max_cases) {
            bytes = bytes.saturating_add(len);
            if bytes > max_bytes {
                break;
            }
            let case = read_case(&file);
            if case.damaged {
                skipped = skipped.saturating_add(1);
                continue;
            }
            out.push((id, case));
        }
        (out, skipped)
    }

    /// Cheap: one stat. Other windows learn of a delete here.
    pub fn exists(&self, at: &CaseRef) -> bool {
        self.case_dir(at).join(CASE_FILE).is_file()
    }

    /// A new, unique folder under `parent`, named from `stamp` and `counter`. `parent` must exist:
    /// a wall deleted meanwhile is `Gone`, not made again without its `wall.json`.
    fn new_dir(&self, parent: &Path, stamp: &str, counter: u64) -> Result<String, CaseError> {
        for attempt in 0..ID_TRIES {
            let id = id_from(stamp, counter, attempt);
            match make_dir(&parent.join(&id), false) {
                Ok(()) => return Ok(id),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(CaseError::Gone),
                Err(e) => return Err(CaseError::Io(format!("{}: {e}", parent.display()))),
            }
        }
        Err(CaseError::Io("a new case folder can't be named".into()))
    }

    /// A wall called `name`. `stamp` is the time as the caller writes it and `counter` a number
    /// the caller changes between calls. Together they must be unique in this process (say, the
    /// process id and a sub-second time): a deleted id made again would take the writes of a
    /// window still holding the old reference.
    pub fn new_wall(&self, _click: Click, name: &str, stamp: &str, counter: u64) -> Result<WallId, CaseError> {
        make_dir(&self.root, true).map_err(|e| CaseError::Io(format!("{}: {e}", self.root.display())))?;
        let id = self.new_dir(&self.root, stamp, counter)?;
        let mut name = Quoted::parse(name);
        name.cut(super::record::MAX_TEXT);
        let bytes = serde_json::to_vec(&WallFile { version: VERSION, name }).map_err(|e| CaseError::Io(e.to_string()))?;
        atomic::write(&self.root.join(&id).join(WALL_FILE), &bytes).map_err(CaseError::Io)?;
        WallId::parse(&id).ok_or_else(|| CaseError::Io("a new id isn't one".into()))
    }

    /// A case called `title` in `wall`. `stamp` and `counter` as for [`Shelf::new_wall`]: unique
    /// in this process, or a deleted case's id can come back.
    pub fn new_case(&self, _click: Click, wall: &WallId, title: &str, stamp: &str, counter: u64) -> Result<CaseRef, CaseError> {
        let dir = self.wall_dir(wall);
        // Only a link is refused here. A wall that is gone is found by `create_dir` itself, so no
        // check-then-create window can bring it back.
        if fs::symlink_metadata(&dir).is_ok_and(|m| m.is_symlink()) {
            return Err(CaseError::Gone);
        }
        let id = self.new_dir(&dir, stamp, counter)?;
        let case = Case::new(title);
        atomic::write(&dir.join(&id).join(CASE_FILE), &case.to_bytes().map_err(CaseError::Io)?).map_err(CaseError::Io)?;
        let case = CaseId::parse(&id).ok_or_else(|| CaseError::Io("a new id isn't one".into()))?;
        Ok(CaseRef { wall: wall.clone(), case })
    }

    /// Lock, read fresh, `change`, write. Never creates a folder. `move_damaged` says whether a
    /// file that can't be read whole is copied aside first (a person's write) or left for the
    /// person (a model's). A write that would pass [`MAX_FILE`] is refused before the file changes.
    fn update<T>(
        &self,
        at: &CaseRef,
        move_damaged: bool,
        lock: fn(&Path) -> Result<Lock, LockError>,
        change: impl FnOnce(&mut Case) -> Result<T, CaseError>,
    ) -> Result<T, CaseError> {
        let dir = self.real_case_dir(at)?;
        let _lock = lock(&self.lock_path(at)).map_err(|e| match e {
            LockError::Busy => CaseError::Busy,
            LockError::Io(e) => CaseError::Io(e),
        })?;
        let path = dir.join(CASE_FILE);
        if fs::symlink_metadata(&path).is_err() {
            return Err(CaseError::Gone);
        }
        let mut case = read_case(&path);
        let damaged = std::mem::take(&mut case.damaged);
        if damaged && !move_damaged {
            return Err(CaseError::Damaged);
        }
        let r = change(&mut case)?;
        let bytes = case.to_bytes().map_err(CaseError::Io)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_FILE {
            return Err(CaseError::Full("file"));
        }
        if damaged {
            // A copy, not a rename: `case.json` is there at every moment, so a crash here can't
            // make the case read as deleted.
            fs::copy(&path, dir.join(format!("{CASE_FILE}{DAMAGED_SUFFIX}")))
                .map_err(|e| CaseError::Io(format!("{} can't be copied aside: {e}", path.display())))?;
        }
        atomic::write(&path, &bytes).map_err(CaseError::Io)?;
        Ok(r)
    }

    /// What the person did, with their click: the case as written.
    pub fn person(&self, _click: Click, at: &CaseRef, op: PersonOp, today: Day) -> Result<Case, CaseError> {
        self.update(at, true, Lock::take, |case| {
            case.apply_person(op, today)?;
            Ok(case.clone())
        })
    }

    /// What a model wrote. A damaged file is left for the person's next change to move aside, and
    /// a lock another WordCraft holds is [`CaseError::Busy`] at once: the window keeps the write
    /// for a later frame rather than waiting in this one.
    pub fn model(&self, at: &CaseRef, write: ModelWrite, today: Day) -> Result<Applied, CaseError> {
        self.update(at, false, Lock::try_take, |case| case.apply_model(write, today))
    }

    /// Rename `dir` to a trash folder under the root, then remove it. Safe to run twice, and a
    /// crash between the two is finished by the next [`Shelf::list`].
    fn trash(&self, dir: &Path, id: &str) -> Result<(), CaseError> {
        let trash = self.root.join(format!("{TRASH}{id}"));
        match fs::rename(dir, &trash) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(CaseError::Io(format!("{} can't be removed: {e}", dir.display()))),
        }
        match fs::remove_dir_all(&trash) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(CaseError::Io(format!("{} can't be removed: {e}", trash.display()))),
        }
    }

    /// The case goes, under its lock, so a write racing it finds it gone rather than half there.
    pub fn delete_case(&self, _click: Click, at: &CaseRef) -> Result<(), CaseError> {
        let dir = self.real_case_dir(at)?;
        let lock_path = self.lock_path(at);
        {
            let _lock = Lock::take(&lock_path).map_err(|e| match e {
                LockError::Busy => CaseError::Busy,
                LockError::Io(e) => CaseError::Io(e),
            })?;
            self.trash(&dir, at.case.as_str())?;
        }
        let _ = fs::remove_file(&lock_path);
        Ok(())
    }

    /// The wall goes if it holds no case. The page offers this when its listing shows none, but
    /// another window may have added one since, so the folder is checked here, where the delete is.
    /// ponytail: a case made between the check and the rename goes with the folder; a wall has no
    /// lock to close that window.
    pub fn delete_empty_wall(&self, _click: Click, wall: &WallId) -> Result<(), CaseError> {
        let dir = self.real_wall_dir(wall)?;
        let has_case = fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .any(|entry| entry.file_name().to_str().and_then(CaseId::parse).is_some() && is_real_dir(&entry));
        if has_case {
            return Err(CaseError::NotEmpty);
        }
        self.trash(&dir, wall.as_str())
    }
}

/// A folder only its owner can enter. `recursive` makes missing parents and accepts one that exists.
fn make_dir(path: &Path, recursive: bool) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(recursive);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path)
}

/// A folder, and not a link to one.
fn real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

/// [`real_dir`] from a listing, without another stat.
fn is_real_dir(entry: &fs::DirEntry) -> bool {
    entry.file_type().is_ok_and(|t| t.is_dir())
}

/// At most [`MAX_FILE`] bytes of `path`; `None` when there are more, it can't be read, or it
/// isn't a plain file. A link is refused rather than followed: one left in a case folder could
/// point at another wall's case, and the folder is the ethical wall.
fn read_whole(path: &Path) -> Option<Vec<u8>> {
    if !fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE.saturating_add(1)).read_to_end(&mut bytes).ok()?;
    (u64::try_from(bytes.len()).unwrap_or(u64::MAX) <= MAX_FILE).then_some(bytes)
}

/// The case at `path`: damaged when it can't be read whole or holds more than this build reads.
fn read_case(path: &Path) -> Case {
    match read_whole(path) {
        Some(bytes) => Case::read(&bytes),
        None => {
            let mut case = Case::new("");
            case.damaged = true;
            case
        }
    }
}

#[cfg(test)]
mod tests;

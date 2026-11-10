//! Which file the person bound to which case, so that opening the file again offers that case. The
//! binding is the person's click, kept beside the walls in `bindings.json` and never in the
//! document, whose `custom.xml` anyone can write. It is keyed by the file's canonical path: a copy
//! saved under another path is another file and is offered nothing until the person binds it. A
//! path doesn't say which document is there, so the window offers the case and binds it only with
//! the person's click.
//!
//! ```json
//! {"version":1,"files":[{"path":"/Users/me/Acme MSA.docx","wall":"<16 hex>","case":"<16 hex>"}]}
//! ```
//!
//! Closed and versioned like `case.json`: a field this build doesn't know, another version, more
//! than [`MAX_FILES`] files or a path past [`MAX_PATH`] makes the file damaged. A damaged file
//! restores nothing, and the person's next bind or unbind moves it aside as
//! `bindings.json.damaged` before writing, so it is never replaced in silence.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{CaseId, CaseRef, DAMAGED_SUFFIX, MAX_FILE, Shelf, WallId, read_whole, real_dir};
use crate::atomic::{self, Lock, LockError};
use crate::cases::record::{CaseError, Click, VERSION};

/// The file, beside the walls.
pub const FILE: &str = "bindings.json";

const LOCK: &str = "bindings.lock";

/// Most files bound. Past it the oldest binding goes.
/// ponytail: a person who binds more files than this binds the oldest again; a file per wall
/// would lift the cap.
pub const MAX_FILES: usize = 200;

/// Most bytes of a path as the file writes it, escaped, so that [`MAX_FILES`] of them fit
/// [`MAX_FILE`].
pub const MAX_PATH: usize = 4096;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bindings {
    version: u32,
    files: Vec<Bound>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bound {
    path: String,
    wall: WallId,
    case: CaseId,
}

/// Whether `path` fits [`MAX_PATH`] as the file writes it.
fn fits(path: &str) -> bool {
    serde_json::to_string(path).is_ok_and(|json| json.len() <= MAX_PATH)
}

/// What a binding is kept under: the file's canonical path, when it is on disk and fits.
fn key(file: &Path) -> Option<String> {
    let path = fs::canonicalize(file).ok()?.to_str()?.to_string();
    fits(&path).then_some(path)
}

/// The bindings in `path`: none when there is no file, `None` when it can't be read whole.
fn read(path: &Path) -> Option<Vec<Bound>> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(Vec::new()),
        // There but unreadable: damaged, so a write doesn't replace it unseen.
        Err(_) => return None,
        Ok(_) => {}
    }
    let file: Bindings = serde_json::from_slice(&read_whole(path, MAX_FILE)?).ok()?;
    let whole = file.version == VERSION && file.files.len() <= MAX_FILES && file.files.iter().all(|b| fits(&b.path));
    whole.then_some(file.files)
}

impl Shelf {
    /// Lock, read fresh, `change`, and write when it changed something. A damaged file is moved
    /// aside first for the `person`'s write, and left as it is otherwise. Nothing to do when no
    /// case was ever made: the folder isn't there.
    fn update_bindings(
        &self,
        person: bool,
        lock: fn(&Path) -> Result<Lock, LockError>,
        change: impl FnOnce(&mut Vec<Bound>) -> bool,
    ) -> Result<(), CaseError> {
        if !real_dir(&self.root) {
            return Ok(());
        }
        let _lock = lock(&self.root.join(LOCK)).map_err(|e| match e {
            LockError::Busy => CaseError::Busy,
            LockError::Io(e) => CaseError::Io(e),
        })?;
        let path = self.root.join(FILE);
        let (mut files, damaged) = read(&path).map_or_else(|| (Vec::new(), true), |files| (files, false));
        if damaged && !person {
            return Ok(());
        }
        if !change(&mut files) && !damaged {
            return Ok(());
        }
        let bytes = serde_json::to_vec(&Bindings { version: VERSION, files }).map_err(|e| CaseError::Io(e.to_string()))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_FILE {
            return Err(CaseError::Full("file"));
        }
        if damaged {
            atomic::rename_if_there(&path, &self.root.join(format!("{FILE}{DAMAGED_SUFFIX}"))).map_err(CaseError::Io)?;
        }
        atomic::write(&path, &bytes).map_err(CaseError::Io)
    }

    /// Keep the person's binding of `file` to `at`, in place of any it had. `Ok(false)` when the
    /// file has no path to keep it under (not on disk yet, or a path past [`MAX_PATH`]): the
    /// binding then lasts while the document is open.
    pub fn bind_file(&self, _click: Click, file: &Path, at: &CaseRef) -> Result<bool, CaseError> {
        let Some(path) = key(file) else { return Ok(false) };
        self.update_bindings(true, Lock::take, |files| {
            files.retain(|b| b.path != path);
            files.push(Bound { path, wall: at.wall.clone(), case: at.case.clone() });
            let over = files.len().saturating_sub(MAX_FILES);
            files.drain(..over);
            true
        })?;
        Ok(true)
    }

    /// Forget `file`'s binding: the person unbound it.
    pub fn unbind_file(&self, _click: Click, file: &Path) -> Result<(), CaseError> {
        let Some(path) = key(file) else { return Ok(()) };
        self.update_bindings(true, Lock::take, |files| drop_where(files, |b| b.path == path))
    }

    /// The case `file` was bound to, if that case is still there. A binding whose case is gone is
    /// dropped from the file, quietly and without waiting for another WordCraft's lock.
    pub fn file_bound(&self, file: &Path) -> Option<CaseRef> {
        let path = key(file)?;
        let files = read(&self.root.join(FILE))?;
        let b = files.iter().rev().find(|b| b.path == path)?;
        let at = CaseRef { wall: b.wall.clone(), case: b.case.clone() };
        if self.exists(&at) {
            return Some(at);
        }
        let _ = self.update_bindings(false, Lock::try_take, |files| drop_where(files, |b| b.path == path));
        None
    }

    /// Every binding to the case or wall `id` goes, as its folder does ([`Shelf::trash`] and the
    /// sweep that finishes one). A failure leaves them for [`Shelf::file_bound`] to drop.
    pub(super) fn forget(&self, id: &str) {
        if fs::symlink_metadata(self.root.join(FILE)).is_err() {
            return;
        }
        let r = self.update_bindings(false, Lock::take, |files| drop_where(files, |b| b.case.as_str() == id || b.wall.as_str() == id));
        if let Err(e) = r {
            log::warn!("a deleted case's bindings stay until its file is opened again: {e}");
        }
    }
}

/// Remove what `gone` picks; whether anything went.
fn drop_where(files: &mut Vec<Bound>, gone: impl Fn(&Bound) -> bool) -> bool {
    let before = files.len();
    files.retain(|b| !gone(b));
    files.len() != before
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    const TODAY: &str = "2026-11-10";

    fn click() -> Click {
        Click::by_person(true).unwrap()
    }

    /// A settings folder with a client holding `cases` cases, and a document file per case.
    fn shelf(name: &str, cases: u64) -> (Shelf, PathBuf, Vec<(CaseRef, PathBuf)>) {
        let dir = std::env::temp_dir().join(format!("wordcraft-bindings-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let shelf = Shelf::at(&dir);
        let wall = shelf.new_wall(click(), "Acme", TODAY, 1).unwrap();
        let made = (0..cases)
            .map(|n| {
                let at = shelf.new_case(click(), &wall, &format!("Case {n}"), TODAY, n + 2).unwrap();
                let file = dir.join(format!("Draft {n}.docx"));
                fs::write(&file, b"a document").unwrap();
                (at, file)
            })
            .collect();
        (shelf, dir, made)
    }

    fn file_text(shelf: &Shelf) -> String {
        fs::read_to_string(shelf.root.join(FILE)).unwrap_or_default()
    }

    #[test]
    fn a_files_binding_comes_back_by_its_canonical_path_and_another_file_has_none() {
        let (shelf, dir, cases) = shelf("canonical", 2);
        let (here, file) = &cases[0];
        assert_eq!(shelf.bind_file(click(), file, here), Ok(true));
        let spelled_otherwise = dir.join("..").join(dir.file_name().unwrap()).join("Draft 0.docx");
        assert_eq!(shelf.file_bound(&spelled_otherwise), Some(here.clone()), "the same file, however its path is spelled");
        assert_eq!(shelf.file_bound(&cases[1].1), None, "another file binds to nothing");
        assert_eq!(shelf.bind_file(click(), &dir.join("Not saved yet.docx"), here), Ok(false), "a file not on disk isn't kept");

        let (there, _) = &cases[1];
        shelf.bind_file(click(), file, there).unwrap();
        assert_eq!(shelf.file_bound(file), Some(there.clone()), "binding again replaces it");
        assert_eq!(file_text(&shelf).matches("Draft 0.docx").count(), 1, "one binding per file");
        shelf.unbind_file(click(), file).unwrap();
        assert_eq!(shelf.file_bound(file), None, "unbinding forgets it");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_deleted_cases_bindings_go_with_it_even_after_a_crash_mid_delete() {
        let (shelf, dir, cases) = shelf("deleted", 3);
        for (at, file) in &cases {
            shelf.bind_file(click(), file, at).unwrap();
        }
        let (deleted, crashed, kept) = (&cases[0], &cases[1], &cases[2]);
        shelf.delete_case(click(), &deleted.0, false).unwrap();
        assert!(!file_text(&shelf).contains(deleted.0.case.as_str()), "{}", file_text(&shelf));

        // A crash after the rename: the sweep at the next listing drops the bindings too.
        fs::rename(shelf.case_dir(&crashed.0), shelf.root.join(format!(".trash-{}", crashed.0.case.as_str()))).unwrap();
        assert!(file_text(&shelf).contains(crashed.0.case.as_str()));
        shelf.list();
        assert!(!file_text(&shelf).contains(crashed.0.case.as_str()), "{}", file_text(&shelf));
        assert_eq!(shelf.file_bound(&kept.1), Some(kept.0.clone()), "another case's binding stays");
        let _ = fs::remove_dir_all(dir);
    }

    /// A case's record goes through the same trash as the case, under `<case id>-log`: deleting
    /// the record alone, or the sweep after a crash in that delete, leaves the file bound; deleting
    /// the case with its record in it forgets the file.
    #[test]
    fn a_cases_record_going_keeps_its_bindings_and_the_case_going_with_its_record_drops_them() {
        let (shelf, dir, cases) = shelf("record", 1);
        let (at, file) = &cases[0];
        shelf.bind_file(click(), file, at).unwrap();
        let log = shelf.log_dir(at);
        let fill = || {
            fs::create_dir_all(&log).unwrap();
            fs::write(log.join("20261110-000000-a.jsonl"), b"{\"t\":\"dropped\",\"n\":1}\n").unwrap();
        };

        fill();
        crate::cases::log::delete_log(&shelf, at).unwrap();
        assert!(!log.exists());
        assert_eq!(shelf.file_bound(file), Some(at.clone()), "the record went, the case and its binding stay");

        fill();
        fs::rename(&log, shelf.root.join(format!(".trash-{}-log", at.case.as_str()))).unwrap();
        shelf.list();
        assert_eq!(shelf.file_bound(file), Some(at.clone()), "the sweep of a record's delete leaves the binding");

        fill();
        let case_dir = shelf.case_dir(at);
        shelf.delete_case(click(), at, false).unwrap();
        assert!(!case_dir.exists(), "the case and its record are gone");
        assert!(!file_text(&shelf).contains(at.case.as_str()), "{}", file_text(&shelf));
        assert_eq!(shelf.file_bound(file), None);
        let _ = fs::remove_dir_all(dir);
    }

    /// A case removed some other way (by hand, or a delete whose bindings couldn't be written) is
    /// dropped as its file is opened, and the file binds to nothing.
    #[test]
    fn a_binding_whose_case_is_gone_is_dropped_quietly() {
        let (shelf, dir, cases) = shelf("gone", 1);
        let (at, file) = &cases[0];
        shelf.bind_file(click(), file, at).unwrap();
        fs::remove_dir_all(shelf.case_dir(at)).unwrap();
        assert_eq!(shelf.file_bound(file), None);
        assert!(!file_text(&shelf).contains(at.case.as_str()), "{}", file_text(&shelf));
        let _ = fs::remove_dir_all(dir);
    }

    fn entry(path: &str, at: &CaseRef) -> serde_json::Value {
        serde_json::json!({"path": path, "wall": at.wall.as_str(), "case": at.case.as_str()})
    }

    #[test]
    fn a_damaged_or_hostile_bindings_file_binds_nothing_and_the_persons_next_bind_moves_it_aside() {
        let (shelf, dir, cases) = shelf("damaged", 1);
        let (at, file) = &cases[0];
        let path = shelf.root.join(FILE);
        let aside = shelf.root.join(format!("{FILE}{DAMAGED_SUFFIX}"));
        let good = entry(&key(file).unwrap(), at);
        let file_of =
            |version: u32, files: Vec<serde_json::Value>| serde_json::to_vec(&serde_json::json!({"version": version, "files": files})).unwrap();
        let mut unknown_in_entry = good.clone();
        unknown_in_entry["colour"] = serde_json::json!("red");
        let mut bad_id = good.clone();
        bad_id["case"] = serde_json::json!("../../etc/passwd");
        let long = format!("/{}", "a".repeat(MAX_PATH));
        let too_many: Vec<_> = (0..=MAX_FILES).map(|n| entry(&format!("/f{n}"), at)).collect();
        let mut over_limit = file_of(1, vec![good.clone()]);
        over_limit.extend(std::iter::repeat_n(b' ', usize::try_from(MAX_FILE).unwrap()));
        for (what, bytes) in [
            ("junk", b"{not json".to_vec()),
            ("another version", file_of(2, vec![good.clone()])),
            ("a field this build doesn't know", serde_json::to_vec(&serde_json::json!({"version": 1, "files": [good], "mood": 1})).unwrap()),
            ("a field in an entry", file_of(1, vec![unknown_in_entry])),
            ("an id that isn't one", file_of(1, vec![bad_id])),
            ("a path past the limit", file_of(1, vec![entry(&long, at)])),
            ("more files than the limit", file_of(1, too_many)),
            ("more bytes than the limit", over_limit),
        ] {
            fs::write(&path, &bytes).unwrap();
            let _ = fs::remove_file(&aside);
            assert_eq!(shelf.file_bound(file), None, "{what} binds nothing");
            shelf.forget(at.case.as_str());
            assert_eq!(fs::read(&path).unwrap(), bytes, "{what}: only the person's write changes it");
            assert_eq!(shelf.bind_file(click(), file, at), Ok(true), "{what}");
            assert_eq!(fs::read(&aside).unwrap(), bytes, "{what} is moved aside, not replaced in silence");
            assert_eq!(shelf.file_bound(file), Some(at.clone()), "{what}: the new binding holds");
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn bindings_at_every_limit_fit_the_file_and_the_oldest_goes_past_the_cap() {
        let (shelf, dir, cases) = shelf("limits", 1);
        let (at, file) = &cases[0];
        // Every path as long as it may be once escaped, the backslashes doubling.
        let longest = |n: usize| format!("{n:04}{}", "\\".repeat((MAX_PATH - 6) / 2));
        assert_eq!(serde_json::to_string(&longest(0)).unwrap().len(), MAX_PATH);
        let full: Vec<Bound> = (0..MAX_FILES).map(|n| Bound { path: longest(n), wall: at.wall.clone(), case: at.case.clone() }).collect();
        let bytes = serde_json::to_vec(&Bindings { version: VERSION, files: full }).unwrap();
        assert!(bytes.len() as u64 <= MAX_FILE, "{} bytes", bytes.len());
        fs::write(shelf.root.join(FILE), &bytes).unwrap();
        assert_eq!(read(&shelf.root.join(FILE)).map(|f| f.len()), Some(MAX_FILES), "a full file reads whole");

        shelf.bind_file(click(), file, at).unwrap();
        let files = read(&shelf.root.join(FILE)).unwrap();
        assert_eq!(files.len(), MAX_FILES);
        assert_eq!(files[0].path, longest(1), "the oldest went");
        assert_eq!(shelf.file_bound(file), Some(at.clone()));
        let _ = fs::remove_dir_all(dir);
    }
}

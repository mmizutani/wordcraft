//! Two WordCrafts share the files the assistant keeps in the settings folder. Every writer takes a
//! lock file, reads the file again under it, and replaces it in one rename, so a crash or a second
//! window never leaves a half-written file or loses the other's change. [`memory`](crate::memory)
//! and [`cases::store`](crate::cases::store) both write this way.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Longest a write waits for another WordCraft's: a window's frame is waiting.
const LOCK_WAIT: Duration = Duration::from_millis(500);

/// What the person is told when the lock is held past [`LOCK_WAIT`].
pub(crate) const BUSY: &str = "another WordCraft is saving what the assistant remembers: try again";

/// The lock file, held by the operating system until this is dropped, so a WordCraft that crashed
/// holds nothing.
pub(crate) struct Lock(fs::File);

/// Why a lock wasn't taken.
#[derive(Debug)]
pub(crate) enum LockError {
    /// Another WordCraft held it past [`LOCK_WAIT`].
    Busy,
    Io(String),
}

impl From<LockError> for String {
    fn from(e: LockError) -> String {
        match e {
            LockError::Busy => BUSY.to_string(),
            LockError::Io(e) => e,
        }
    }
}

impl Lock {
    pub(crate) fn take(path: &Path) -> Result<Lock, LockError> {
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .map_err(|e| LockError::Io(format!("{}: {e}", path.display())))?;
        let until = Instant::now().checked_add(LOCK_WAIT);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Lock(file)),
                Err(fs::TryLockError::WouldBlock) => {
                    if until.is_none_or(|until| Instant::now() >= until) {
                        return Err(LockError::Busy);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                // A file system without locks: the fresh read before the write still narrows the race.
                Err(fs::TryLockError::Error(e)) => {
                    log::warn!("{} can't be locked: {e}", path.display());
                    return Ok(Lock(file));
                }
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// `bytes` as a new file beside `path`, only its owner able to read it, renamed over it: a failed
/// write leaves the old one as it was. The caller's lock makes the temporary name this writer's alone.
pub(crate) fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temp: PathBuf = path.with_extension("json.tmp");
    let written = (|| {
        match fs::remove_file(&temp) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let mut open = fs::OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
        let mut f = open.open(&temp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&temp, path)
    })();
    written.map_err(|e| {
        let _ = fs::remove_file(&temp);
        format!("{} can't be written: {e}", path.display())
    })
}

/// Rename `from` to `to`; a missing `from` is nothing to do.
pub(crate) fn rename_if_there(from: &Path, to: &Path) -> Result<(), String> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{} can't be moved aside: {e}", from.display())),
    }
}

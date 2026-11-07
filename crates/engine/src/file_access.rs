//! The folders automation may read and write (#769).
//!
//! `wordcraft-cli mcp --automation-read-root <dir> --automation-write-root <dir>` confines the
//! files an agent's commands reach, and so do the folders File › Options › Agents lets agents in
//! a shared document use ([`AutomationRoots::folders`]). [`AutomationRoots`] holds the folders:
//! some to read, some to write. While they are in force, every path a command reads goes through
//! [`check_read`] and every path it writes through [`check_write`]:
//!
//! - The path is made absolute against the working directory, as the file system reads it. Its
//!   links are followed: for a file that doesn't exist yet, the links of its nearest existing
//!   folder, the rest of the path being plain names (`..` below a missing folder is refused, and so
//!   is a link that leads nowhere).
//! - The result must lie inside one of the roots, compared by whole components (`/work2` isn't
//!   inside `/work`), as the file system spells them both (on Windows, a drive letter in either
//!   case).
//! - Writing replaces or removes a directory entry, so for write access the entry itself (its
//!   folder's links followed, its own name as given) must lie inside a write root as well: a link
//!   outside the roots that leads into one is not a way to write outside (#3735).
//! - Read and write access are separate: a root given alone grants only its own kind of access.
//!   A folder inside another one is reached as both are: a read-only folder inside one that may
//!   be written is written too.
//!
//! The folders agents in a shared document use are stricter: a path must be a full one, a network
//! path is refused, and a path outside every folder is refused by its spelling, before the file
//! system is asked anything about it (an agent learns nothing about files outside its folders).
//! Agents given any file ([`AutomationRoots::everywhere`]) are held to full paths too, and to
//! this computer: a network path is refused by its spelling, and on Windows a drive mapped to
//! another computer, or a link to one, once its links are followed. (On macOS, Linux and BSD a
//! mounted network folder is an ordinary path, which they reach as any other.)
//!
//! Folders kept out of reach ([`protect`]: WordCraft's settings folder, which holds the window's
//! control key, its recovery copies and its preferences) are refused to every confined command,
//! whatever its roots, once their links are followed.
//!
//! Roots are in force on a thread inside [`confine`], and in the whole process after
//! [`confine_process`] (`wordcraft-cli mcp`), except inside [`unconfined`]. Without roots
//! nothing is checked.
//!
//! A path is checked, then opened: a process that can change the folders in between (swapping in
//! a link) can still win that race. The roots keep an agent to the files it was given; they don't
//! guard against other programs on the computer.

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// Read access or write access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

/// The folders automation may read and write; with no folder of a kind, that kind of access is
/// granted nowhere.
#[derive(Debug)]
pub struct AutomationRoots {
    read: Vec<Root>,
    write: Vec<Root>,
    /// Every path may be read and written, but those [`protect`] keeps out of reach
    /// ([`AutomationRoots::everywhere`]).
    everywhere: bool,
    /// For agents in a shared document: a path must be a full one, a network path is refused, and
    /// with folders a path outside all of them is refused by its spelling, before the file system
    /// is asked about it.
    strict: bool,
    /// Once set, every path is refused ([`AutomationRoots::until`]).
    withdrawn: Option<Arc<AtomicBool>>,
}

/// The most folders an error message names; the rest are counted.
const NAMED: usize = 3;

#[derive(Clone, Debug)]
struct Root {
    /// The folder made absolute, links followed: what paths are compared with.
    resolved: PathBuf,
    /// The same without Windows' `\\?\` prefix, for people.
    shown: PathBuf,
    /// The folder made absolute as it was spelled, `.` and `..` taken out by name: what a path
    /// is compared with before the file system is asked about it.
    spelled: PathBuf,
}

impl Root {
    /// The folder at `path`, which must exist.
    fn open(path: &Path, access: Access) -> Result<Self, String> {
        let kind = match access {
            Access::Read => "read",
            Access::Write => "write",
        };
        let resolved = std::fs::canonicalize(path)
            .and_then(|p| if p.is_dir() { Ok(p) } else { Err(std::io::ErrorKind::NotADirectory.into()) })
            .map_err(|e| format!("cannot open the automation {kind} root `{}`: {e}", path.display()))?;
        Ok(Self { shown: plain(resolved.clone()), resolved, spelled: spelled(path) })
    }

    /// Whether `path`, spelled out ([`spelled`]), is this folder or inside it as spelled, as the
    /// file system spells it, or without the `\\?\` prefix (without regard to case on Windows:
    /// what passes is checked again once resolved).
    fn spells(&self, path: &Path) -> bool {
        [&self.spelled, &self.resolved, &self.shown].into_iter().any(|root| inside(path, root, true))
    }
}

impl AutomationRoots {
    /// The roots `read` and `write`, which must be existing folders; `None` when neither is given
    /// (nothing is confined then).
    pub fn new(read: Option<&Path>, write: Option<&Path>) -> Result<Option<Arc<Self>>, String> {
        if read.is_none() && write.is_none() {
            return Ok(None);
        }
        let read = read.map(|p| Root::open(p, Access::Read)).transpose()?;
        let write = write.map(|p| Root::open(p, Access::Write)).transpose()?;
        Ok(Some(Arc::new(Self {
            read: read.into_iter().collect(),
            write: write.into_iter().collect(),
            everywhere: false,
            strict: false,
            withdrawn: None,
        })))
    }

    /// No folder at all: every path a command reads or writes is refused. For the copies the
    /// process hosts for participants who joined a shared document with a key
    /// ([`crate::room::hosted::call`]), while the files agents can use are none.
    pub fn none() -> Arc<Self> {
        Arc::new(Self { read: vec![], write: vec![], everywhere: false, strict: true, withdrawn: None })
    }

    /// `folders`, each `(path, write)`: read, and written too when `write`. For the folders agents
    /// in a shared document can use. A folder that can't be opened (gone, or not a folder) is
    /// left out, so with none left nothing is in reach: never everything.
    pub fn folders<'a>(folders: impl IntoIterator<Item = (&'a str, bool)>) -> Arc<Self> {
        let (mut read, mut write) = (vec![], vec![]);
        for (path, writes) in folders {
            let Ok(root) = Root::open(Path::new(path), Access::Read) else { continue };
            if writes {
                write.push(root.clone());
            }
            read.push(root);
        }
        Arc::new(Self { read, write, everywhere: false, strict: true, withdrawn: None })
    }

    /// Any file on this computer, given by its full path, but the folders [`protect`]
    /// keeps out of reach. For agents in a shared document given any file.
    pub fn everywhere() -> Arc<Self> {
        Arc::new(Self { read: vec![], write: vec![], everywhere: true, strict: true, withdrawn: None })
    }

    /// These roots until `withdrawn` is set, and from then on no path at all: a participant's
    /// reach for one request, taken back when the person narrows which files agents can use or
    /// the participant's invite, while the request runs (a long macro, say).
    pub fn until(&self, withdrawn: Arc<AtomicBool>) -> Arc<Self> {
        Arc::new(Self {
            read: self.read.clone(),
            write: self.write.clone(),
            everywhere: self.everywhere,
            strict: self.strict,
            withdrawn: Some(withdrawn),
        })
    }

    /// The (first) read root, absolute with its links followed.
    pub fn read_root(&self) -> Option<&Path> {
        self.read.first().map(|r| r.shown.as_path())
    }

    /// The (first) write root, absolute with its links followed.
    pub fn write_root(&self) -> Option<&Path> {
        self.write.first().map(|r| r.shown.as_path())
    }

    /// May the file or folder at `path` be read (or written)? `Err` says why not. Roots the
    /// process runs under ([`confine_process`]) hold too.
    pub fn check(&self, path: &str, access: Access) -> Result<(), String> {
        if self.withdrawn.as_ref().is_some_and(|w| w.load(Ordering::Acquire)) {
            return Err(format!("automation path rejected: the files you can use changed while this request ran: {path}"));
        }
        let (roots, kind) = match access {
            Access::Read => (&self.read, "read"),
            Access::Write => (&self.write, "write"),
        };
        if roots.is_empty() && !self.everywhere {
            return Err(format!("automation filesystem access is not granted: {kind} authority is absent: {path}"));
        }
        // Agents' paths are judged by their spelling first: the file system is asked nothing about
        // one outside their folders, so a refusal says nothing about what is there, and a network
        // path never makes Windows sign in to another computer (#2852).
        if self.strict {
            if !Path::new(path).is_absolute() {
                return Err(format!("automation path rejected: not a full path: {path}"));
            }
            if crate::cmd::templates::is_network_path(path) {
                return Err(format!("automation path rejected: on another computer: {path}"));
            }
            if !self.everywhere && !roots.iter().any(|r| r.spells(&spelled(Path::new(path)))) {
                return Err(outside(kind, roots, path));
            }
        }
        let in_one = |p: &Path| self.everywhere || roots.iter().any(|r| inside(p, &r.resolved, false));
        let resolved = resolve(path)?;
        if protected(&resolved) {
            return Err(format!("automation path rejected: WordCraft's settings folder is out of reach: {path}"));
        }
        // A drive mapped to another computer, or a link to one, is only seen to be one once its
        // links are followed (Windows): agents given any file don't reach it either.
        if self.strict && on_another_computer(&resolved) {
            return Err(format!("automation path rejected: on another computer: {path}"));
        }
        if !in_one(&resolved) {
            return Err(outside(kind, roots, path));
        }
        // Writing replaces or removes the directory entry `path` names (a safe save renames its
        // temporary file over it, a rename removes it): that entry must be inside a write root
        // too, not only what a link there leads to (#3735).
        if access == Access::Write {
            let entry = resolve_entry(path)?;
            if protected(&entry) {
                return Err(format!("automation path rejected: WordCraft's settings folder is out of reach: {path}"));
            }
            if self.strict && on_another_computer(&entry) {
                return Err(format!("automation path rejected: on another computer: {path}"));
            }
            if !in_one(&entry) {
                return Err(outside(kind, roots, path));
            }
        }
        // What the process itself runs under still holds (`wordcraft-cli mcp`'s roots).
        match PROCESS.get() {
            Some(process) if !std::ptr::eq(process.as_ref(), self) => process.check(path, access),
            _ => Ok(()),
        }
    }
}

/// Why `path` is refused: it is outside the `kind` folders `roots`.
fn outside(kind: &str, roots: &[Root], path: &str) -> String {
    match roots {
        [root] => format!("automation path rejected: outside the {kind} root {}: {path}", root.shown.display()),
        _ => {
            let mut named: Vec<String> = roots.iter().take(NAMED).map(|r| r.shown.display().to_string()).collect();
            if roots.len() > NAMED {
                named.push(format!("{} more", roots.len() - NAMED));
            }
            format!("automation path rejected: outside the {kind} roots {}: {path}", named.join(", "))
        }
    }
}

/// Whether `resolved` (a path with its links followed) is on another computer: on Windows, a
/// network path, which is what a drive mapped to a share, or a link to one, resolves to. On macOS,
/// Linux and BSD a mounted network folder is an ordinary path, which nothing tells apart.
fn on_another_computer(resolved: &Path) -> bool {
    crate::cmd::templates::is_network_path(&resolved.to_string_lossy())
}

/// Folders kept out of reach of every confined command ([`protect`]), spelled out ([`spelled`]).
static PROTECTED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Keep `folder` out of reach of every confined command from now on, whatever roots are in force:
/// WordCraft's settings folder, which holds the window's control key (with it an agent would
/// be the window), its recovery copies and its preferences. Commands that aren't confined, the
/// person's own, still reach it. The folder needn't exist yet.
pub fn protect(folder: &Path) {
    let folder = spelled(folder);
    let mut kept = PROTECTED.lock().unwrap_or_else(PoisonError::into_inner);
    if !kept.contains(&folder) {
        kept.push(folder);
    }
}

/// Whether `resolved` (a path [`resolve`] resolved) is in a folder kept out of reach: compared with
/// the folder as spelled and with its links followed, without regard to case on Windows (so it
/// refuses more, never less).
fn protected(resolved: &Path) -> bool {
    let kept = PROTECTED.lock().unwrap_or_else(PoisonError::into_inner).clone();
    kept.iter().any(|folder| {
        inside(resolved, folder, true) || resolve_absolute(&folder.to_string_lossy(), folder).is_ok_and(|real| inside(resolved, &real, true))
    })
}

/// `path` made absolute against the working directory, its `.` and `..` taken out by name: the
/// file system is asked nothing about it.
fn spelled(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::new();
    for c in absolute.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

/// Where the roots are in force on a thread ([`confine`], [`unconfined`]).
#[derive(Clone, Debug)]
enum Scope {
    Confined(Arc<AutomationRoots>),
    Unconfined,
}

thread_local! {
    /// This thread's scope; `None`: the process's roots ([`confine_process`]).
    static SCOPE: RefCell<Option<Scope>> = const { RefCell::new(None) };
}

/// The roots in force in every thread outside [`confine`] and [`unconfined`].
static PROCESS: OnceLock<Arc<AutomationRoots>> = OnceLock::new();

/// Put `roots` in force in the whole process, for good. `false` when roots were in force already
/// (they stay as they were).
pub fn confine_process(roots: Arc<AutomationRoots>) -> bool {
    PROCESS.set(roots).is_ok()
}

/// Puts the scope it replaced back when dropped (also when `f` panics).
struct Restore(Option<Scope>);

impl Drop for Restore {
    fn drop(&mut self) {
        let previous = self.0.take();
        SCOPE.with(|s| *s.borrow_mut() = previous);
    }
}

fn with_scope<T>(scope: Option<Scope>, f: impl FnOnce() -> T) -> T {
    let _restore = Restore(SCOPE.with(|s| s.replace(scope)));
    f()
}

/// Run `f` with `roots` in force on this thread (`None`: as things stand).
pub fn confine<T>(roots: Option<&Arc<AutomationRoots>>, f: impl FnOnce() -> T) -> T {
    match roots {
        Some(r) => with_scope(Some(Scope::Confined(r.clone())), f),
        None => f(),
    }
}

/// Run `f` with no roots in force on this thread: what the person at the keyboard does, and the
/// app's own folders.
pub fn unconfined<T>(f: impl FnOnce() -> T) -> T {
    with_scope(Some(Scope::Unconfined), f)
}

/// Whether [`confine`] (with roots) or [`unconfined`] decided what this thread reaches, rather
/// than the process ([`confine_process`]).
pub(crate) fn scoped() -> bool {
    SCOPE.with(|s| s.borrow().is_some())
}

/// The roots in force on this thread, if any.
pub fn current() -> Option<Arc<AutomationRoots>> {
    match SCOPE.with(|s| s.borrow().clone()) {
        Some(Scope::Confined(r)) => Some(r),
        Some(Scope::Unconfined) => None,
        None => PROCESS.get().cloned(),
    }
}

/// `path` without Windows' `\\?\` prefix (which `canonicalize` adds), for people to read.
pub(crate) fn plain(path: PathBuf) -> PathBuf {
    if !cfg!(windows) {
        return path;
    }
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => path,
    }
}

/// May `path` be read here? Always, without roots in force.
pub fn check_read(path: &str) -> Result<(), String> {
    current().map_or(Ok(()), |r| r.check(path, Access::Read))
}

/// May `path` be written here? Always, without roots in force.
pub fn check_write(path: &str) -> Result<(), String> {
    current().map_or(Ok(()), |r| r.check(path, Access::Write))
}

/// `path` made absolute with its links followed; for a path that doesn't exist (yet), its nearest
/// existing folder's links followed and the rest appended.
fn resolve(path: &str) -> Result<PathBuf, String> {
    if path.is_empty() {
        return Err("automation path rejected: path is empty".into());
    }
    // Checked as given: making `…\NUL` absolute gives the device path `\\.\NUL` on Windows.
    if cfg!(windows) && Path::new(path).components().any(|c| matches!(c, Component::Normal(n) if is_device_name(&n.to_string_lossy()))) {
        return Err(format!("automation path rejected: reserved device names are not allowed: {path}"));
    }
    let absolute = std::path::absolute(path).map_err(|e| format!("automation path rejected: cannot resolve {path}: {e}"))?;
    resolve_absolute(path, &absolute)
}

/// The directory entry `path` names: its folder resolved as [`resolve`] resolves it, then its own
/// name as given, so a link there is the entry itself, not what it leads to. That entry is what
/// replacing or removing the file changes.
fn resolve_entry(path: &str) -> Result<PathBuf, String> {
    let absolute = std::path::absolute(path).map_err(|e| format!("automation path rejected: cannot resolve {path}: {e}"))?;
    let (Some(folder), Some(name)) = (absolute.parent(), absolute.file_name()) else {
        return Err(format!("automation path rejected: not a file name: {path}"));
    };
    let mut entry = resolve_absolute(path, folder)?;
    entry.push(name);
    Ok(entry)
}

/// [`resolve`] for `absolute`, `path` made absolute (`path` as given, for messages).
fn resolve_absolute(path: &str, absolute: &Path) -> Result<PathBuf, String> {
    // The names below the nearest existing folder, last first.
    let mut missing: Vec<OsString> = vec![];
    let mut at = absolute;
    loop {
        match std::fs::canonicalize(at) {
            Ok(mut found) => {
                found.extend(missing.iter().rev());
                return Ok(found);
            }
            Err(e) => match std::fs::symlink_metadata(at) {
                Ok(m) if m.file_type().is_symlink() => return Err(format!("automation path rejected: a link that leads nowhere: {path}")),
                // There, yet its real path can't be had (no permission): nothing to compare.
                Ok(_) => return Err(format!("automation path rejected: cannot resolve {path}: {e}")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("automation path rejected: cannot resolve {path}: {e}")),
            },
        }
        let parent = at.parent();
        match (at.components().next_back(), parent) {
            (Some(Component::Normal(name)), Some(parent)) => {
                missing.push(name.to_os_string());
                at = parent;
            }
            (Some(Component::ParentDir), _) => {
                return Err(format!("automation path rejected: parent traversal below a folder that doesn't exist: {path}"));
            }
            // Down to the root (or drive) and it doesn't exist either.
            _ => {
                let e = std::io::Error::from(std::io::ErrorKind::NotFound);
                return Err(format!("automation path rejected: cannot resolve {path}: {e}"));
            }
        }
    }
}

/// Is `path` `root` or inside it, component by component? Both resolved, they are compared as the
/// file system spells them (a drive letter in either case); with `any_case`, without regard to
/// case on Windows, for spellings that are checked again once resolved and for folders kept out
/// of reach. A case-sensitive folder on Windows can hold `Data` and `data` both.
fn inside(path: &Path, root: &Path, any_case: bool) -> bool {
    let mut path = path.components();
    root.components().all(|r| path.next().is_some_and(|p| same_component(p, r, any_case)))
}

fn same_component(a: Component, b: Component, any_case: bool) -> bool {
    if a == b {
        return true;
    }
    let drive = matches!((a, b), (Component::Prefix(_), Component::Prefix(_)));
    if !cfg!(windows) || !(any_case || drive) {
        return false;
    }
    match (a.as_os_str().to_str(), b.as_os_str().to_str()) {
        (Some(a), Some(b)) => a.to_lowercase() == b.to_lowercase(),
        _ => false,
    }
}

/// A Windows device name (`CON`, `NUL`, `COM1`… also with an extension), which names no file.
fn is_device_name(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or(component).trim_end_matches(' ').to_ascii_uppercase();
    let numbered = |prefix: &str| stem.strip_prefix(prefix).is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"));
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$") || numbered("COM") || numbered("LPT")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder holding `in` (read root), `out` (write root) and `outside`.
    fn folders(name: &str) -> (PathBuf, Arc<AutomationRoots>) {
        let base = std::env::temp_dir().join(format!("wc-file-access-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for d in ["in/sub", "out", "outside", "in2"] {
            std::fs::create_dir_all(base.join(d)).unwrap();
        }
        std::fs::write(base.join("in/a.docx"), "x").unwrap();
        std::fs::write(base.join("outside/secret.docx"), "x").unwrap();
        let roots = AutomationRoots::new(Some(&base.join("in")), Some(&base.join("out"))).unwrap().unwrap();
        (base, roots)
    }

    fn s(p: PathBuf) -> String {
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn paths_inside_the_roots_pass_and_others_dont() {
        let (base, roots) = folders("inside");
        assert_eq!(roots.check(&s(base.join("in/a.docx")), Access::Read), Ok(()));
        assert_eq!(roots.check(&s(base.join("in")), Access::Read), Ok(()), "the root itself");
        assert_eq!(roots.check(&s(base.join("in/sub/new.docx")), Access::Read), Ok(()), "missing files are checked by their folder");
        assert_eq!(roots.check(&s(base.join("out/new/deeper/x.png")), Access::Write), Ok(()), "and by their nearest folder");
        let e = roots.check(&s(base.join("outside/secret.docx")), Access::Read).unwrap_err();
        assert!(e.starts_with("automation path rejected: outside the read root"), "{e}");
        // Read and write access are separate.
        let e = roots.check(&s(base.join("in/a.docx")), Access::Write).unwrap_err();
        assert!(e.contains("outside the write root"), "{e}");
        assert!(roots.check(&s(base.join("out/x.png")), Access::Read).unwrap_err().contains("outside the read root"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_sibling_with_the_same_prefix_is_outside() {
        let (base, roots) = folders("prefix");
        std::fs::write(base.join("in2/b.docx"), "x").unwrap();
        assert!(roots.check(&s(base.join("in2/b.docx")), Access::Read).is_err(), "in2 is not inside in");
        assert!(roots.check(&format!("{}2/c.docx", s(base.join("in"))), Access::Read).is_err());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn parent_traversal_is_resolved_or_refused() {
        let (base, roots) = folders("dotdot");
        let sep = std::path::MAIN_SEPARATOR;
        let up = format!("{}{sep}..{sep}outside{sep}secret.docx", s(base.join("in")));
        assert!(roots.check(&up, Access::Read).unwrap_err().contains("outside the read root"), "`..` out of the root");
        let back = format!("{}{sep}..{sep}a.docx", s(base.join("in/sub")));
        assert_eq!(roots.check(&back, Access::Read), Ok(()), "`..` that stays inside");
        let missing = format!("{}{sep}nope{sep}..{sep}..{sep}outside{sep}x.docx", s(base.join("out")));
        let e = roots.check(&missing, Access::Write).unwrap_err();
        // Windows resolves `..` by name first (so does opening the file there): then it's outside.
        assert!(e.contains("parent traversal below a folder that doesn't exist") || e.contains("outside the write root"), "{e}");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn relative_paths_resolve_against_the_working_directory() {
        let (base, roots) = folders("relative");
        // The tests' working directory is the crate's folder: not inside the roots.
        assert!(roots.check("a.docx", Access::Read).unwrap_err().contains("outside the read root"));
        assert!(roots.check("", Access::Read).unwrap_err().contains("path is empty"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_missing_root_is_an_error_and_none_is_no_confinement() {
        let (base, _) = folders("missing");
        let e = AutomationRoots::new(Some(&base.join("nope")), None).unwrap_err();
        assert!(e.starts_with("cannot open the automation read root"), "{e}");
        let e = AutomationRoots::new(None, Some(&base.join("in/a.docx"))).unwrap_err();
        assert!(
            e.starts_with("cannot open the automation write root")
                && e.ends_with(&std::io::Error::from(std::io::ErrorKind::NotADirectory).to_string()),
            "{e}"
        );
        assert!(AutomationRoots::new(Some(Path::new("")), None).unwrap_err().starts_with("cannot open the automation read root ``"));
        assert!(AutomationRoots::new(None, None).unwrap().is_none());
        // One root alone grants only its own access.
        let read_only = AutomationRoots::new(Some(&base.join("in")), None).unwrap().unwrap();
        let e = read_only.check(&s(base.join("in/x.docx")), Access::Write).unwrap_err();
        assert!(e.contains("write authority is absent"), "{e}");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn links_are_followed_before_comparing() {
        let (base, roots) = folders("links");
        #[cfg(unix)]
        let made = |target: &Path, link: &Path| std::os::unix::fs::symlink(target, link).is_ok();
        #[cfg(windows)]
        let made = |target: &Path, link: &Path| {
            let made =
                if target.is_dir() { std::os::windows::fs::symlink_dir(target, link) } else { std::os::windows::fs::symlink_file(target, link) };
            made.is_ok()
        };
        #[cfg(not(any(unix, windows)))]
        let made = |_: &Path, _: &Path| false;
        // Windows needs Developer Mode or elevation to make links: nothing to test without them.
        if !made(&base.join("outside/secret.docx"), &base.join("in/leak.docx")) {
            eprintln!("skipped: symbolic links can't be made here");
            let _ = std::fs::remove_dir_all(base);
            return;
        }
        assert!(roots.check(&s(base.join("in/leak.docx")), Access::Read).unwrap_err().contains("outside the read root"), "a file link out");
        assert!(made(&base.join("outside"), &base.join("out/away")));
        let e = roots.check(&s(base.join("out/away/new.png")), Access::Write).unwrap_err();
        assert!(e.contains("outside the write root"), "a folder link out: {e}");
        assert!(made(&base.join("outside/gone.docx"), &base.join("out/dangling.png")));
        let e = roots.check(&s(base.join("out/dangling.png")), Access::Write).unwrap_err();
        assert!(e.contains("a link that leads nowhere"), "writing would create the file outside: {e}");
        // A link to a folder inside the root stays inside.
        assert!(made(&base.join("in/sub"), &base.join("in/alias")));
        assert_eq!(roots.check(&s(base.join("in/alias/x.docx")), Access::Read), Ok(()));
        // A link outside the write root that leads into it: writing would replace the link,
        // outside (#3735). One inside that leads to a file inside is fine.
        std::fs::write(base.join("out/doc.docx"), "x").unwrap();
        assert!(made(&base.join("out/doc.docx"), &base.join("outside/alias.docx")));
        let e = roots.check(&s(base.join("outside/alias.docx")), Access::Write).unwrap_err();
        assert!(e.contains("outside the write root"), "the link itself is outside: {e}");
        assert!(made(&base.join("out/doc.docx"), &base.join("out/alias.docx")));
        assert_eq!(roots.check(&s(base.join("out/alias.docx")), Access::Write), Ok(()));
        // A folder link outside that leads into the root: the entry is inside, as the system opens it.
        assert!(made(&base.join("out"), &base.join("outside/to-out")));
        assert_eq!(roots.check(&s(base.join("outside/to-out/new.docx")), Access::Write), Ok(()));
        // A root given through a link is the folder it leads to.
        assert!(made(&base.join("in"), &base.join("in-link")));
        let linked = AutomationRoots::new(Some(&base.join("in-link")), None).unwrap().unwrap();
        assert_eq!(linked.check(&s(base.join("in/a.docx")), Access::Read), Ok(()));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn agents_paths_are_full_and_local_and_never_in_the_settings_folder() {
        let (base, _) = folders("agents");
        std::fs::create_dir_all(base.join("settings/recovery")).unwrap();
        std::fs::write(base.join("settings/control-key.local-7981"), "k").unwrap();
        let inside = s(base.join("in"));
        let roots = AutomationRoots::folders([(inside.as_str(), true)]);
        let any = AutomationRoots::everywhere();
        for r in [&roots, &any] {
            let e = r.check("in/a.docx", Access::Read).unwrap_err();
            assert!(e.contains("not a full path"), "an agent can't see the working directory: {e}");
            let e = r.check("//host/share/a.docx", Access::Read).unwrap_err();
            assert!(e.contains("on another computer"), "{e}");
        }
        // What a drive mapped to a share, or a link to one, resolves to on Windows.
        assert!(on_another_computer(Path::new(r"\\?\UNC\server\share\a.docx")));
        assert!(on_another_computer(Path::new(r"\\server\share\a.docx")));
        assert!(!on_another_computer(Path::new(r"\\?\C:\Users\a.docx")));
        assert!(!on_another_computer(Path::new("/home/a.docx")));
        assert_eq!(any.check(&s(base.join("outside/secret.docx")), Access::Read), Ok(()), "any file");
        assert_eq!(any.check(&s(base.join("outside/new.pdf")), Access::Write), Ok(()));
        // The settings folder hold the window's key: out of reach whatever an agent may use, also
        // through a folder around it or a link into it.
        protect(&base.join("settings"));
        let key = s(base.join("settings/control-key.local-7981"));
        let around = AutomationRoots::folders([(s(base.clone()).as_str(), true)]);
        for r in [&any, &around] {
            let e = r.check(&key, Access::Read).unwrap_err();
            assert!(e.contains("settings folder is out of reach"), "{e}");
            assert!(r.check(&s(base.join("settings/recovery/new.docx")), Access::Write).is_err());
            assert_eq!(r.check(&s(base.join("in/a.docx")), Access::Read), Ok(()), "the rest stays in reach");
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(base.join("settings"), base.join("in/link")).unwrap();
            let e = roots.check(&s(base.join("in/link/control-key.local-7981")), Access::Read).unwrap_err();
            assert!(e.contains("out of reach"), "{e}");
        }
        // Withdrawn while a request runs: no path from then on.
        let withdrawn = Arc::new(AtomicBool::new(false));
        let held = roots.until(withdrawn.clone());
        assert_eq!(held.check(&s(base.join("in/a.docx")), Access::Read), Ok(()));
        withdrawn.store(true, Ordering::Release);
        let e = held.check(&s(base.join("in/a.docx")), Access::Read).unwrap_err();
        assert!(e.contains("changed while this request ran"), "{e}");
        assert_eq!(roots.check(&s(base.join("in/a.docx")), Access::Read), Ok(()), "the roots it came from are as they were");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn several_folders_each_grant_their_own_access() {
        let (base, _) = folders("several");
        std::fs::create_dir_all(base.join("both/deep")).unwrap();
        let (read_only, both, gone) = (s(base.join("in")), s(base.join("both")), s(base.join("gone")));
        let roots = AutomationRoots::folders([(read_only.as_str(), false), (both.as_str(), true), (gone.as_str(), true)]);
        assert_eq!(roots.check(&s(base.join("in/a.docx")), Access::Read), Ok(()));
        let e = roots.check(&s(base.join("in/a.docx")), Access::Write).unwrap_err();
        assert!(e.starts_with("automation path rejected: outside the write root "), "a read-only folder isn't written: {e}");
        assert_eq!(roots.check(&s(base.join("both/deep/new.pdf")), Access::Write), Ok(()));
        assert_eq!(roots.check(&s(base.join("both/deep/new.pdf")), Access::Read), Ok(()), "a folder written is read too");
        let e = roots.check(&s(base.join("outside/secret.docx")), Access::Read).unwrap_err();
        assert!(e.starts_with("automation path rejected: outside the read roots "), "{e}");
        let sep = std::path::MAIN_SEPARATOR;
        let up = format!("{both}{sep}..{sep}outside{sep}secret.docx");
        assert!(roots.check(&up, Access::Read).is_err() && roots.check(&up, Access::Write).is_err(), "`..` out of a folder");
        // A folder that isn't there is left out, also once it is made.
        std::fs::create_dir_all(base.join("gone")).unwrap();
        assert!(roots.check(&s(base.join("gone/x.pdf")), Access::Write).is_err());
        // No folder at all, or none that opens, reaches nothing: never everything.
        for nothing in
            [AutomationRoots::folders([]), AutomationRoots::folders([(s(base.join("nope")).as_str(), true), ("", true), ("relative", false)])]
        {
            let e = nothing.check(&s(base.join("in/a.docx")), Access::Read).unwrap_err();
            assert!(e.contains("read authority is absent"), "{e}");
            assert!(nothing.check(&s(base.join("both/x.pdf")), Access::Write).unwrap_err().contains("write authority is absent"));
        }
        // Many folders: the message names a few and counts the rest.
        let many: Vec<String> = (0..5).map(|i| s(base.join(format!("many{i}")))).collect();
        for m in &many {
            std::fs::create_dir_all(m).unwrap();
        }
        let roots5 = AutomationRoots::folders(many.iter().map(|m| (m.as_str(), false)));
        let e = roots5.check(&s(base.join("outside/secret.docx")), Access::Read).unwrap_err();
        assert!(e.contains(", 2 more: "), "{e}");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(base.join("outside"), base.join("both/away")).unwrap();
            let e = roots.check(&s(base.join("both/away/secret.docx")), Access::Read).unwrap_err();
            assert!(e.contains("outside the read roots"), "a link out of a folder: {e}");
            assert!(roots.check(&s(base.join("both/away/new.pdf")), Access::Write).is_err());
        }
        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(windows)]
    #[test]
    fn windows_ignores_case_and_refuses_devices() {
        let (base, roots) = folders("case");
        let upper = s(base.join("IN/A.SVG"));
        assert_eq!(roots.check(&upper, Access::Read), Ok(()), "{upper}");
        assert_eq!(roots.check(&s(base.join("In/Sub/New.docx")), Access::Read), Ok(()));
        let e = roots.check(&s(base.join("out/NUL")), Access::Write).unwrap_err();
        assert!(e.contains("reserved device names"), "{e}");
        assert!(roots.check(&s(base.join("out/com1.txt")), Access::Write).is_err());
        assert_eq!(roots.check(&s(base.join("out/console.txt")), Access::Write), Ok(()));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn scopes_nest_and_stay_on_their_thread() {
        let (base, roots) = folders("scopes");
        let outside = s(base.join("outside/secret.docx"));
        assert_eq!(check_read(&outside), Ok(()), "nothing is confined by default");
        confine(Some(&roots), || {
            assert!(check_read(&outside).is_err());
            assert_eq!(unconfined(|| check_read(&outside)), Ok(()));
            assert!(check_read(&outside).is_err(), "restored after unconfined");
            let o = outside.clone();
            let there = std::thread::spawn(move || check_read(&o).is_ok()).join().unwrap();
            assert!(there, "another thread is not confined by this thread's scope");
        });
        assert_eq!(check_read(&outside), Ok(()), "restored after confine");
        let r = std::panic::catch_unwind(|| confine(Some(&roots), || panic!("boom")));
        assert!(r.is_err());
        assert_eq!(check_read(&outside), Ok(()), "restored after a panic");
        assert_eq!(confine(None, || check_read(&outside)), Ok(()));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn device_names() {
        for d in ["CON", "nul", "Com1", "LPT9.txt", "aux.docx", "NUL "] {
            assert!(is_device_name(d), "{d}");
        }
        for f in ["console", "com10", "COM0", "lpt", "a.docx", "nul-x"] {
            assert!(!is_device_name(f), "{f}");
        }
    }

    /// Every file a command reads or writes by path goes through `io` (`open_path_with`,
    /// `save_path_with`, `read_file`, `write_file`), which check the roots. Engine code that
    /// reaches for `std::fs` itself would bypass them, so none may: every file under `src/`
    /// (commands, `print.rs`, `session.rs`…) except `io.rs` with its `io/` helpers (`safe_save`,
    /// reached only after `io` checked the roots), this module, and the test files.
    /// The host-enabled recovery store owns its profile files; commands address known snapshot
    /// IDs, never paths. Only that storage module is exempt, not recovery commands in general.
    #[test]
    fn commands_do_not_touch_the_file_system_themselves() {
        fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, files);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    files.push(path);
                }
            }
        }
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = vec![];
        walk(&src, &mut files);
        assert!(files.iter().any(|f| f.ends_with("print.rs")) && files.iter().any(|f| f.ends_with("cmd/file.rs")), "{files:?}");
        let mut offenders = vec![];
        for path in files {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // `io` and this module are the checked gateway; `tests*.rs` files and `tests/`
            // directories are test-only modules.
            if path.parent() == Some(src.as_path()) && (name == "io.rs" || name == "file_access.rs")
                || name.starts_with("tests")
                || path.strip_prefix(&src).is_ok_and(|rel| rel.components().any(|c| c.as_os_str() == "tests"))
                || path.starts_with(src.join("io"))
                || path == src.join("recovery/desktop.rs")
            {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            // Inline test modules (which build their own fixtures) come last in each file. A
            // `#[cfg(test)]` on a `use` line, a `mod tests;` declaration or another item doesn't end
            // the code.
            let end = text
                .match_indices("#[cfg(test)]")
                .find(|&(i, a)| {
                    let first = text[i + a.len()..].trim_start().lines().next().unwrap_or("");
                    first.starts_with("mod ") && first.trim_end().ends_with('{')
                })
                .map_or(text.len(), |(i, _)| i);
            let code = &text[..end];
            for (n, line) in code.lines().enumerate() {
                let line = line.trim_start();
                let touches = ["std::fs", "File::open", "File::create", "OpenOptions"].iter().any(|w| line.contains(w));
                if !line.starts_with("//") && touches {
                    offenders.push(format!("{}:{}: {line}", path.display(), n + 1));
                }
            }
        }
        assert!(offenders.is_empty(), "use crate::io::{{read_file, write_file, open_path_with, save_path_with}}: {offenders:#?}");
    }
}

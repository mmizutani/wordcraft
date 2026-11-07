//! Which files the agents in a shared document can use (#31): File › Options › Agents, "Files
//! agents can use" ([`crate::Prefs::agent_files`], [`crate::Prefs::agent_folders`]), the most any
//! agent gets, and an invite's own choice for its agent (`room.invite {files}`), which may be
//! less. The room the window shares holds the setting ([`crate::room::Room::set_agent_files`]);
//! each request of a participant who joined with a key runs within what it allows them
//! ([`crate::room::Room::reach`]).

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};

use crate::file_access::AutomationRoots;

/// Most folders agents can be given.
pub const MAX_AGENT_FOLDERS: usize = 32;
/// Longest folder path taken, in characters (as File › Options' other folders).
pub const MAX_FOLDER_CHARS: usize = 4096;

/// Which files agents' commands can use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentFiles {
    /// None: their commands read and write no file (the default).
    #[default]
    None,
    /// Only the folders of the setting ([`AgentFolder`]), each read-only or read and write.
    Folders,
    /// Any file the person can use.
    All,
}

impl AgentFiles {
    /// Every choice, in the order File › Options › Agents lists them.
    pub const ALL: [AgentFiles; 3] = [AgentFiles::None, AgentFiles::Folders, AgentFiles::All];

    /// The choice's name: `none`, `folders` or `all`.
    pub fn name(self) -> &'static str {
        match self {
            AgentFiles::None => "none",
            AgentFiles::Folders => "folders",
            AgentFiles::All => "all",
        }
    }

    /// The choice called `name` ([`AgentFiles::name`]).
    pub fn named(name: &str) -> Option<AgentFiles> {
        AgentFiles::ALL.into_iter().find(|f| f.name() == name)
    }

    /// This choice, or `most` when that gives fewer files: what narrowing the setting to `most`
    /// leaves an invite that chose this.
    pub fn at_most(self, most: AgentFiles) -> AgentFiles {
        let rank = |f: AgentFiles| AgentFiles::ALL.iter().position(|a| *a == f).unwrap_or(0);
        if rank(self) <= rank(most) { self } else { most }
    }
}

/// A folder agents can use, and whether they can write in it too.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentFolder {
    /// The folder: a full path, its links followed when it was chosen.
    pub path: String,
    /// They can write in it too (an exported PDF, say); otherwise they only read from it.
    #[serde(default)]
    pub write: bool,
}

/// Where a participant's commands can reach: no file, some folders, or any file on this
/// computer (not by a network path); never WordCraft's settings folder ([`crate::file_access::protect`]),
/// and always by a full path.
#[derive(Clone, Debug)]
pub struct Reach(Arc<AutomationRoots>);

impl Reach {
    /// What `files` gives, with the setting's `folders`. Folders that can't be opened any more
    /// are left out, and with none left nothing is in reach: never every file.
    pub fn of(files: AgentFiles, folders: &[AgentFolder]) -> Reach {
        Reach(match files {
            AgentFiles::None => AutomationRoots::none(),
            AgentFiles::Folders => AutomationRoots::folders(folders.iter().map(|f| (f.path.as_str(), f.write))),
            AgentFiles::All => AutomationRoots::everywhere(),
        })
    }

    /// This reach until `withdrawn` is set, and no file from then on
    /// ([`AutomationRoots::until`]).
    pub fn until(self, withdrawn: Arc<AtomicBool>) -> Reach {
        Reach(self.0.until(withdrawn))
    }

    /// Run `f` within this reach, on this thread ([`crate::file_access::confine`]).
    pub fn confine<T>(&self, f: impl FnOnce() -> T) -> T {
        crate::file_access::confine(Some(&self.0), f)
    }
}

/// Why `path` can't be a folder agents use, judged by its text alone: empty, too long, with
/// control characters, not a full path, or on another machine (checking that a network folder
/// exists would make Windows authenticate there, #2852).
pub(crate) fn unusable(path: &str) -> Option<&'static str> {
    if path.is_empty() {
        Some("it is empty")
    } else if path.chars().count() > MAX_FOLDER_CHARS || path.chars().any(char::is_control) {
        Some("it isn't a folder path")
    } else if crate::cmd::templates::is_network_path(path) {
        Some("it is a network folder")
    } else if !std::path::Path::new(path).is_absolute() {
        Some("it isn't a full path")
    } else {
        None
    }
}

/// Saved folders as they load, without looking at the file system: those [`unusable`] refuses are
/// dropped, a folder listed again too (the first stays), and the list ends at
/// [`MAX_AGENT_FOLDERS`].
pub(crate) fn cleaned(folders: Vec<AgentFolder>) -> Vec<AgentFolder> {
    let mut kept: Vec<AgentFolder> = Vec::new();
    for f in folders {
        if kept.len() >= MAX_AGENT_FOLDERS {
            break;
        }
        if unusable(&f.path).is_none() && !kept.iter().any(|k| k.path == f.path) {
            kept.push(f);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_have_names() {
        for f in AgentFiles::ALL {
            assert_eq!(AgentFiles::named(f.name()), Some(f));
            assert_eq!(serde_json::to_value(f).unwrap(), f.name());
        }
        assert_eq!(AgentFiles::named("All"), None);
        assert_eq!(AgentFiles::default(), AgentFiles::None);
    }

    #[test]
    fn narrowing_leaves_a_choice_no_wider() {
        use AgentFiles::{All, Folders, None};
        assert_eq!(All.at_most(None), None);
        assert_eq!(All.at_most(Folders), Folders);
        assert_eq!(Folders.at_most(None), None);
        assert_eq!(Folders.at_most(All), Folders);
        assert_eq!(None.at_most(All), None);
    }

    #[test]
    fn saved_folders_are_cleaned_without_looking_at_them() {
        let root = if cfg!(windows) { r"C:\work" } else { "/work" };
        let f = |path: &str, write| AgentFolder { path: path.into(), write };
        let long = format!("{root}{}", "x".repeat(MAX_FOLDER_CHARS));
        let saved = vec![
            f(root, false),
            f("relative", true),
            f("", true),
            f(&long, true),
            f(&format!("{root}\u{7}"), true),
            f(r"\\host\share", true),
            f("//host/share", true),
            f(root, true),
        ];
        assert_eq!(cleaned(saved), vec![f(root, false)], "the first of a folder listed twice stays");
        let many: Vec<AgentFolder> = (0..MAX_AGENT_FOLDERS + 5).map(|i| f(&format!("{root}{i}"), false)).collect();
        assert_eq!(cleaned(many).len(), MAX_AGENT_FOLDERS);
    }
}

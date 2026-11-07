//! A participant's role in a shared document ([`crate::room::Role`]), held to by every command
//! the session runs ([`Session::role`]).

use serde_json::Value;
use wordcraft_doc::Document;

use super::{CmdError, Session};
use crate::room::Role;

/// The command no participant's copy runs, whatever their role ([`Session::confined_refuses`]).
const OPTIONS: &str = "file.options";

/// Why not.
const OPTIONS_ARE_THE_WINDOW_S: &str = "the options, which files agents can use among them, are set at the window";

/// What a suggester's command forced on: the document's Track Changes setting and the Track
/// Formatting preference as they were.
pub(super) type Forced = Option<(bool, bool)>;

impl Session {
    /// Why the participant's role refuses a command before it runs: a viewer changes nothing.
    /// The other roles' commands are held to their role once they ran ([`Session::hold_to_role`]).
    pub(super) fn refused_by_role(&self, mutates: bool) -> Option<&'static str> {
        (mutates && self.role == Some(Role::Viewer)).then_some("a viewer changes nothing")
    }

    /// Why a participant's copy (`confined`) doesn't run command `id`, inside other
    /// commands too (a macro's steps, Repeat): its role's scopes don't allow it
    /// ([`Role::refuses_command`]). No files but exports, other documents or windows, and the
    /// application's settings only for an owner, but never File › Options, which says which
    /// files agents can use: that is the person's, at the window.
    pub(crate) fn confined_refuses(&self, id: &str) -> Option<String> {
        if !self.confined {
            return None;
        }
        if id == OPTIONS {
            return Some(OPTIONS_ARE_THE_WINDOW_S.to_string());
        }
        self.role.unwrap_or(Role::Owner).refuses_command(id)
    }

    /// Where a participant's copy (`confined`) may export to: a new file in a folder it may write.
    /// It never replaces a file there, as a viewer's role says they change nothing; outside
    /// those folders the file system isn't asked whether there's one.
    pub(crate) fn exports_to_new_file(&self, path: &str) -> Result<(), CmdError> {
        if !self.confined {
            return Ok(());
        }
        crate::file_access::check_write(path).map_err(CmdError::Failed)?;
        if crate::io::entry_exists(std::path::Path::new(path)) {
            return Err(CmdError::Failed(crate::io::not_new(path)));
        }
        Ok(())
    }

    /// Write an export's `bytes` to `path`, once it is made. From a participant's copy, as a new
    /// file only, created so that nothing put there meanwhile (another participant's export of
    /// the same name, say) is replaced, and only if the copy may still write there: the files it
    /// can use may have narrowed while the export was made ([`crate::io::write_new_file`]).
    /// Otherwise as any file is written.
    pub(crate) fn write_export(&self, path: &str, bytes: &[u8]) -> Result<(), CmdError> {
        let written = if self.confined { crate::io::write_new_file(path, bytes) } else { crate::io::write_file(path, bytes) };
        written.map_err(CmdError::Failed)
    }

    /// Whether a command is held to the participant's role as a whole: an edit (`mutates`), and
    /// a pure command run by a participant below editor outside other commands, since Repeat
    /// and a macro change the document through the commands they run, which aren't held to it
    /// on their own, and Undo and Redo through the steps they put back.
    pub(super) fn held_to_role(&self, mutates: bool) -> bool {
        mutates || (self.role.is_some_and(|r| r < Role::Editor) && self.outside_commands())
    }

    /// A suggester's edits are tracked changes in their name, whatever the document's setting:
    /// Track Changes, formatting included, is on for their commands alone.
    pub(super) fn force_tracking(&mut self, mutates: bool) -> Forced {
        let tracking = self.doc.settings.track_changes && self.prefs.markup.track_formatting;
        if !mutates || self.role != Some(Role::Suggester) || tracking {
            return None;
        }
        let was = (self.doc.settings.track_changes, self.prefs.markup.track_formatting);
        self.doc.settings.track_changes = true;
        self.prefs.markup.track_formatting = true;
        Some(was)
    }

    /// After command `id` ran on document `home` (`before` it, when it `mutates`, or is a pure
    /// one a role below editor is held to): in a shared document the whole command (not each
    /// command it ran) is held to the participant's role, a pure one's too, since Repeat, a
    /// macro, Undo and Redo change the document through the commands they run or the steps
    /// they put back. The formatting an edit changed becomes formatting revisions now, while
    /// their tracking is on, for the check to see them (a pure command's were, by the commands
    /// it ran), and so does that of an edit run inside another one with tracking forced on for
    /// it, before that goes back: the outer command's check sees it as the suggestion it is.
    /// Tracking forced on goes back. A command that did more than the role lets them gets an
    /// error in `result`, and is undone like any failed one. Whether its formatting revisions
    /// are recorded.
    pub(super) fn hold_to_role(
        &mut self,
        id: &str,
        home: u64,
        mutates: bool,
        forced: Forced,
        before: Option<&Document>,
        result: &mut Result<Value, CmdError>,
    ) -> bool {
        let here = self.document_id == home;
        let checked = self.role.is_some() && self.outside_commands() && here;
        let recorded = (checked || (forced.is_some() && here)) && mutates && result.is_ok() && before.is_some();
        if recorded && let Some(before) = before {
            crate::cmd::fmt_revisions::record(self, id, before);
        }
        if let Some(was) = forced {
            self.unforce_tracking(id, home, was, result);
        }
        if checked
            && result.is_ok()
            && let (Some(role), Some(before)) = (self.role, before)
            && let Err(why) = crate::room::rules::check(role, &self.author, before, &self.doc)
        {
            *result = Err(CmdError::Disabled(format!("{id}: {why}")));
        }
        recorded
    }

    /// Put back what [`Session::force_tracking`] turned on. A command that turned Track Changes
    /// off is refused: it stays on for a suggester.
    fn unforce_tracking(&mut self, id: &str, home: u64, (track, formatting): (bool, bool), result: &mut Result<Value, CmdError>) {
        if self.document_id == home {
            if self.doc.settings.track_changes {
                self.doc.settings.track_changes = track;
            } else if result.is_ok() {
                *result = Err(CmdError::Disabled(format!("{id}: Track Changes stays on for a suggester")));
            }
        } else if let Some(d) = self.others.iter_mut().find(|d| d.document_id == home) {
            d.doc.settings.track_changes = track;
        }
        // Track Formatting is the participant's own setting: they may turn it off (their
        // formatting is tracked whatever it says).
        if self.prefs.markup.track_formatting {
            self.prefs.markup.track_formatting = formatting;
        }
    }
}

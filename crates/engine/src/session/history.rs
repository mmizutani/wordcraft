use super::doc_state::{MAX_UNDO, Selection, UNDO_LIST_MAX, Undo, next_math_session};
use super::{Arc, Document, Session};

impl Session {
    /// Snapshot for undo before a change. Returns whether it took a step: typing that is
    /// already in an open step is not a new one.
    pub fn checkpoint(&mut self, label: &str) -> bool {
        if label == "Typing" && self.typing_open {
            return false;
        }
        self.typing_open = label == "Typing";
        self.history.push(Arc::new(Undo {
            label: label.to_string(),
            doc: self.doc.clone(),
            sel: self.sel.clone(),
            pending: self.pending.clone(),
            column: self.column.clone(),
            theirs: None,
        }));
        if self.history.len() > MAX_UNDO {
            self.history.remove(0);
            self.undo_evicted += 1;
        }
        self.redo.clear();
        // The newest step is what a joined command merges into.
        self.join_anchor = true;
        true
    }

    /// Record `doc`/`sel` as their own undo step after the fact and close the typing group, so
    /// Undo right after an automatic change (AutoFormat, AutoCorrect) reverts only that change.
    pub fn push_undo(&mut self, label: &str, doc: Document, sel: Selection) {
        self.history.push(Arc::new(Undo { label: label.to_string(), doc, sel, pending: self.pending.clone(), column: None, theirs: None }));
        if self.history.len() > MAX_UNDO {
            self.history.remove(0);
            self.undo_evicted += 1;
        }
        self.typing_open = false;
        self.join_anchor = true;
    }

    /// Make the next mutating command part of the previous undo step instead of a new one, so a
    /// drag that runs a command every frame is a single Undo. Call it on every frame of the drag
    /// except the first; it is consumed by the next `run`.
    pub fn join_next_undo(&mut self) {
        self.join_next = true;
    }

    /// Make the next equation typing command (`equation.type` or `equation.backspace`) join the
    /// previous equation deletion's undo step. Unlike [`Self::join_next_undo`], the promise is
    /// not generic: any other command consumes and discards it, so unrelated edits get their own
    /// checkpoint. A deletion-only IME request therefore cannot leak a join into the next edit.
    pub fn join_next_equation_edit(&mut self) {
        self.equation_join_next = true;
    }

    /// Discard an equation replacement join that received no replacement (for example an empty
    /// commit), so it cannot join a later composition's typing.
    pub fn clear_equation_join(&mut self) {
        self.equation_join_next = false;
    }

    /// Make the next `text.insert` at the current selection join a successful body
    /// `DeleteSurrounding`'s undo step. Unlike [`Self::join_next_undo`], the promise is scoped to
    /// that exact replacement: any other command consumes and discards it, so an intervening
    /// ordinary edit gets its own checkpoint.
    pub fn join_next_ime_text_replacement(&mut self) {
        self.ime_text_join = Some(self.sel.clone());
    }

    /// Discard a body replacement join that received no replacement (an empty or refused
    /// commit, a new request or a refusal), so it cannot join a later composition.
    pub fn clear_ime_text_join(&mut self) {
        self.ime_text_join = None;
    }

    /// Discard every pending IME replacement join. A refused surrounding request supersedes the
    /// composition it interrupts, including any promise from an earlier accepted request.
    pub fn clear_ime_joins(&mut self) {
        self.equation_join_next = false;
        self.ime_text_join = None;
    }

    /// Stamp the document's save metadata (modified time, last modified by, revision; created
    /// when it has none) the way Word does on every save. `file.save` calls it, and so does any
    /// front end that writes the bytes itself (the web build's download, #262). Like Word, the
    /// document also keeps the zoom it is shown at (#995).
    pub fn stamp_save(&mut self) {
        self.doc.settings.zoom = crate::cmd::view::saved_zoom(&self.view);
        let core = &mut self.doc.core;
        core.modified = crate::cmd::now_iso();
        if core.created.is_empty() {
            core.created = core.modified.clone();
        }
        core.last_modified_by = self.author.clone();
        core.revision = core.revision.saturating_add(1);
        // Saving is no edit (no undo step), but copies of the document follow the metadata too.
        if self.outside_commands() {
            self.record_changes();
        }
    }

    /// Whether typing is still collecting into one undo step (no other command since).
    pub fn typing_open(&self) -> bool {
        self.typing_open
    }

    /// Reopen the typing group when the last undo step is typing, so the keys typed next join
    /// it: after an input method's rewrite was joined to it (`join_next_undo` closes the group).
    /// Not after other participants changed the shared document since: what is typed then is a
    /// step of its own, which leaves their changes alone.
    pub fn continue_typing(&mut self) {
        if self.undo_label() == Some("Typing") && self.history.last().is_some_and(|u| u.theirs.is_none()) {
            self.typing_open = true;
        }
    }

    /// Close an open typing group (caret moved, other command).
    pub fn close_typing(&mut self) {
        self.typing_open = false;
    }

    pub fn can_undo(&self) -> bool {
        !self.history.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.history.last().map(|u| u.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|u| u.label.as_str())
    }

    /// Undo steps pushed so far, evicted ones included: only grows while steps are added.
    pub(crate) fn undo_steps_so_far(&self) -> u64 {
        self.history.len() as u64 + self.undo_evicted
    }

    /// The number of undo steps.
    pub fn undo_depth(&self) -> usize {
        self.history.len()
    }

    /// Where the undo history stands, for [`Session::merge_undo_since`].
    pub fn undo_mark(&self) -> u64 {
        (self.history.len() as u64).saturating_add(self.undo_evicted)
    }

    /// Makes the steps recorded since `mark` one undo step, the first: a drag that applied its
    /// value as it went (a slider in a pane) undoes in one go. In a shared document, steps with
    /// other participants' changes between them stay apart: one step made of them would take
    /// those changes back too.
    pub fn merge_undo_since(&mut self, mark: u64) {
        let keep = usize::try_from(mark.saturating_sub(self.undo_evicted).saturating_add(1)).unwrap_or(usize::MAX);
        let newest = self.history.len().saturating_sub(1);
        let apart = self.history.get(..newest).and_then(|h| h.iter().rposition(|u| u.theirs.is_some())).map_or(0, |i| i.saturating_add(2));
        let keep = keep.max(apart);
        if keep >= self.history.len() {
            return;
        }
        // The step kept is the merged one: the changes others made after the last of them come
        // after it.
        let theirs = self.history.last().and_then(|u| u.theirs.clone());
        self.history.truncate(keep);
        if let Some(u) = self.history.last_mut() {
            Arc::make_mut(u).theirs = theirs;
        }
    }

    pub fn undo_labels(&self) -> Vec<String> {
        self.history.iter().rev().map(|u| u.label.clone()).collect()
    }

    /// The Undo button's list (#701): the undo steps' labels (English, as commands name them),
    /// newest first, at most [`UNDO_LIST_MAX`].
    pub fn undo_history(&self) -> Vec<&str> {
        self.history.iter().rev().take(UNDO_LIST_MAX).map(|u| u.label.as_str()).collect()
    }

    /// Undo the newest `count` steps in one go, as picking an entry in the Undo list does (#701).
    /// More than the undo depth undoes everything; a step that would take the document's
    /// protection away stops it (only Stop Protection lifts it), and so does one that a change
    /// another participant made since keeps ([`Session::undo_step`]). Returns the steps undone.
    pub fn undo_steps(&mut self, count: usize) -> usize {
        self.undo_steps_or_why(count).0
    }

    /// [`Session::undo_steps`], and why it stopped when a change another participant made since
    /// kept a step.
    pub fn undo_steps_or_why(&mut self, count: usize) -> (usize, Option<String>) {
        let mut done = 0;
        while done < count && !self.step_lifts_protection(false) {
            match self.undo_step() {
                Ok(true) => done += 1,
                Ok(false) => break,
                Err(why) => return (done, Some(why)),
            }
        }
        (done, None)
    }

    /// Undoes the newest step: closes typing, records an AutoCorrect exception when the undone step
    /// was its automatic capital, swaps the document, selection, pending formatting and column with
    /// the redo snapshot, replaces the body identity, refreshes equation editing and touches layout.
    /// In a shared document, the changes others made since stay ([`Session::undo_step`]).
    pub fn undo(&mut self) -> bool {
        self.undo_step().unwrap_or(false)
    }

    /// [`Session::undo`], or why it can't: in a shared document, a change another participant
    /// made since the step changed what the step changed (the same characters, blocks, part or
    /// document field). The step and the document then stay as they are.
    pub fn undo_step(&mut self) -> Result<bool, String> {
        self.typing_open = false;
        let moved = self.past_theirs(false)?;
        // Undoing the automatic capitalisation's own step makes its word an exception; any
        // other Undo forgets the capital.
        let steps = self.history.len() as u64 + self.undo_evicted;
        if let Some(c) = self.autocap.take().filter(|c| c.depth == steps) {
            c.add_exceptions(&mut self.prefs.autocorrect);
        }
        let Some(u) = self.history.pop().map(Arc::unwrap_or_clone) else { return Ok(false) };
        let cur = self.put_step(u, moved, false);
        self.redo.push(Arc::new(cur));
        // Undo swaps the whole body back: a composition belongs to the text it showed.
        self.note_body_replaced();
        self.refresh_math_session();
        self.touch();
        Ok(true)
    }

    /// Would Undo (or Redo) take the document's protection away? Only Stop Protection lifts it.
    pub fn step_lifts_protection(&self, redo: bool) -> bool {
        let next = if redo { self.redo.last() } else { self.history.last() };
        next.is_some_and(|u| {
            // Past changes others made since, the step puts the document's settings back only
            // when it changed them itself.
            let its_own = u.theirs.as_ref().is_none_or(|t| t.base.settings != u.doc.settings);
            its_own && crate::cmd::protect::protection_lifted(&self.doc, &u.doc)
        })
    }

    /// Redoes the newest undone step by swapping the snapshots back, replacing the body identity,
    /// refreshing equation editing and touching layout. In a shared document, the changes
    /// others made since stay ([`Session::redo_step`]).
    pub fn redo(&mut self) -> bool {
        self.redo_step().unwrap_or(false)
    }

    /// [`Session::redo`], or why it can't: a change another participant made since the step was
    /// undone changed what it changes ([`Session::undo_step`]).
    pub fn redo_step(&mut self) -> Result<bool, String> {
        self.typing_open = false;
        let moved = self.past_theirs(true)?;
        let Some(u) = self.redo.pop().map(Arc::unwrap_or_clone) else { return Ok(false) };
        let cur = self.put_step(u, moved, true);
        self.history.push(Arc::new(cur));
        self.note_body_replaced();
        self.refresh_math_session();
        self.touch();
        Ok(true)
    }

    /// An equation's editing session cannot ride across Undo/Redo: the equation tree, or the
    /// object at that offset, may be a different one now. A new session token starts a new IME
    /// context; if tokens are exhausted, the equation is left (fail closed).
    fn refresh_math_session(&mut self) {
        if self.math.is_none() {
            return;
        }
        match next_math_session() {
            Some(session) => {
                if let Some(m) = self.math.as_mut() {
                    m.session = session;
                }
            }
            None => self.math = None,
        }
    }

    /// Drop history (after open / new).
    pub fn reset_history(&mut self) {
        self.autocap = None;
        self.history.clear();
        self.redo.clear();
        self.typing_open = false;
        self.join_anchor = false;
    }

    /// The version snapshots belong to the document they were taken from: keeping the old ones
    /// would let a restore put the previous document's content into this one (#1327 follow-up).
    pub fn clear_versions(&mut self) {
        self.versions.clear();
    }
}

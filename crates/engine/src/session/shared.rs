//! A document shared with other copies of it ([`crate::room`]): their changes are made in the
//! open document as they come, and the caret, the selection and the other places the session
//! holds move with the text they are in.

use wordcraft_doc::change::Mapped;
use wordcraft_doc::{ChangeSet, Document, Pos};

use super::doc_state::{Selection, TypedWord};
use super::{MathEdit, Session};

/// Where place `p` of `before` is in `now`: mapped through `cs`, the change that made `now` from
/// `before`; or, with no change, in the same paragraph (by id) at the same offset. `None` when
/// its text is gone.
fn moved(before: &Document, now: &Document, cs: Option<&ChangeSet>, p: &Pos) -> Option<Pos> {
    let a = before.anchor_at(p)?;
    let m = match cs {
        Some(cs) => cs.map(a),
        None => Mapped::At(a),
    };
    now.place(m, Some(p))
}

impl Session {
    /// Make change `cs`, which another copy of the active document made (another participant
    /// of its room), in this one. The places the session holds move with the text they are in:
    /// the caret and the selection, the equation being edited, the body selection a header
    /// keeps, the word being typed and Go Back's places. Pictures selected beside the
    /// selection and a column selection are let go. Followers of the document see the change
    /// ([`Session::changes`]).
    ///
    /// The undo history is dropped for now: its steps are whole documents from before the
    /// change, and putting one back would take the change away again. Undo per participant
    /// comes with milestone A1.4.
    ///
    /// An error, leaving the session as it was, when the change doesn't apply
    /// ([`Document::apply`]).
    pub fn apply_change(&mut self, cs: &ChangeSet) -> wordcraft_doc::Result<()> {
        let mut doc = self.doc.clone();
        doc.apply(cs)?;
        let before = std::mem::replace(&mut self.doc, doc);
        self.follow_shared(&before, Some(cs));
        Ok(())
    }

    /// Replace the active document with `doc`, another version of the same shared document: what
    /// a room's copy holds when it drops edits that clash with another participant's, before it
    /// makes them again, or when it reads the document again. Unlike
    /// [`Session::set_document`], it stays the same document (its id, file, name and saved
    /// versions); the places the session holds stay in the same paragraphs (by id) where those
    /// are still there. Followers of the document see the difference as one change.
    pub fn reset_shared(&mut self, doc: Document) {
        let before = std::mem::replace(&mut self.doc, doc);
        self.doc.ensure_nonempty();
        // An input method's composition doesn't survive its text being replaced.
        self.note_body_replaced();
        self.follow_shared(&before, None);
    }

    /// Make document `id` the active one again after a command made another one active, and
    /// close the documents other than `id` (a room's copy holds one document). False, changing
    /// nothing, when `id` is not open.
    pub(crate) fn return_to(&mut self, id: u64) -> bool {
        if self.document_id == id {
            return true;
        }
        if self.switch_to(id).is_err() {
            return false;
        }
        let others: Vec<u64> = self.order.iter().copied().filter(|d| *d != id).collect();
        for d in others {
            let _ = self.close_document(d);
        }
        true
    }

    /// After `self.doc` became what change `cs` (or, with none, another version) made of
    /// `before`: move what the session holds, and record the change.
    fn follow_shared(&mut self, before: &Document, cs: Option<&ChangeSet>) {
        let now = &self.doc;
        let place = |p: &Pos| moved(before, now, cs, p).unwrap_or_else(|| now.clamp(p));
        let sel = Selection { anchor: place(&self.sel.anchor), focus: place(&self.sel.focus) };
        let hf_return = self.hf_return.as_ref().map(|s| Selection { anchor: place(&s.anchor), focus: place(&s.focus) });
        let math = self.math.as_ref().and_then(|m| Some(MathEdit { at: moved(before, now, cs, &m.at)?, ..m.clone() }));
        let typing = self.typing.as_ref().and_then(|t| {
            let at = moved(before, now, cs, &t.at)?;
            // The word's ends move as the caret did: only text put in or taken away before
            // the caret moves it.
            let shift = |off: usize| off.checked_add(at.off)?.checked_sub(t.at.off);
            Some(TypedWord { start: shift(t.start)?, end: shift(t.end)?, at })
        });
        let marks: Vec<Pos> = self.edit_marks.iter().filter_map(|p| moved(before, now, cs, p)).collect();
        self.sel = sel;
        self.hf_return = hf_return;
        self.math = math;
        self.typing = typing;
        self.edit_marks = marks;
        self.also_selected.clear();
        self.column = None;
        self.widened = None;
        self.goal_x = None;
        self.autocap = None;
        self.clamp_selection();
        // The steps are whole documents without the change (see `apply_change`).
        self.reset_history();
        // Followers of this copy see the change as one of their own.
        if self.outside_commands() {
            self.record_changes();
        }
        // The input method sees a change it didn't make, as after a command.
        self.ime_state_seen = Some(self.current_ime_caret_state());
        self.reset_ime_state_revision();
        self.pending_ime_application_change = true;
        self.touch();
    }
}

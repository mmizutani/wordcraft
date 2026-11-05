//! A document shared with other copies of it ([`crate::room`]): their changes are made in the
//! open document as they come, and the caret, the selection and the other places the session
//! holds move with the text they are in.
//!
//! Undo stays the participant's own. The undo and redo steps are snapshots of the whole
//! document, so the newest of each keeps the changes others made after it ([`Theirs`]): taking
//! the step back makes its change undone (or redone) in the document as it is now, moved past
//! theirs as the hub moves an edit past those it accepted meanwhile, and leaves theirs in. When
//! one of theirs changed what the step changed (the same characters, blocks, part or document
//! field), the step stays and Undo names who made it.

use std::sync::Arc;

use wordcraft_doc::change::{Anchor, Mapped, Side};
use wordcraft_doc::{ChangeSet, Document, Pos};

use super::doc_state::{MAX_THEIRS, Selection, Their, Theirs, TypedWord, Undo};
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

/// Where place `p` of `snap` is in `now`, which is `snap` with `theirs` made in it one after the
/// other: moved through them, or as near as `now` has when its text is gone.
fn carried(snap: &Document, now: &Document, theirs: &[Their], p: &Pos) -> Pos {
    let mut m = snap.anchor_at(p).map(Mapped::At);
    for (cs, _) in theirs {
        m = match m {
            Some(Mapped::At(a)) => Some(cs.map(a)),
            Some(Mapped::Before(block)) => Some(cs.map(Anchor { block, off: 0 })),
            Some(Mapped::After(block)) => Some(cs.map(Anchor { block, off: usize::MAX })),
            Some(Mapped::Gone) | None => None,
        };
    }
    m.and_then(|m| now.place(m, Some(p))).unwrap_or_else(|| now.clamp(p))
}

/// The name an undo step gives for who made a change: theirs, or "another participant" for a
/// change that came with none.
fn who(by: &str) -> &str {
    if by.is_empty() { "another participant" } else { by }
}

/// Who made `changes`: each name once, in the order they came, the first three of them.
fn names(changes: &[Their]) -> Arc<str> {
    let mut seen: Vec<&str> = Vec::new();
    for (_, by) in changes {
        let by = who(by);
        if !seen.contains(&by) {
            seen.push(by);
        }
    }
    let mut out = seen.iter().take(3).copied().collect::<Vec<_>>().join(", ");
    if seen.len() > 3 {
        out.push_str(" and others");
    }
    Arc::from(out)
}

/// Add `more`, changes others made after step `u`, to those it keeps: the first of them made on
/// `base`, the document as the step left it, when it keeps none yet. Past [`MAX_THEIRS`] they're
/// kept as one change, the difference they made up to `now`, the document they lead to.
fn add_theirs(u: &mut Arc<Undo>, base: impl FnOnce() -> Document, more: Vec<Their>, now: &Document) {
    if more.is_empty() {
        return;
    }
    let u = Arc::make_mut(u);
    let t = Arc::make_mut(u.theirs.get_or_insert_with(|| Arc::new(Theirs { base: base(), changes: Vec::new() })));
    t.changes.extend(more);
    if t.changes.len() > MAX_THEIRS {
        let by = names(&t.changes);
        t.changes = vec![(Arc::new(ChangeSet::between(&t.base, now)), by)];
    }
}

/// An undo or redo step made again past the changes others made since ([`Session::past_theirs`]).
pub(super) struct Moved {
    /// The document with the step's change undone (or redone) in it.
    doc: Document,
    /// Theirs, moved to apply to the step's snapshot, one after the other.
    theirs: Vec<Their>,
}

/// The undo and redo steps as they stood before a command, for a room's copy to put back when it
/// makes the command again after a clash ([`Session::put_steps`]).
#[derive(Clone)]
pub(crate) struct Steps {
    history: Vec<Arc<Undo>>,
    redo: Vec<Arc<Undo>>,
    evicted: u64,
    /// The document then.
    at: Document,
    /// The changes others made since, each on the document the one before it left.
    theirs: Vec<Their>,
    /// Whether more came than the steps keep: they can't be put back.
    lost: bool,
}

impl std::fmt::Debug for Steps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Steps").field("undo", &self.history.len()).field("redo", &self.redo.len()).field("theirs", &self.theirs.len()).finish()
    }
}

impl Steps {
    /// Change `cs`, which participant `by` made on the document as it is with the changes noted
    /// so far, came: the steps put back keep it.
    pub(crate) fn note_theirs(&mut self, cs: &ChangeSet, by: &str) {
        if self.theirs.len() >= MAX_THEIRS {
            self.lost = true;
            self.theirs.clear();
        } else if !self.lost {
            self.theirs.push((Arc::new(cs.clone()), Arc::from(by)));
        }
    }
}

impl Session {
    /// Make change `cs`, which another copy of the active document made (another participant
    /// of its room), in this one. The places the session holds move with the text they are in:
    /// the caret and the selection, the equation being edited, the body selection a header
    /// keeps, the word being typed and Go Back's places. Pictures selected beside the
    /// selection and a column selection are let go. Followers of the document see the change
    /// ([`Session::changes`]).
    ///
    /// The undo and redo steps stay: Undo takes the session's own steps back and leaves the
    /// change in (see the [module](self) docs). What is typed next is a step of its own.
    ///
    /// An error, leaving the session as it was, when the change doesn't apply
    /// ([`Document::apply`]).
    pub fn apply_change(&mut self, cs: &ChangeSet) -> wordcraft_doc::Result<()> {
        self.apply_theirs(cs, "")
    }

    /// [`Session::apply_change`] for a change participant `by` made: an undo step that can't be
    /// taken back past it names them.
    pub fn apply_theirs(&mut self, cs: &ChangeSet, by: &str) -> wordcraft_doc::Result<()> {
        let mut doc = self.doc.clone();
        doc.apply(cs)?;
        let before = std::mem::replace(&mut self.doc, doc);
        self.follow_shared(&before, Some(cs));
        self.note_theirs(before, cs, by);
        Ok(())
    }

    /// Others' change `cs`, made on `before`, is in the document now: the newest undo step and
    /// the newest redo step keep it, to move it past their change when they're taken.
    fn note_theirs(&mut self, before: Document, cs: &ChangeSet, by: &str) {
        let theirs: Their = (Arc::new(cs.clone()), Arc::from(by));
        let now = &self.doc;
        for u in [self.history.last_mut(), self.redo.last_mut()].into_iter().flatten() {
            add_theirs(u, || before.clone(), vec![theirs.clone()], now);
        }
    }

    /// The newest undo step (or redo step, `redo`) made again in the document as it is, past the
    /// changes others made since it: its change undone (or redone) moved past theirs, and theirs
    /// moved before it. `None` when there's no step, or no change of theirs since (the step's
    /// snapshot is then the document as it was). An error, naming who, when one of theirs
    /// changed what the step changed.
    pub(super) fn past_theirs(&self, redo: bool) -> Result<Option<Moved>, String> {
        let step = if redo { self.redo.last() } else { self.history.last() };
        let Some((u, t)) = step.and_then(|u| Some((u, u.theirs.as_deref()?))) else { return Ok(None) };
        let done = if redo { "redone" } else { "undone" };
        // The step's change undone (or redone), made on the document as the step left it.
        let mut back = ChangeSet::between(&t.base, &u.doc);
        let mut theirs = Vec::with_capacity(t.changes.len());
        for (cs, by) in &t.changes {
            // As the hub moves edits made at the same time: theirs comes first where both put
            // text in at the same place.
            match (cs.rebase(&back, Side::Before), back.rebase(cs, Side::After)) {
                (Ok(c), Ok(b)) => {
                    theirs.push((Arc::new(c), by.clone()));
                    back = b;
                }
                _ => return Err(format!("{} can't be {done}: {} has changed it since", u.label, who(by))),
            }
        }
        let mut doc = self.doc.clone();
        doc.apply(&back).map_err(|e| format!("{} can't be {done}: the document changed since ({e})", u.label))?;
        Ok(Some(Moved { doc, theirs }))
    }

    /// Make step `u`, taken off the undo stack (or the redo stack, `redo`), the document: its
    /// snapshot, or `moved` past the changes others made since ([`Session::past_theirs`]),
    /// which then come after the step that is now the newest on that stack. Returns what it
    /// replaced (the document, the selection and the rest): the step for the other stack.
    pub(super) fn put_step(&mut self, u: Undo, moved: Option<Moved>, redo: bool) -> Undo {
        let Undo { label, doc, sel, pending, column, theirs: _ } = u;
        let (doc, sel, column) = match moved {
            None => (doc, sel, column),
            Some(Moved { doc: now, theirs }) => {
                let sel = Selection { anchor: carried(&doc, &now, &theirs, &sel.anchor), focus: carried(&doc, &now, &theirs, &sel.focus) };
                let below = if redo { self.redo.last_mut() } else { self.history.last_mut() };
                if let Some(below) = below {
                    add_theirs(below, || doc, theirs, &now);
                }
                (now, sel, None)
            }
        };
        Undo {
            label,
            doc: std::mem::replace(&mut self.doc, doc),
            sel: std::mem::replace(&mut self.sel, sel),
            pending: std::mem::replace(&mut self.pending, pending),
            column: std::mem::replace(&mut self.column, column),
            theirs: None,
        }
    }

    /// The undo and redo steps as they stand, and the document they stand on, to put back with
    /// [`Session::put_steps`].
    pub(crate) fn steps(&self) -> Steps {
        Steps {
            history: self.history.clone(),
            redo: self.redo.clone(),
            evicted: self.undo_evicted,
            at: self.doc.clone(),
            theirs: Vec::new(),
            lost: false,
        }
    }

    /// Put back undo and redo steps taken with [`Session::steps`], now that the document is
    /// the one they were taken on with the changes others made since in it
    /// ([`Steps::note_theirs`]): a room's copy that drops its edits from the one that clashed
    /// with theirs, before it makes them again, keeps the steps it had before them. Nothing
    /// changes when more came than the steps keep.
    pub(crate) fn put_steps(&mut self, steps: Steps) {
        let Steps { mut history, mut redo, evicted, at, theirs, lost } = steps;
        if lost {
            return;
        }
        let now = &self.doc;
        for u in [history.last_mut(), redo.last_mut()].into_iter().flatten() {
            add_theirs(u, || at.clone(), theirs.clone(), now);
        }
        self.history = history;
        self.redo = redo;
        self.undo_evicted = evicted;
        self.typing_open = false;
        self.join_anchor = false;
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
        // The steps are snapshots of another version: putting one back would take what the
        // versions differ by away too (a room's copy puts its own back, `Session::put_steps`).
        self.reset_history();
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

    /// Make document `id` the active one again after a command made another one active,
    /// leaving the others open: a session the room's own process lends a copy keeps them
    /// ([`crate::room::Replica::lend`]). False, changing nothing, when `id` is not open.
    pub(crate) fn come_back_to(&mut self, id: u64) -> bool {
        self.switch_to(id).is_ok()
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
        // What is typed or dragged next is a step of its own, so that undoing it leaves the
        // change alone.
        self.typing_open = false;
        self.join_anchor = false;
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

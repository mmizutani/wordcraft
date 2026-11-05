//! [`Replica`]: a participant's own copy of a room's document.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use wordcraft_doc::change::{Anchor, Mapped, Side};
use wordcraft_doc::edit::Fragment;
use wordcraft_doc::props::{CharProps, Highlight, ParaProps};
use wordcraft_doc::{BlockId, ChangeSet, Document, Path, Pos, StoryRef};

use super::{Accepted, Bounce, MAX_BATCH, Participant, ParticipantId, Room};
use crate::cmd::edit::ClipHistory;
use crate::cmd::moves::PendingMove;
use crate::scope::{Place, Scope, positions};
use crate::{CellRange, CmdError, ColumnBlock, FindState, Prefs, Selection, Session, ViewState};

/// How many times a copy makes the same edit again after reading the document again before it
/// gives up on it (the hub refused it, or the copy and the hub disagreed). Edits made again
/// after a clash don't count: each clash is another participant's change accepted first.
const MAX_TRIES: u32 = 3;

/// What a copy did since it joined.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    /// Changes it sent to the hub: one for each command that changed the document.
    pub sent: u64,
    /// Of those, how many the hub accepted after moving them past changes it accepted meanwhile.
    pub moved: u64,
    /// Of those, how many the hub sent back.
    pub bounced: u64,
    /// Changes of other participants it made in its copy.
    pub received: u64,
    /// Of those, how many clashed with edits of its own the hub hadn't accepted yet.
    pub clashes: u64,
    /// Commands it ran again, after a clash or after reading the document again.
    pub made_again: u64,
    /// Edits it gave up: commands that failed when run again (or were run again too often),
    /// and changes made outside a command, which can't be.
    pub lost: u64,
    /// Times it read the whole document from the hub again.
    pub read_again: u64,
}

/// The selection as anchors: where it starts and where the caret is.
type SelAnchors = Option<(Anchor, Anchor)>;

/// A column selection, or whole table cells, as anchors: its line pieces, and the table its
/// cells are in by id.
#[derive(Clone, Debug)]
struct ColumnAnchors {
    anchor_x: f32,
    focus_x: f32,
    segments: Vec<(Anchor, Anchor)>,
    /// Whole cells: their table's id, the range, and the row and cell each piece starts in.
    cells: Option<(BlockId, CellRange, Vec<(u32, u32)>)>,
}

/// The equation being edited when a command was made: the equation by anchor, the caret in
/// it, and how typing in it is read.
#[derive(Clone, Debug)]
struct MathAnchors {
    at: Anchor,
    pos: wordcraft_doc::math_edit::MathPos,
    latex: bool,
    normal_text: bool,
}

/// What a selection held besides its ends when a command was made, as anchors: a column or
/// whole cells, the equation being edited, more pictures and shapes selected along with the
/// one the selection holds, and the ink selected with the lasso (its page and the strokes).
#[derive(Clone, Debug, Default)]
struct Around {
    column: Option<ColumnAnchors>,
    math: Option<MathAnchors>,
    also: Vec<Anchor>,
    lasso: Option<(usize, Vec<Anchor>)>,
}

impl Around {
    /// Session `s`'s.
    fn of(s: &Session) -> Around {
        let math = s.math.as_ref().and_then(|m| {
            Some(MathAnchors { at: s.doc.anchor_at(&m.at)?, pos: m.pos.clone(), latex: s.math_latex, normal_text: s.math_normal_text })
        });
        let also = s.also_selected.iter().map(|p| s.doc.anchor_at(p)).collect::<Option<Vec<_>>>().unwrap_or_default();
        let lasso = crate::cmd::draw::lasso_selection(s)
            .and_then(|l| Some((l.page, l.items.iter().map(|p| s.doc.anchor_at(p)).collect::<Option<Vec<_>>>()?)));
        Around { column: column_anchors(s), math, also, lasso }
    }

    fn is_empty(&self) -> bool {
        self.column.is_none() && self.math.is_none() && self.also.is_empty() && self.lasso.is_none()
    }

    /// Move its anchors through `cs`. One whose text is gone keeps its place, which isn't found
    /// again: the edit is given up rather than made on what is left.
    fn moved(&mut self, cs: &ChangeSet) {
        let mv = |a: &mut Anchor| *a = moved_anchor(*a, cs).unwrap_or(*a);
        if let Some(c) = &mut self.column {
            for (a, b) in &mut c.segments {
                mv(a);
                mv(b);
            }
        }
        if let Some(m) = &mut self.math {
            mv(&mut m.at);
        }
        self.also.iter_mut().for_each(mv);
        if let Some((_, items)) = &mut self.lasso {
            items.iter_mut().for_each(mv);
        }
    }
}

/// The row and cell of table `table` (in `story`) that place `p` is in.
fn cell_of(story: StoryRef, table: &Path, p: &Pos) -> Option<(u32, u32)> {
    let n = table.0.len();
    if p.story != story || p.path.0.get(..n)? != table.0.as_slice() {
        return None;
    }
    Some((*p.path.0.get(n)?, *p.path.0.get(n + 1)?))
}

/// Session `s`'s column selection or whole cells, when the selection is one, as anchors (`None`
/// when a piece isn't in a paragraph).
fn column_anchors(s: &Session) -> Option<ColumnAnchors> {
    let c = s.column.as_ref().filter(|c| c.sel == s.sel)?;
    let segments = c.segments.iter().map(|(a, b)| Some((s.doc.anchor_at(a)?, s.doc.anchor_at(b)?))).collect::<Option<Vec<_>>>()?;
    let cells = match &c.cells {
        Some(r) => {
            let at = c.segments.iter().map(|(a, _)| cell_of(r.story, &r.table, a)).collect::<Option<Vec<_>>>()?;
            Some((s.doc.block(r.story, &r.table)?.id(), r.clone(), at))
        }
        None => None,
    };
    Some(ColumnAnchors { anchor_x: c.anchor_x, focus_x: c.focus_x, segments, cells })
}

/// Commands that read more of their session than the document, the selection and their params
/// ([`Inputs`]): pastes, Paste Formatting, Replace (`^c` is the clipboard), the Spike, Quick
/// Parts and AutoText, macros and Repeat. A copy keeps those inputs with each of them, so a
/// command made again after a clash pastes what was pasted then, not what was copied since.
const READS_INPUTS: &[&str] = &[
    "edit.paste",
    "edit.pasteSpecial",
    "edit.pasteText",
    "edit.pasteMerge",
    "edit.pasteClipboardItem",
    "edit.pasteAllClipboard",
    "edit.pasteFormat",
    "edit.replace",
    "edit.replaceAll",
    "edit.insertSpike",
    "insert.quickParts",
    "insert.autoText",
    "tools.macros",
    "edit.repeat",
];

/// What a command can read from its session besides the document, the selection and its
/// params: the clipboard and the Clipboard pane's items, the format painter's formatting, what
/// Find looks for, the command Repeat repeats, macros and building blocks (the Spike among them).
#[derive(Clone, Debug)]
struct Inputs {
    clipboard: Option<Fragment>,
    clipboard_text: String,
    clipboard_html: String,
    clip_history: ClipHistory,
    painter: Option<(CharProps, ParaProps, bool)>,
    find: FindState,
    last_command: Option<(String, Value)>,
    macros: BTreeMap<String, Vec<(String, Value)>>,
    building_blocks: BTreeMap<String, Fragment>,
    /// The tracked cut a paste makes a move of.
    pending_move: Option<PendingMove>,
}

impl Inputs {
    /// Session `s`'s.
    fn of(s: &Session) -> Inputs {
        Inputs {
            clipboard: s.clipboard.clone(),
            clipboard_text: s.clipboard_text.clone(),
            clipboard_html: s.clipboard_html.clone(),
            clip_history: s.clip_history.clone(),
            painter: s.painter.clone(),
            find: s.find.clone(),
            last_command: s.last_command.clone(),
            macros: s.macros.clone(),
            building_blocks: s.building_blocks.clone(),
            pending_move: s.pending_move.clone(),
        }
    }

    /// Put them in session `s`.
    fn put(self, s: &mut Session) {
        let Inputs { clipboard, clipboard_text, clipboard_html, clip_history, painter, find, last_command, macros, building_blocks, pending_move } =
            self;
        s.clipboard = clipboard;
        s.clipboard_text = clipboard_text;
        s.clipboard_html = clipboard_html;
        s.clip_history = clip_history;
        s.painter = painter;
        s.find = find;
        s.last_command = last_command;
        s.macros = macros;
        s.building_blocks = building_blocks;
        // A cut made again just before keeps the move it began (its revision can differ from
        // the one this cut had).
        if s.pending_move.is_none() {
            s.pending_move = pending_move;
        }
    }
}

/// The commands whose `id` param names a comment. A comment made again after a clash can get
/// another id than it had (the other participant's new comment took it), so the edits after it
/// that name it are made again with its new id.
const NAMES_COMMENTS: &[&str] = &["review.reply", "review.editComment", "review.deleteComment", "review.resolveComment"];

/// The ids of the comments in `doc` that aren't in `before` (sorted).
fn new_comments(before: &[u32], doc: &Document) -> Vec<u32> {
    doc.comments.keys().copied().filter(|id| before.binary_search(id).is_err()).collect()
}

/// The ids of the blocks change `cs` put in that weren't in the document before it (in the
/// order it put them in): not those it took away and put back.
fn new_blocks(cs: &ChangeSet) -> Vec<BlockId> {
    let parts = cs.parts.iter().flat_map(|p| match p {
        wordcraft_doc::change::PartChange::Removed { blocks, .. } => blocks.as_slice(),
        _ => &[],
    });
    let old: std::collections::BTreeSet<BlockId> =
        cs.splices.iter().flat_map(|s| &s.removed).chain(parts).flat_map(|b| std::iter::once(b.id).chain(b.inside.iter().copied())).collect();
    cs.ids_put_in().into_iter().filter(|id| !old.contains(id)).collect()
}

/// What a command reads from its session besides the document, the selection, its params and
/// [`Inputs`], and the participant can change without changing the document: the options
/// (typing replaces the selection, Overtype, AutoCorrect, AutoFormat…), the view (Outline view,
/// field codes, the pens and the rest of the Draw tab, design mode…; without the lasso, kept
/// with the selection in [`Around`]), the highlighter colour, the editing language, the
/// bibliography style, column selection mode, and the name and initials new comments and
/// tracked changes are made under.
#[derive(Clone, Debug, PartialEq)]
struct Context {
    prefs: Prefs,
    view: ViewState,
    highlight: Highlight,
    editing_lang: Option<String>,
    bib_style: String,
    column_mode: bool,
    author: String,
    initials: String,
}

impl Context {
    /// Session `s`'s, whose lasso is taken out.
    fn of(s: &Session) -> Context {
        Context {
            prefs: s.prefs.clone(),
            view: s.view.clone(),
            highlight: s.last_highlight,
            editing_lang: s.editing_lang.clone(),
            bib_style: s.bib_style.clone(),
            column_mode: s.column_mode,
            author: s.author.clone(),
            initials: s.initials.clone(),
        }
    }

    /// Whether session `s`, whose lasso is taken out, has this one.
    fn is(&self, s: &Session) -> bool {
        self.prefs == s.prefs
            && self.view == s.view
            && self.highlight == s.last_highlight
            && self.editing_lang == s.editing_lang
            && self.bib_style == s.bib_style
            && self.column_mode == s.column_mode
            && self.author == s.author
            && self.initials == s.initials
    }

    /// Put it in session `s`, which keeps its lasso.
    fn put(self, s: &mut Session) {
        let lasso = s.view.draw.lasso.take();
        s.prefs = self.prefs;
        s.view = self.view;
        s.view.draw.lasso = lasso;
        s.last_highlight = self.highlight;
        s.editing_lang = self.editing_lang;
        s.bib_style = self.bib_style;
        s.column_mode = self.column_mode;
        s.author = self.author;
        s.initials = self.initials;
    }
}

/// What a session holds besides its document that making edits again must leave as the
/// participant has it now: [`Inputs`], [`Context`] and the lasso, the
/// formatting picked at the caret for the next typed text, the
/// tracked cut a paste can make a move of, and the macro being recorded (edits made again
/// aren't steps of it). Edits made again use and change their own while they run.
struct Kept {
    inputs: Inputs,
    context: Context,
    lasso: Option<crate::cmd::draw::LassoSelection>,
    pending: Option<CharProps>,
    math_flags: (bool, bool),
    pending_move: Option<PendingMove>,
    recording: Option<(String, Vec<(String, Value)>)>,
}

impl Kept {
    fn take(s: &mut Session) -> Kept {
        Kept {
            inputs: Inputs::of(s),
            context: Context::of(s),
            lasso: s.view.draw.lasso.clone(),
            pending: s.pending.clone(),
            math_flags: (s.math_latex, s.math_normal_text),
            pending_move: s.pending_move.take(),
            recording: s.recording.take(),
        }
    }

    fn put(self, s: &mut Session) {
        self.inputs.put(s);
        self.context.put(s);
        s.view.draw.lasso = self.lasso;
        s.pending = self.pending;
        (s.math_latex, s.math_normal_text) = self.math_flags;
        s.pending_move = self.pending_move;
        s.recording = self.recording;
    }
}

/// A command the copy ran, with what it takes to run it again from the same place.
#[derive(Clone, Debug)]
struct Made {
    /// The command and its params; `None` for a change made outside a command.
    command: Option<(String, Value)>,
    /// The selection before and after it.
    before: SelAnchors,
    after: SelAnchors,
    /// What the selection held besides its ends before it (a column or cells, the equation being
    /// edited, more objects, the lasso).
    around: Around,
    /// Its params that are places in the document ([`positions`]).
    places: Vec<(String, Anchor)>,
    /// What it read from the session besides the document and its params, for the commands that
    /// read more ([`READS_INPUTS`]).
    inputs: Option<Box<Inputs>>,
    /// The formatting picked at the caret for the next typed text when it was made.
    pending: Option<CharProps>,
    /// The ids of the comments it made (sorted).
    comments: Vec<u32>,
    /// The ids of the blocks it made ([`new_blocks`]).
    blocks: Vec<BlockId>,
    /// The options, view and the rest of what it read from the session that the participant can
    /// change without an edit, shared with the commands before it while they don't change.
    context: Option<Arc<Context>>,
    /// How many times it was run again after the copy read the document again.
    tries: u32,
}

impl Made {
    /// A change made outside a command.
    fn outside() -> Made {
        Made {
            command: None,
            before: None,
            after: None,
            around: Around::default(),
            places: Vec::new(),
            inputs: None,
            pending: None,
            comments: Vec::new(),
            blocks: Vec::new(),
            context: None,
            tries: 0,
        }
    }
}

/// Edits of the copy's own the hub hasn't accepted yet: their change, which applies to the
/// copy's document before them, and the commands that made them.
#[derive(Clone, Debug)]
struct Unit {
    change: ChangeSet,
    made: Vec<Made>,
    /// Which edits these are, for the copy to know them after a round trip.
    serial: u64,
}

/// The selection of session `s`, as anchors (`None` when an end isn't in a paragraph).
fn selection_anchors(s: &Session) -> SelAnchors {
    Some((s.doc.anchor_at(&s.sel.anchor)?, s.doc.anchor_at(&s.sel.focus)?))
}

/// Where anchor `a` is after change `cs`: in the text it was in, or at the start or end of the
/// block it is left beside (see [`Document::pos_of`]); `None` when its part is gone.
fn moved_anchor(a: Anchor, cs: &ChangeSet) -> Option<Anchor> {
    match cs.map(a) {
        Mapped::At(a) => Some(a),
        Mapped::Before(block) => Some(Anchor { block, off: 0 }),
        Mapped::After(block) => Some(Anchor { block, off: usize::MAX }),
        Mapped::Gone => None,
    }
}

impl Made {
    /// Move the places it keeps through `before`, a change made in the copy's document before
    /// the command, and `after`, the same change moved past the command's own.
    fn moved(&mut self, before: &ChangeSet, after: &ChangeSet) {
        let sel = |s: SelAnchors, cs: &ChangeSet| s.and_then(|(a, f)| Some((moved_anchor(a, cs)?, moved_anchor(f, cs)?)));
        self.before = sel(self.before, before);
        self.after = sel(self.after, after);
        self.places.retain_mut(|(_, at)| match moved_anchor(*at, before) {
            Some(a) => {
                *at = a;
                true
            }
            None => false,
        });
        self.around.moved(before);
    }
}

/// A participant's copy of a room's document (see [`super`]). Its commands run at once
/// ([`Replica::run`]); [`Replica::send`] sends the edits they made to the hub,
/// [`Replica::receive`] makes the changes the hub accepted in the copy. A copy on a thread of
/// its own calls them when it likes: the copy is never wrong, only behind.
pub struct Replica {
    room: Arc<Room>,
    who: Participant,
    session: Session,
    /// The session's id for the room's document, the only one the copy has open.
    document: u64,
    /// The version of the document the copy got every change up to.
    received: u64,
    /// The document as of `received`, as the hub had it.
    confirmed: Document,
    /// Its own edits the hub hasn't accepted yet, oldest first: the session's document is
    /// `confirmed` with them made in it.
    pending: Vec<Unit>,
    /// The changes of its own the hub accepted that haven't come back yet, the first ones of
    /// `pending`: the version the first of them made, and how many they are.
    sent: Option<(u64, usize)>,
    /// How many changes of the session's the copy has looked at.
    seen: u64,
    /// The next unit's serial.
    serials: u64,
    stats: Stats,
    /// While edits are made again: the blocks they made, by the id each had when it was made,
    /// with the id it has now, or none when it wasn't made again. Places in them are found by
    /// these ids, not moved by comparing documents, which would take a block another
    /// participant put in at the same place for one of them.
    reborn: BTreeMap<BlockId, Option<BlockId>>,
}

impl Replica {
    pub(super) fn new(room: Arc<Room>, who: Participant, mut session: Session, version: u64, confirmed: Document) -> Replica {
        // Commands run inside others (a macro's steps, Repeat) are held to the role's scopes
        // too: they can't reach files either.
        session.confined = true;
        let document = session.document_id();
        let seen = session.changes().version();
        Replica {
            room,
            who,
            session,
            document,
            received: version,
            confirmed,
            pending: Vec::new(),
            sent: None,
            seen,
            serials: 0,
            stats: Stats::default(),
            reborn: BTreeMap::new(),
        }
    }

    /// Which participant this is.
    pub fn id(&self) -> ParticipantId {
        self.who.id
    }

    /// Who the participant is: their number, name and role.
    pub fn participant(&self) -> &Participant {
        &self.who
    }

    /// The room.
    pub fn room(&self) -> &Arc<Room> {
        &self.room
    }

    /// The copy's session: its document, selection and the rest. Commands go through
    /// [`Replica::run`], which keeps what they change.
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// The version of the document the copy got every change up to.
    pub fn received(&self) -> u64 {
        self.received
    }

    /// Whether the copy has edits the hub hasn't accepted, or accepted and not sent back yet.
    pub fn has_unconfirmed(&self) -> bool {
        !self.pending.is_empty()
    }

    /// What the copy did since it joined.
    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// Run command `id` in the copy ([`Session::run`]): what it changes is in the copy at once,
    /// and goes to the hub with the next [`Replica::send`]. Commands on files, other documents
    /// and windows are refused: a copy has the room's document only. So are commands the
    /// participant's role doesn't allow ([`super::Role::allows`]), and the session undoes one
    /// that changed more than the role lets them ([`Session::role`]).
    pub fn run(&mut self, id: &str, params: &Value) -> Result<Value, CmdError> {
        let Some(scope) = Scope::of(id) else { return Err(CmdError::Unknown(id.to_string())) };
        if let Some(why) = self.who.role.refuses(scope) {
            return Err(CmdError::Disabled(format!("{id}: {why}")));
        }
        // A macro's steps run one by one as the copy's commands, so that each keeps its places
        // and the rest of what it takes to make it again (made again whole, a macro would make
        // its steps at the offsets they had). A step that fails stops the macro; the steps
        // before it stay made.
        if id == "tools.macros"
            && let Some(name) = params.get("run").and_then(Value::as_str)
        {
            let mut rest = params.clone();
            if let Some(o) = rest.as_object_mut() {
                o.remove("run");
            }
            if rest.as_object().is_some_and(|o| !o.is_empty()) {
                self.run(id, &rest)?;
            }
            let steps = self.session.macros.get(name).cloned().ok_or_else(|| CmdError::Params(format!("no macro `{name}`")))?;
            for (step, p) in steps.iter().take(10_000).filter(|(step, _)| step != "tools.macros") {
                self.run(step, p)?;
            }
            return Ok(serde_json::json!({"ran": name, "steps": steps.len()}));
        }
        self.capture();
        self.run_made(id, params, 0)
    }

    /// Run command `id`, run again `tries` times after reading the document again so far, and
    /// keep what it changed as an edit of the copy's own.
    fn run_made(&mut self, id: &str, params: &Value, tries: u32) -> Result<Value, CmdError> {
        let before = selection_anchors(&self.session);
        let around = Around::of(&self.session);
        let doc = &self.session.doc;
        let places = positions(id)
            .iter()
            .filter(|(_, how)| *how == Place::Pos)
            .filter_map(|(key, _)| Some((key.to_string(), doc.anchor_at(&crate::cmd::parse_pos(params.get(*key)?)?)?)))
            .collect();
        let inputs = READS_INPUTS.contains(&id).then(|| Box::new(Inputs::of(&self.session)));
        let lasso = self.session.view.draw.lasso.take();
        let context = match self.pending.last().and_then(|u| u.made.last()).and_then(|m| m.context.clone()) {
            Some(c) if c.is(&self.session) => c,
            _ => Arc::new(Context::of(&self.session)),
        };
        self.session.view.draw.lasso = lasso;
        let pending = self.session.pending.clone();
        let comments_before: Vec<u32> = self.session.doc.comments.keys().copied().collect();
        let result = self.session.run(id, params);
        let comments = new_comments(&comments_before, &self.session.doc);
        self.stay_in_document();
        let after = selection_anchors(&self.session);
        let made = Made {
            command: Some((id.to_string(), params.clone())),
            before,
            after,
            around,
            places,
            inputs,
            pending,
            comments,
            blocks: Vec::new(),
            context: Some(context),
            tries,
        };
        self.collect(made);
        result
    }

    /// Send the copy's edits the hub hasn't had yet to the hub, each command's change on its
    /// own, made on the version the copy got last; unless changes it sent haven't all come back
    /// yet. When the hub sends one back, the copy makes it and those after it again on the
    /// latest version.
    pub fn send(&mut self) {
        self.capture();
        if self.sent.is_some() || self.pending.is_empty() {
            return;
        }
        let n = self.pending.len().min(MAX_BATCH);
        let changes: Vec<ChangeSet> = self.pending.iter().take(n).map(|u| u.change.clone()).collect();
        let serials: Vec<u64> = self.pending.iter().take(n).map(|u| u.serial).collect();
        self.stats.sent = self.stats.sent.saturating_add(n as u64);
        let took = self.room.submit(self.received, &changes, self.who.id);
        let back = match took {
            Ok(took) => {
                self.sent = Some((took.version, took.accepted));
                if took.moved_past > 0 {
                    self.stats.moved = self.stats.moved.saturating_add(took.accepted as u64);
                }
                serials.get(took.accepted).copied()
            }
            Err(Bounce::Clash { .. }) => serials.first().copied(),
            Err(_) => {
                self.stats.bounced = self.stats.bounced.saturating_add(n as u64);
                self.read_again();
                return;
            }
        };
        if let Some(back) = back {
            let bounced = n.saturating_sub(serials.iter().position(|s| *s == back).unwrap_or(n));
            self.stats.bounced = self.stats.bounced.saturating_add(bounced as u64);
            // The change the first of them clashed with clashes here too: the copy drops them
            // there and makes them again.
            self.receive();
            if self.pending.iter().any(|u| u.serial == back) {
                self.read_again();
            }
        }
    }

    /// Get the changes the hub accepted since the version the copy got last: its own come
    /// back, and the others' are made in the copy, past its edits the hub hasn't accepted
    /// (which move past them). Edits of its own that clash with one are made again after it.
    pub fn receive(&mut self) {
        self.capture();
        let got = self.room.hub().since(self.received);
        match got {
            Ok(changes) => {
                for a in &changes {
                    self.integrate(a);
                }
            }
            Err(_) => self.read_again(),
        }
    }

    /// [`Replica::receive`], then [`Replica::send`].
    pub fn sync(&mut self) {
        self.receive();
        self.send();
    }

    /// Make accepted change `a` in the copy.
    fn integrate(&mut self, a: &Accepted) {
        if a.version <= self.received {
            // The copy read the document again meanwhile.
            return;
        }
        if a.version != self.received.saturating_add(1) {
            self.read_again();
            return;
        }
        if let Some((version, count)) = self.sent
            && version == a.version
        {
            // Its own change, back, as the hub moved it: the copy moved it the same way.
            let same = self.pending.first().is_some_and(|u| u.change == *a.change);
            if !same || self.confirmed.apply(&a.change).is_err() {
                self.read_again();
                return;
            }
            self.sent = (count > 1).then(|| (version.saturating_add(1), count - 1));
            self.pending.remove(0);
            self.received = a.version;
            return;
        }
        // Another participant's change: it goes before the copy's own edits, which move past it.
        // `through` has it as it meets each of them (it moves past each in turn), and after the
        // last it moved past.
        let mut m: ChangeSet = (*a.change).clone();
        let mut moved = Vec::with_capacity(self.pending.len());
        let mut through = Vec::with_capacity(self.pending.len().saturating_add(1));
        let mut clash = None;
        for (i, u) in self.pending.iter().enumerate() {
            match (u.change.rebase(&m, Side::After), m.rebase(&u.change, Side::Before)) {
                (Ok(c), Ok(n)) => {
                    moved.push(c);
                    through.push(std::mem::replace(&mut m, n));
                }
                _ => {
                    clash = Some(i);
                    break;
                }
            }
        }
        through.push(m.clone());
        // The hub accepted the changes it sent after this one, so they don't clash with it.
        if let (Some(i), Some((_, count))) = (clash, self.sent)
            && i < count
        {
            self.read_again();
            return;
        }
        // The document the first edit that clashed was made on.
        let origin = clash.and_then(|i| {
            let mut doc = self.confirmed.clone();
            for u in self.pending.get(..i)? {
                doc.apply(&u.change).ok()?;
            }
            Some(doc)
        });
        if self.confirmed.apply(&a.change).is_err() {
            self.read_again();
            return;
        }
        self.received = a.version;
        self.stats.received = self.stats.received.saturating_add(1);
        let dropped = clash.map(|i| self.pending.split_off(i)).unwrap_or_default();
        for (k, (u, c)) in self.pending.iter_mut().zip(moved).enumerate() {
            u.change = c;
            // The places its commands were made at move with the text, to make them again from
            // there if a later change clashes with them.
            if let (Some(before), Some(after)) = (through.get(k), through.get(k + 1)) {
                for made in &mut u.made {
                    made.moved(before, after);
                }
            }
        }
        if dropped.is_empty() {
            if self.session.apply_change(&m).is_err() {
                self.read_again();
                return;
            }
            self.seen = self.session.changes().version();
            return;
        }
        // Its edits from the clash on go; the copy holds the change with the edits before
        // them, and makes the dropped ones again from where they were made, moved through it.
        self.stats.clashes = self.stats.clashes.saturating_add(1);
        match self.expected() {
            Some(doc) => self.make_again(doc, dropped, Some(&m), origin, false),
            None => {
                self.pending.extend(dropped);
                self.read_again();
            }
        }
    }

    /// Read the document again from the hub, and make the copy's edits the hub hasn't accepted
    /// again on it.
    fn read_again(&mut self) {
        self.stats.read_again = self.stats.read_again.saturating_add(1);
        let (version, doc) = self.room.document();
        let mut dropped = std::mem::take(&mut self.pending);
        // The changes the hub accepted are in its document already. The others were made on the
        // version the copy got last with those in it.
        let mut origin = Some(self.confirmed.clone());
        if let Some((first, count)) = self.sent.take()
            && first <= version
        {
            for u in dropped.drain(..count.min(dropped.len())) {
                origin = origin.and_then(|mut d| d.apply(&u.change).ok().map(|_| d));
            }
        }
        self.received = version;
        self.confirmed = doc.clone();
        self.make_again(doc, dropped, None, origin, true);
    }

    /// Put `doc` in the copy and run the commands of `dropped` again: each from the selection
    /// it was made with, unless it went on from where the command before it left the caret,
    /// with the places in its params, and with what it read from the session then
    /// ([`Inputs`]). `origin` is the document the first of them was made on; the places of each
    /// move from the document it was made on to the one it is made again on, which holds the
    /// change the first of them clashed with, `first`, and those made again before it (or, when
    /// that isn't known, are found again by their paragraph). `read_again` when the copy read
    /// the document again, which counts as a try. The session's clipboard and the rest of what
    /// commands read stay as the participant has them now.
    fn make_again(&mut self, doc: Document, dropped: Vec<Unit>, first: Option<&ChangeSet>, mut origin: Option<Document>, read_again: bool) {
        let now = selection_anchors(&self.session);
        // The equation the participant is editing now, which edits made again leave them in.
        let math_now = self.session.math.as_ref().and_then(|m| Some((self.session.doc.anchor_at(&m.at)?, m.clone())));
        let last = dropped.iter().flat_map(|u| &u.made).last().map(|m| m.after);
        let kept = Kept::take(&mut self.session);
        self.session.reset_shared(doc);
        self.seen = self.session.changes().version();
        let mut went_on: Option<SelAnchors> = None;
        // The comments the edits made, by the id each had when it was made: the id it has now,
        // or none when it wasn't made again.
        let mut renamed: BTreeMap<u32, Option<u32>> = BTreeMap::new();
        let gone = |renamed: &mut BTreeMap<u32, Option<u32>>, comments: &[u32]| {
            for c in comments {
                renamed.insert(*c, None);
            }
        };
        self.reborn.clear();
        for (k, u) in dropped.into_iter().enumerate() {
            for (j, m) in u.made.into_iter().enumerate() {
                let Made { command, before, after, around, places, inputs, pending, comments, blocks, context, tries } = m;
                let tries = if read_again { tries.saturating_add(1) } else { tries };
                let Some((id, mut params)) = command.filter(|_| tries <= MAX_TRIES) else {
                    self.stats.lost = self.stats.lost.saturating_add(1);
                    gone(&mut renamed, &comments);
                    self.lost_blocks(&blocks);
                    went_on = None;
                    continue;
                };
                // A comment an edit before it made, named by the id it had then.
                if NAMES_COMMENTS.contains(&id.as_str())
                    && let Some(was) = params.get("id").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok())
                    && let Some(&now) = renamed.get(&was)
                {
                    match (now, params.as_object_mut()) {
                        (Some(now), Some(o)) => {
                            o.insert("id".into(), Value::from(now));
                        }
                        _ => {
                            self.stats.lost = self.stats.lost.saturating_add(1);
                            self.session.status = format!(
                                "An edit made at the same time as another participant's couldn't be made again ({id}): the comment it was made on is gone"
                            );
                            gone(&mut renamed, &comments);
                            self.lost_blocks(&blocks);
                            went_on = None;
                            continue;
                        }
                    }
                }
                let reselect = went_on != Some(before) || !around.is_empty();
                // From the document the command was made on to the one it is made again on: the
                // change its edits clashed with for the first, the difference for the others.
                let between;
                let through = match (k, j, first, &origin) {
                    (0, 0, Some(first), _) => Some(first),
                    (_, 0, _, Some(origin)) if reselect || !places.is_empty() => {
                        between = ChangeSet::between(origin, &self.session.doc);
                        Some(&between)
                    }
                    _ => None,
                };
                // An edit whose selection or places are gone (another participant took their text
                // or the text box they were in away) isn't made anywhere else.
                if !self.made_at(reselect, before, &around, &places, &mut params, through) {
                    self.stats.lost = self.stats.lost.saturating_add(1);
                    gone(&mut renamed, &comments);
                    self.lost_blocks(&blocks);
                    self.session.status = format!(
                        "An edit made at the same time as another participant's couldn't be made again ({id}): the text it was made in is gone"
                    );
                    went_on = None;
                    continue;
                }
                if let Some(inputs) = inputs {
                    inputs.put(&mut self.session);
                }
                if let Some(context) = context {
                    Arc::unwrap_or_clone(context).put(&mut self.session);
                }
                self.session.pending = pending;
                went_on = Some(after);
                let comments_before: Vec<u32> = self.session.doc.comments.keys().copied().collect();
                let units = self.pending.len();
                match self.run_made(&id, &params, tries) {
                    Ok(_) => {
                        self.stats.made_again = self.stats.made_again.saturating_add(1);
                        // Its new comments in the order they were made in, as their ids go up.
                        let made = new_comments(&comments_before, &self.session.doc);
                        if made.len() == comments.len() {
                            renamed.extend(comments.iter().copied().zip(made.into_iter().map(Some)));
                        } else {
                            gone(&mut renamed, &comments);
                        }
                        // Its new blocks, paired in the order they were put in.
                        let made = self
                            .pending
                            .get(units..)
                            .and_then(|u| u.last())
                            .and_then(|u| u.made.last())
                            .map(|m| m.blocks.clone())
                            .unwrap_or_default();
                        if made.len() == blocks.len() {
                            self.reborn.extend(blocks.iter().copied().zip(made.into_iter().map(Some)));
                        } else {
                            self.lost_blocks(&blocks);
                        }
                    }
                    Err(e) => {
                        self.stats.lost = self.stats.lost.saturating_add(1);
                        gone(&mut renamed, &comments);
                        self.lost_blocks(&blocks);
                        self.session.status = format!("An edit made at the same time as another participant's couldn't be made again ({id}): {e}");
                        went_on = None;
                    }
                }
            }
            origin = origin.and_then(|mut d| d.apply(&u.change).ok().map(|_| d));
        }
        kept.put(&mut self.session);
        // The participant's own selection, when they moved it after their last edit, and the
        // equation they are editing move from the document they had to the one with their edits
        // made again.
        let moved_sel = last != Some(now);
        let between = (moved_sel || math_now.is_some()).then(|| origin.map(|origin| ChangeSet::between(&origin, &self.session.doc))).flatten();
        let through = between.as_ref().or(first);
        if moved_sel && let Some(sel) = self.selection_at(now, through) {
            self.session.sel = sel;
            self.session.clamp_selection();
        }
        self.session.math = math_now.and_then(|(a, m)| Some(crate::MathEdit { at: self.find(a, through)?, ..m }));
        self.session.also_selected.clear();
        self.reborn.clear();
    }

    /// Blocks dropped edits made that weren't made again: places in them are gone.
    fn lost_blocks(&mut self, blocks: &[BlockId]) {
        self.reborn.extend(blocks.iter().map(|b| (*b, None)));
    }

    /// Put the selection a command was made with back (when `reselect`), with its column or
    /// cells, and its places in `params`, each moved through `through` or found by its
    /// paragraph. False, when one of them is gone.
    fn made_at(
        &mut self,
        reselect: bool,
        before: SelAnchors,
        around: &Around,
        places: &[(String, Anchor)],
        params: &mut Value,
        through: Option<&ChangeSet>,
    ) -> bool {
        if reselect && before.is_some() {
            let Some(sel) = self.selection_at(before, through) else { return false };
            self.session.sel = sel;
        }
        // Its column or cells, or none: a command made without one isn't made on the column a
        // command made again before it had.
        self.session.column = match &around.column {
            Some(c) => {
                let Some(block) = self.column_at(c, through) else { return false };
                Some(block)
            }
            None => None,
        };
        // The equation it was typed in, or none: a command made again outside an equation
        // isn't made in the one the participant is editing now.
        self.session.math = match &around.math {
            Some(m) => {
                let (Some(at), Some(session)) = (self.find(m.at, through), crate::next_math_session()) else { return false };
                self.session.math_latex = m.latex;
                self.session.math_normal_text = m.normal_text;
                Some(crate::MathEdit { at, pos: m.pos.clone(), session })
            }
            None => None,
        };
        let mut also = Vec::with_capacity(around.also.len());
        for a in &around.also {
            let Some(p) = self.find(*a, through) else { return false };
            also.push(p);
        }
        self.session.also_selected = also;
        self.session.view.draw.lasso = match &around.lasso {
            Some((page, items)) => {
                let Some(items) = items.iter().map(|a| self.find(*a, through)).collect::<Option<Vec<_>>>() else { return false };
                Some(crate::cmd::draw::lasso_now(&self.session, *page, items))
            }
            None => None,
        };
        for (key, at) in places {
            let Some(p) = self.find(*at, through) else { return false };
            if let Some(o) = params.as_object_mut() {
                o.insert(key.clone(), crate::cmd::pos_json(&p));
            }
        }
        true
    }

    /// The column selection or whole cells `c` names now, for the session's selection
    /// ([`Replica::find`]; the cells' table by its id, which must still have them).
    fn column_at(&self, c: &ColumnAnchors, through: Option<&ChangeSet>) -> Option<ColumnBlock> {
        let segments = c.segments.iter().map(|(a, b)| Some((self.find(*a, through)?, self.find(*b, through)?))).collect::<Option<Vec<_>>>()?;
        let cells = match &c.cells {
            Some((id, r, was)) => {
                // A table an edit made again made is found by the id it has now.
                let table = match self.reborn.get(id) {
                    Some(now) => (*now)?,
                    None => *id,
                };
                let mut at = None;
                self.session.doc.each_block(&mut |story, path, b| {
                    if at.is_none() && b.id() == table {
                        at = Some((story, Path(path.to_vec()), b.as_table().map_or(0, |t| t.rows.len())));
                    }
                });
                let (story, table, rows) = at?;
                // The cells are the same ones when each piece is in the same cell of its row and
                // every row moved by as much (rows put in or taken away before them); otherwise
                // which cells were meant isn't known.
                let now = segments.iter().map(|(a, _)| cell_of(story, &table, a)).collect::<Option<Vec<_>>>()?;
                let shift = match (was.first(), now.first()) {
                    (Some(w), Some(n)) => i64::from(n.0) - i64::from(w.0),
                    _ => 0,
                };
                let same = now.len() == was.len() && now.iter().zip(was).all(|(n, w)| n.1 == w.1 && i64::from(n.0) - i64::from(w.0) == shift);
                let row = |r: usize| usize::try_from(i64::try_from(r).ok()?.checked_add(shift)?).ok();
                let moved = (row(r.rows.0)?, row(r.rows.1)?);
                if !same || moved.1 >= rows {
                    return None;
                }
                Some(CellRange { story, table, rows: moved, ..r.clone() })
            }
            None => None,
        };
        Some(ColumnBlock { sel: self.session.sel.clone(), anchor_x: c.anchor_x, focus_x: c.focus_x, segments, cells })
    }

    /// Where anchor `a` is now: moved through `through`, which the copy's document had made
    /// since, or found by its paragraph.
    fn find(&self, a: Anchor, through: Option<&ChangeSet>) -> Option<Pos> {
        // In a block an edit made again made: in that block as it was made again.
        if let Some(now) = self.reborn.get(&a.block) {
            return self.session.doc.pos_of(Anchor { block: (*now)?, ..a }, None);
        }
        // Not looked for by its paragraph's id when the change moved it beside blocks: that
        // paragraph may hold other text now, and the old offset would be somewhere else in it.
        let m = through.map_or(Mapped::At(a), |cs| cs.map(a));
        self.session.doc.place(m, None)
    }

    /// The selection `sel` names now ([`Replica::find`]).
    fn selection_at(&self, sel: SelAnchors, through: Option<&ChangeSet>) -> Option<Selection> {
        let (a, f) = sel?;
        Some(Selection { anchor: self.find(a, through)?, focus: self.find(f, through)? })
    }

    /// Take what the session changed outside [`Replica::run`] since the copy last looked as edits
    /// of its own, which can't be made again.
    fn capture(&mut self) {
        self.stay_in_document();
        self.collect(Made::outside());
    }

    /// Take the changes the session recorded since the copy last looked as edits of the copy's
    /// own: the last of them made by `made`, any before it outside a command.
    fn collect(&mut self, made: Made) {
        let changes: Vec<Arc<ChangeSet>> = match self.session.changes().since(self.seen) {
            Ok(changes) => changes.into_iter().map(|(_, cs)| cs).collect(),
            // More than the session keeps: what changed since what the copy expected.
            Err(_) => match self.expected() {
                Some(doc) => vec![Arc::new(ChangeSet::between(&doc, &self.session.doc))],
                None => {
                    self.read_again();
                    return;
                }
            },
        };
        self.seen = self.session.changes().version();
        let n = changes.len();
        let mut made = made;
        made.blocks = changes.iter().flat_map(|cs| new_blocks(cs)).collect();
        for (i, cs) in changes.into_iter().enumerate() {
            if cs.is_empty() {
                continue;
            }
            let made = if i + 1 == n { made.clone() } else { Made::outside() };
            let serial = self.serial();
            self.pending.push(Unit { change: (*cs).clone(), made: vec![made], serial });
        }
    }

    /// Bring the room's document back as the session's active one, if a command made another
    /// one active (scopes keep commands that can from copies; this is a net under them).
    fn stay_in_document(&mut self) {
        if self.session.document_id() == self.document || self.session.return_to(self.document) {
            return;
        }
        // The room's document was closed: the copy starts again from the hub's.
        let (_, doc) = self.room.document();
        self.session.set_document(doc);
        self.document = self.session.document_id();
        self.seen = self.session.changes().version();
        self.read_again();
    }

    /// What the session's document should be: the version the copy got last, with the copy's
    /// own edits made in it. `None` when they don't apply.
    fn expected(&self) -> Option<Document> {
        let mut doc = self.confirmed.clone();
        for u in &self.pending {
            doc.apply(&u.change).ok()?;
        }
        Some(doc)
    }

    fn serial(&mut self) -> u64 {
        self.serials = self.serials.wrapping_add(1);
        self.serials
    }

    /// Whether the copy is as it should be: its session holds the version it got last with its
    /// own edits made in it, under the same block ids, and that version is the hub's when
    /// nothing came since.
    #[cfg(test)]
    pub(crate) fn check(&self) -> Result<(), String> {
        let doc = self.expected().ok_or("the copy's own edits don't apply to the version it got")?;
        if doc != self.session.doc {
            return Err("the session holds other content than the copy's version with its edits".into());
        }
        if doc.block_ids() != self.session.doc.block_ids() {
            return Err("the session holds its content under other block ids".into());
        }
        let (version, hub) = self.room.document();
        if version == self.received && (hub != self.confirmed || hub.block_ids() != self.confirmed.block_ids()) {
            return Err(format!("the copy's version {version} isn't the hub's"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{NAMES_COMMENTS, READS_INPUTS};

    #[test]
    fn the_commands_that_read_more_than_the_document_or_name_comments_are_commands() {
        let s = crate::Session::new(wordcraft_doc::Document::new());
        for id in READS_INPUTS.iter().chain(NAMES_COMMENTS) {
            assert!(s.registry.get(id).is_some(), "{id}");
        }
    }
}

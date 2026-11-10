//! What a turn tells the model about the document: the selection, the rest of the paragraphs it
//! starts and ends in, the paragraphs around it and the outline, as text, held to hard caps
//! however large the document or the selection is.

use wordcraft_doc::{Block, Blocks, Document, Paragraph, Path, Pos, StoryRef, para::OBJ};
use wordcraft_engine::Session;

use serde::{Deserialize, Serialize};

use crate::backend::{cut, label};

/// How much of the document a turn's context may carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    /// Most bytes of the selected text.
    pub selection_bytes: usize,
    /// Most paragraphs the selected text is read from.
    pub selection_paragraphs: usize,
    /// Most paragraphs before and after the selection.
    pub around: usize,
    /// Most bytes of each paragraph around the selection.
    pub paragraph_bytes: usize,
    /// Most headings of the outline.
    pub headings: usize,
    /// Most bytes of each heading.
    pub heading_bytes: usize,
    /// Most bytes of the whole context, as text ([`Context::to_text`]): the selection is kept
    /// first, then the rest of its paragraphs, the paragraphs nearest it and the outline.
    pub total_bytes: usize,
}

impl Default for Caps {
    fn default() -> Self {
        Caps {
            selection_bytes: 16 << 10,
            selection_paragraphs: 400,
            around: 3,
            paragraph_bytes: 2 << 10,
            headings: 60,
            heading_bytes: 200,
            total_bytes: 32 << 10,
        }
    }
}

/// What a turn gave the model of the document, as [`Context::rendered`] wrote it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sent {
    /// The document's title.
    pub document: String,
    /// Bytes of the selected text it carries; 0 for a caret.
    pub selection_bytes: u32,
    /// Whether the selected text is longer than what it carries.
    pub selection_cut: bool,
    /// Paragraphs before and after the selection it carries.
    pub before: u32,
    pub after: u32,
    /// Headings of the outline it carries.
    pub headings: u32,
    /// Bytes of it all, of `cap`.
    pub bytes: u32,
    pub cap: u32,
    /// The sources the person attached that the turn carried, beside this.
    pub knowledge: Vec<crate::knowledge::SentSource>,
    /// Bytes of what the person did with the assistant's changes ([`crate::Feedback`]), which the
    /// context starts with, beside `bytes`.
    pub feedback: u32,
    /// When the turn started, as a revision's date is written: a tracked change dated from then
    /// until the next turn started is this turn's.
    pub at: String,
    /// The person's preferences the turn carried, beside this.
    pub memory: Vec<crate::memory::SentPreference>,
    /// How many more are on but didn't fit.
    pub memory_left_out: u32,
    /// Why the turn recalled nothing from the person's cases, when that is so and the window knows
    /// before the turn runs: no case is bound to the document, or someone else can read it (the
    /// names). `None`: a case is bound and the conversation's thread looked ([`crate::Entry::Recalled`]).
    #[serde(default)]
    pub recall_withheld: Option<String>,
}

/// The document around the selection, as text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Context {
    /// The document's title or file name.
    pub title: String,
    /// The selected text, as it reads with tracked deletions left out; empty for a caret.
    pub selection: String,
    /// Whether the selected text was cut to the caps.
    pub selection_cut: bool,
    /// The text of the selection's first paragraph before it (of the caret's paragraph before
    /// the caret), on one line, its end kept when it is cut.
    pub lead: String,
    /// The text of the selection's last paragraph after it (of the caret's paragraph after the
    /// caret), on one line.
    pub trail: String,
    /// The paragraphs before the selection, nearest last.
    pub before: Vec<String>,
    /// The paragraphs after the selection, nearest first.
    pub after: Vec<String>,
    /// The body's headings, in order: level (1–9) and text.
    pub outline: Vec<(u8, String)>,
    /// Whether the outline has more headings than it carries.
    pub outline_cut: bool,
    /// Words in the document.
    pub words: usize,
}

impl Context {
    /// The context of `session`'s selection, held to `caps`.
    pub fn of(session: &Session, caps: &Caps) -> Context {
        let doc = &session.doc;
        let (a, b) = session.sel.ordered();
        let (selection, selection_cut) = selected(doc, &a, &b, caps);
        let (lead, trail) = edges(doc, &a, &b, caps);
        let (before, after) = around(doc, &a, &b, caps);
        let (outline, outline_cut) = outline(doc, caps);
        let title = doc.core.title.trim();
        let title = if title.is_empty() { session.file_name.as_deref().unwrap_or_default() } else { title };
        let title = cut(label(title), 200);
        Context { title, selection, selection_cut, lead, trail, before, after, outline, outline_cut, words: session.word_count() }
    }

    /// The context as text for the model, at most `caps.total_bytes` bytes. What doesn't fit is
    /// left out, the farthest first: the outline, then the paragraphs around the selection, then
    /// the rest of its paragraphs. The selection is cut only when it doesn't fit alone, and then
    /// the text says so, so it never ends in the middle of the selection.
    pub fn to_text(&self, caps: &Caps) -> String {
        self.rendered(caps).0
    }

    /// [`Context::to_text`], and what it carries: told by the one pass that decides, so the two
    /// can't disagree.
    pub fn rendered(&self, caps: &Caps) -> (String, Sent) {
        let total = caps.total_bytes;
        let mut head = String::new();
        if !self.title.is_empty() {
            head.push_str(&format!("Document: {}\n", quoted(&self.title)));
        }
        head.push_str(&format!("Words: {}\n", self.words));
        let mut left = total.saturating_sub(head.len());
        let Focus { text: focus, selection_bytes, selection_cut } = self.focus(left);
        left = left.saturating_sub(focus.len());
        // The paragraphs nearest the selection, taking turns before and after it, while they fit.
        let what = if self.selection.is_empty() { "caret" } else { "selection" };
        let (before_title, after_title) = (format!("\nBefore the {what}:\n"), format!("\nAfter the {what}:\n"));
        let (mut nb, mut na) = (0usize, 0usize);
        let (mut more_before, mut more_after) = (true, true);
        while more_before || more_after {
            if more_before {
                let next = self.before.len().checked_sub(nb.saturating_add(1)).and_then(|i| self.before.get(i));
                more_before = take(&mut left, next, &before_title, nb == 0);
                nb = nb.saturating_add(usize::from(more_before));
            }
            if more_after {
                more_after = take(&mut left, self.after.get(na), &after_title, na == 0);
                na = na.saturating_add(usize::from(more_after));
            }
        }
        // Then as much of the outline as fits.
        let (outline_title, more) = ("\nOutline:\n", "  (more headings)\n");
        let room = left.saturating_sub(outline_title.len().saturating_add(more.len()));
        let mut lines = String::new();
        let mut every_heading = true;
        let mut headings = 0usize;
        for (level, text) in &self.outline {
            let indent = "  ".repeat(usize::from(level.saturating_sub(1)).min(8));
            let line = format!("{indent}- {text}\n");
            if lines.len().saturating_add(line.len()) > room {
                every_heading = false;
                break;
            }
            lines.push_str(&line);
            headings = headings.saturating_add(1);
        }
        let mut out = head;
        if !lines.is_empty() {
            out.push_str(outline_title);
            out.push_str(&lines);
            if self.outline_cut || !every_heading {
                out.push_str(more);
            }
        }
        if nb > 0 {
            out.push_str(&before_title);
            for p in self.before.iter().skip(self.before.len().saturating_sub(nb)) {
                out.push_str(p);
                out.push('\n');
            }
        }
        out.push_str(&focus);
        if na > 0 {
            out.push_str(&after_title);
            for p in self.after.iter().take(na) {
                out.push_str(p);
                out.push('\n');
            }
        }
        let out = cut(out, total);
        let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        let sent = Sent {
            document: self.title.clone(),
            selection_bytes: count(selection_bytes),
            selection_cut,
            before: count(nb),
            after: count(na),
            headings: count(headings),
            bytes: count(out.len()),
            cap: count(total),
            knowledge: Vec::new(),
            feedback: 0,
            at: String::new(),
            memory: Vec::new(),
            memory_left_out: 0,
            recall_withheld: None,
        };
        (out, sent)
    }

    /// The selection (or where the caret is) with the rest of the paragraphs it is in, in at most
    /// `room` bytes, as far as the selection's own frame fits.
    fn focus(&self, room: usize) -> Focus {
        if self.selection.is_empty() {
            let bare = "\nThe caret is here (nothing is selected).\n";
            let words = "\nThe caret is here (nothing is selected), in a paragraph that reads  before it and  after it.\n";
            let (lead, trail) = edge_quotes(&self.lead, &self.trail, room.saturating_sub(words.len()));
            let text = match (lead.is_empty(), trail.is_empty()) {
                (true, true) => bare.to_string(),
                (true, false) => format!("\nThe caret is here (nothing is selected), at the start of a paragraph that goes on {trail}.\n"),
                (false, true) => format!("\nThe caret is here (nothing is selected), at the end of a paragraph that reads {lead}.\n"),
                (false, false) => {
                    format!("\nThe caret is here (nothing is selected), in a paragraph that reads {lead} before it and {trail} after it.\n")
                }
            };
            return Focus { text, selection_bytes: 0, selection_cut: false };
        }
        let (open, close) = ("\nSelected text:\n<<<\n", "\n>>>\n");
        let notice = "(The selection is longer: read the rest with get_text or inspect_document.)\n";
        let frame = open.len().saturating_add(close.len()).saturating_add(notice.len());
        let selection = cut(self.selection.clone(), room.saturating_sub(frame));
        let shortened = selection.len() < self.selection.len();
        let mut out = format!("{open}{selection}{close}");
        let selection_cut = self.selection_cut || shortened;
        if selection_cut {
            out.push_str(notice);
        }
        let (before, after) = ("Just before it, in the same paragraph: \n", "Just after it, in the same paragraph: \n");
        let room = room.saturating_sub(out.len().saturating_add(before.len()).saturating_add(after.len()));
        let (lead, trail) = edge_quotes(&self.lead, &self.trail, room);
        if !lead.is_empty() {
            out.push_str(&format!("Just before it, in the same paragraph: {lead}\n"));
        }
        if !trail.is_empty() {
            out.push_str(&format!("Just after it, in the same paragraph: {trail}\n"));
        }
        Focus { text: out, selection_bytes: selection.len(), selection_cut }
    }
}

/// The part of a context around the selection ([`Context::focus`]).
struct Focus {
    text: String,
    selection_bytes: usize,
    selection_cut: bool,
}

/// `words` as [`final_text`] reads: a model is given a tab or a line break in a paragraph as a space.
pub fn flat(words: &str) -> String {
    words.chars().map(flat_char).collect()
}

/// `doc`'s body as it reads with tracked deletions left out, a line for each paragraph: what a turn
/// gives the model, and so what its citations can quote.
pub fn final_text(doc: &Document) -> String {
    let mut text = String::new();
    for path in doc.para_paths(StoryRef::Body) {
        if let Some(p) = doc.para(StoryRef::Body, &path) {
            text.extend(final_chars(p, 0, p.text.len()).map(flat_char));
        }
        text.push('\n');
    }
    text
}

/// Where `words` are in the body as it reads with tracked deletions left out: from where the first
/// of them is to where the last ends, across anything deleted between them. Each paragraph is read
/// once to look and, only where they are, again to find their ends: a map of every byte would cost
/// many times the paragraph.
pub fn find_in_final(doc: &Document, words: &str) -> Option<(Pos, Pos)> {
    if words.is_empty() {
        return None;
    }
    let words = flat(words);
    for path in doc.para_paths(StoryRef::Body) {
        let Some(p) = doc.para(StoryRef::Body, &path) else { continue };
        let mut text = String::new();
        reading(p, |piece, _, _| text.push_str(piece));
        let Some(at) = text.find(&words) else { continue };
        let last = at.saturating_add(words.len());
        let (mut read, mut start, mut end) = (0usize, None, None);
        reading(p, |piece, off, len| {
            let next = read.saturating_add(piece.len());
            if start.is_none() && next > at {
                start = Some(off);
            }
            if end.is_none() && next >= last {
                end = Some(off.saturating_add(len));
            }
            read = next;
        });
        let pos = |off| Pos { story: StoryRef::Body, path: path.clone(), off };
        if let (Some(start), Some(end)) = (start, end) {
            return Some((pos(start), pos(end)));
        }
    }
    None
}

/// Call `visit` with each piece of `p` as it reads with tracked deletions left out, on one line: a
/// character, or an object's result, which [`final_text`] gives too, with where it is in the
/// paragraph and how long it is there.
fn reading(p: &Paragraph, mut visit: impl FnMut(&str, usize, usize)) {
    let mut objects = p.objects.iter();
    let mut piece = [0u8; 4];
    for (range, props) in p.run_ranges() {
        let run = p.text.get(range.clone()).unwrap_or_default();
        for (i, c) in run.char_indices() {
            let at = range.start.saturating_add(i);
            let result = (c == OBJ).then(|| objects.next().map(|o| o.plain_text()).unwrap_or_default());
            if props.del.is_some() {
                continue;
            }
            match result {
                Some(result) => {
                    let flat: String = result.chars().map(flat_char).collect();
                    visit(&flat, at, c.len_utf8());
                }
                None => visit(flat_char(c).encode_utf8(&mut piece), at, c.len_utf8()),
            }
        }
    }
}

/// Count `line` (with `title` when it is the first of its part) against `left`, when there is
/// one and it fits: whether it was.
fn take(left: &mut usize, line: Option<&String>, title: &str, first: bool) -> bool {
    let Some(line) = line else { return false };
    let cost = line.len().saturating_add(1).saturating_add(if first { title.len() } else { 0 });
    if cost > *left {
        return false;
    }
    *left -= cost;
    true
}

/// `lead` and `trail` quoted, in at most `room` bytes between them: each gets half, and what one
/// doesn't need the other may have. The lead keeps its end, the part nearest the selection.
fn edge_quotes(lead: &str, trail: &str, room: usize) -> (String, String) {
    let half = room / 2;
    let lead_room = if quoted(trail).len() < half { room.saturating_sub(quoted(trail).len()) } else { half };
    let lead = quoted_in(lead, lead_room, true);
    let trail = quoted_in(trail, room.saturating_sub(lead.len()), false);
    (lead, trail)
}

/// `text` as a JSON string, so its quotes and backslashes can't be read as the context's own.
fn quoted(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_default()
}

/// `text` quoted ([`quoted`]) in at most `room` bytes: all of it when it fits, else its start
/// (or its end, `keep_end`) and "…". Empty when there is no text or no room for any.
fn quoted_in(text: &str, room: usize, keep_end: bool) -> String {
    if text.is_empty() {
        return String::new();
    }
    let whole = quoted(text);
    if whole.len() <= room {
        return whole;
    }
    // The quotes and "…" take 5 bytes. A byte of text takes at most two quoted (only `"` and
    // `\` are escaped: the text has no control characters), so if the first try is too long,
    // half the room always does.
    for keep in [room.saturating_sub(5), room.saturating_sub(5) / 2] {
        if keep == 0 {
            break;
        }
        let part = if keep_end { format!("…{}", tail(text, keep)) } else { format!("{}…", cut(text.to_string(), keep)) };
        let q = quoted(&part);
        if q.len() <= room {
            return q;
        }
    }
    String::new()
}

/// The last `max` bytes of `text` at most, from a character's start.
fn tail(text: &str, max: usize) -> &str {
    let mut start = text.len().saturating_sub(max);
    while start < text.len() && !text.is_char_boundary(start) {
        start = start.saturating_add(1);
    }
    text.get(start..).unwrap_or_default()
}

/// A control character (line break, tab) on one line, as a space.
fn flat_char(c: char) -> char {
    if c.is_control() { ' ' } else { c }
}

// Match the document's paragraph walk and text export depth.
const PARA_DEPTH: usize = 16;

/// Visit a range in either direction with one depth-bounded scratch path, stopping on false.
fn visit_paths(
    doc: &Document,
    story: StoryRef,
    range: (Option<&Path>, Option<&Path>),
    reverse: bool,
    mut visit: impl FnMut(&Path, &Paragraph) -> bool,
) {
    if let Some(blocks) = doc.story(story) {
        walk_paths(blocks, &mut Path::default(), range, reverse, &mut visit);
    }
}

fn walk_paths(
    blocks: &Blocks,
    path: &mut Path,
    range: (Option<&Path>, Option<&Path>),
    reverse: bool,
    visit: &mut impl FnMut(&Path, &Paragraph) -> bool,
) -> bool {
    if path.0.len() / 3 > PARA_DEPTH {
        return true;
    }
    let overlaps = |path: &Path| {
        let side = |bound: &Path| {
            let n = path.0.len().min(bound.0.len());
            path.0.get(..n).unwrap_or_default().cmp(bound.0.get(..n).unwrap_or_default())
        };
        range.0.is_none_or(|from| !side(from).is_lt()) && range.1.is_none_or(|to| !side(to).is_gt())
    };
    let index = |len: usize, i: usize| if reverse { len.saturating_sub(i.saturating_add(1)) } else { i };
    for i in 0..blocks.len() {
        let i = index(blocks.len(), i);
        let (Some(block), Ok(i)) = (blocks.get(i), u32::try_from(i)) else { continue };
        path.0.push(i);
        if overlaps(path) {
            match &**block {
                Block::Para(p) => {
                    if range.0.is_none_or(|from| &*path >= from) && range.1.is_none_or(|to| &*path <= to) && !visit(path, p) {
                        return false;
                    }
                }
                Block::Table(table) => {
                    for r in 0..table.rows.len() {
                        let r = index(table.rows.len(), r);
                        let (Some(row), Ok(r)) = (table.rows.get(r), u32::try_from(r)) else { continue };
                        path.0.push(r);
                        for c in 0..row.cells.len() {
                            let c = index(row.cells.len(), c);
                            let (Some(cell), Ok(c)) = (row.cells.get(c), u32::try_from(c)) else { continue };
                            path.0.push(c);
                            if overlaps(path) && !walk_paths(&cell.blocks, path, range, reverse, visit) {
                                return false;
                            }
                            path.0.pop();
                        }
                        path.0.pop();
                    }
                }
            }
        }
        path.0.pop();
    }
    true
}

fn selection_paths(doc: &Document, a: &Pos, b: &Pos, limit: usize) -> (Vec<Path>, bool) {
    let (mut paths, mut cut) = (Vec::new(), false);
    if a.story == b.story {
        visit_paths(doc, a.story, (Some(&a.path), Some(&b.path)), false, |path, _| {
            // An empty ending slice carries the previous paragraph's mark, without counting as selected.
            let mark = a.path != b.path && path == &b.path && b.off == 0;
            if paths.len() >= limit.max(1) && !mark {
                cut = true;
                return false;
            }
            paths.push(path.clone());
            true
        });
    }
    (paths, cut)
}

/// The selected text from `a` to `b`, read from at most `caps.selection_paragraphs`
/// paragraphs, cut to `caps.selection_bytes`; and whether it was cut.
fn selected(doc: &Document, a: &Pos, b: &Pos, caps: &Caps) -> (String, bool) {
    if a == b {
        return (String::new(), false);
    }
    let (paths, cut_short) = selection_paths(doc, a, b, caps.selection_paragraphs);
    let end = if cut_short {
        paths
            .last()
            .and_then(|path| doc.para(a.story, path).map(|p| Pos { story: a.story, path: path.clone(), off: p.text.len() }))
            .unwrap_or_else(|| b.clone())
    } else {
        b.clone()
    };
    let (text, cut_here) = prefix(&mut selection_chars(doc, a, &end, paths).peekable(), caps.selection_bytes);
    (text, cut_short || cut_here)
}

/// The rest of the paragraphs the selection starts and ends in: the text of `a`'s paragraph
/// before `a` (its end kept) and of `b`'s paragraph after `b`, each cut to
/// `caps.paragraph_bytes` and on one line. A selection that ends at a paragraph's start (Select
/// Paragraph takes the paragraph mark, #418) has no trail: that paragraph comes after it.
fn edges(doc: &Document, a: &Pos, b: &Pos, caps: &Caps) -> (String, String) {
    // No more of a paragraph is read than this, however long it is.
    let span = caps.paragraph_bytes.saturating_mul(4);
    let lead = match doc.para(a.story, &a.path) {
        Some(p) if a.off > 0 && caps.paragraph_bytes > 0 => {
            let to = floor_boundary(&p.text, a.off.min(p.text.len()));
            let from = floor_boundary(&p.text, to.saturating_sub(span));
            // The source span bounds these references; field results stay borrowed and read backwards.
            let parts: Vec<_> = final_parts(p, from, to).collect();
            let chars = parts.iter().rev().flat_map(|part| part.chars().rev()).map(flat_char);
            let text = prefix(&mut chars.peekable(), caps.paragraph_bytes).0;
            text.chars().rev().collect::<String>().trim_start().to_string()
        }
        _ => String::new(),
    };
    let ends_at_a_paragraph_start = a != b && b.off == 0;
    let trail = match doc.para(b.story, &b.path) {
        Some(p) if !ends_at_a_paragraph_start && b.off < p.text.len() && caps.paragraph_bytes > 0 => {
            let from = floor_boundary(&p.text, b.off);
            let to = ceil_boundary(&p.text, from.saturating_add(span).min(p.text.len()));
            prefix(&mut final_chars(p, from, to).map(flat_char).peekable(), caps.paragraph_bytes).0.trim_end().to_string()
        }
        _ => String::new(),
    };
    (lead, trail)
}

/// The nearest character start at or before `i` in `text`.
fn floor_boundary(text: &str, i: usize) -> usize {
    let mut i = i.min(text.len());
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// The nearest character start at or after `i` in `text`.
fn ceil_boundary(text: &str, i: usize) -> usize {
    let mut i = i.min(text.len());
    while i < text.len() && !text.is_char_boundary(i) {
        i = i.saturating_add(1);
    }
    i
}

/// The paragraphs before `a` (nearest last) and after `b` (nearest first) in their story, at
/// most `caps.around` of each, each cut to `caps.paragraph_bytes`; empty ones skipped. A
/// selection that ends at a paragraph's start leaves that paragraph out of it: it is the first
/// one after.
fn around(doc: &Document, a: &Pos, b: &Pos, caps: &Caps) -> (Vec<String>, Vec<String>) {
    let (mut before, mut after) = (Vec::new(), Vec::new());
    if caps.around == 0 || caps.paragraph_bytes == 0 {
        return (before, after);
    }
    let anchor = |path: &Path| path.0.len() % 3 == 1 && path.depth() <= PARA_DEPTH && doc.para(a.story, path).is_some();
    let looked = caps.around.saturating_mul(8);
    if anchor(&a.path) {
        let mut visited = 0usize;
        visit_paths(doc, a.story, (None, Some(&a.path)), true, |path, p| {
            if path == &a.path {
                return true;
            }
            visited = visited.saturating_add(1);
            if let Some(text) = trimmed_prefix(p, caps.paragraph_bytes)
                && !text.is_empty()
            {
                before.push(text);
            }
            visited < looked && before.len() < caps.around
        });
        before.reverse();
    }
    if anchor(&b.path) {
        let mut visited = 0usize;
        visit_paths(doc, a.story, (Some(&b.path), None), false, |path, p| {
            if path == &b.path && (a == b || b.off != 0) {
                return true;
            }
            visited = visited.saturating_add(1);
            if let Some(text) = trimmed_prefix(p, caps.paragraph_bytes)
                && !text.is_empty()
            {
                after.push(text);
            }
            visited < looked && after.len() < caps.around
        });
    }
    (before, after)
}

/// The body's headings (paragraphs with an outline level), at most `caps.headings`, each cut to
/// `caps.heading_bytes`; and whether there are more.
fn outline(doc: &Document, caps: &Caps) -> (Vec<(u8, String)>, bool) {
    let mut out = Vec::new();
    for block in doc.body.iter() {
        let Some(p) = block.as_para() else { continue };
        let Some(level) = doc.styles.resolve_para(&p.props).outline_level else { continue };
        let Some(t) = trimmed_prefix(p, caps.heading_bytes) else { continue };
        if out.len() >= caps.headings {
            return (out, true);
        }
        out.push((level.saturating_add(1), t));
    }
    (out, false)
}

/// Borrow visible characters, including inline results, without cloning text or objects.
fn final_chars(p: &Paragraph, from: usize, to: usize) -> impl Iterator<Item = char> + '_ {
    final_parts(p, from, to).flat_map(str::chars)
}

fn final_parts(p: &Paragraph, from: usize, to: usize) -> impl Iterator<Item = &str> {
    let from = floor_boundary(&p.text, from);
    let to = floor_boundary(&p.text, to).max(from);
    let mut objects = p.objects.iter().skip(p.text.get(..from).unwrap_or_default().matches(OBJ).count());
    let mut runs = p
        .runs
        .iter()
        .scan(0usize, |start, run| {
            let end = start.saturating_add(run.len);
            let range = *start..end;
            *start = end;
            Some((range, &run.props))
        })
        .peekable();
    p.text.get(from..to).unwrap_or_default().char_indices().filter_map(move |(i, c)| {
        let i = from.saturating_add(i);
        while runs.peek().is_some_and(|(range, _)| range.end <= i) {
            runs.next();
        }
        let deleted = runs.peek().is_some_and(|(range, props)| range.contains(&i) && props.del.is_some());
        let text = if c == OBJ {
            objects.next().map(|o| o.plain_text()).unwrap_or_default()
        } else {
            p.text.get(i..i.saturating_add(c.len_utf8())).unwrap_or_default()
        };
        (!deleted).then_some(text)
    })
}

/// Read a UTF-8 prefix, inspecting only the first character beyond the byte budget.
fn prefix(chars: &mut std::iter::Peekable<impl Iterator<Item = char>>, max: usize) -> (String, bool) {
    let mut text = String::new();
    while let Some(&c) = chars.peek() {
        if text.len().saturating_add(c.len_utf8()) > max {
            return (text, true);
        }
        text.push(c);
        chars.next();
    }
    (text, false)
}

/// Trim as before, but retain only the prefix (even for a huge inline field result).
fn trimmed_prefix(p: &Paragraph, max: usize) -> Option<String> {
    let mut chars = final_chars(p, 0, p.text.len()).skip_while(|c| c.is_whitespace()).peekable();
    chars.peek()?;
    let (mut text, shortened) = prefix(&mut chars, max);
    if !shortened || (text.ends_with(char::is_whitespace) && !chars.any(|c| !c.is_whitespace())) {
        text.truncate(text.trim_end().len());
    }
    Some(text)
}

/// Drop trailing newlines lazily, as plain-text table cells do.
fn trim_newlines(chars: impl Iterator<Item = char>) -> impl Iterator<Item = char> {
    let mut chars = chars.peekable();
    let mut pending = 0usize;
    std::iter::from_fn(move || {
        if pending > 0 {
            pending -= 1;
            return Some('\n');
        }
        let c = chars.next()?;
        if c == '\n' {
            while chars.peek() == Some(&'\n') {
                chars.next();
                pending = pending.saturating_add(1);
            }
            if chars.peek().is_none() {
                pending = 0;
                return None;
            }
        }
        Some(c)
    })
}

/// The final text walk's table separators, with borrowed paragraphs and a bounded depth.
fn block_chars(block: &Block, depth: usize) -> Box<dyn Iterator<Item = char> + '_> {
    if depth > PARA_DEPTH {
        return Box::new(std::iter::empty());
    }
    match block {
        Block::Para(p) => Box::new(final_chars(p, 0, p.text.len()).chain(std::iter::once('\n'))),
        Block::Table(table) => Box::new(table.rows.iter().flat_map(move |row| {
            row.cells
                .iter()
                .enumerate()
                .flat_map(move |(i, cell)| {
                    (i > 0)
                        .then_some('\t')
                        .into_iter()
                        .chain(trim_newlines(blocks_chars(&cell.blocks, depth + 1)).map(|c| if c == '\n' { ' ' } else { c }))
                })
                .chain(std::iter::once('\n'))
        })),
    }
}

fn blocks_chars(blocks: &Blocks, depth: usize) -> impl Iterator<Item = char> + '_ {
    blocks.iter().flat_map(move |block| block_chars(block, depth))
}

/// Recognize a fully selected table without collecting every paragraph in its story.
fn selected_table<'a>(doc: &'a Document, a: &Pos, b: &Pos) -> Option<&'a Block> {
    if a.story != b.story || a.off != 0 || a.path.parent() == b.path.parent() || b.path.depth() > PARA_DEPTH {
        return None;
    }
    if b.off < doc.para(b.story, &b.path)?.text.len() {
        return None;
    }
    let mut n = 1usize;
    while n.saturating_add(3) <= a.path.0.len() {
        let pre = a.path.0.get(..n)?;
        if !b.path.0.starts_with(pre) {
            return None;
        }
        let table = Path(pre.to_vec());
        if let Some(block @ Block::Table(_)) = doc.block(a.story, &table) {
            let mut first = false;
            visit_paths(doc, a.story, (Some(&table), Some(&a.path)), false, |path, _| {
                first = path == &a.path;
                false
            });
            let mut last = true;
            visit_paths(doc, a.story, (Some(&b.path), None), false, |path, _| {
                if path == &b.path {
                    return true;
                }
                last = !path.0.starts_with(pre);
                false
            });
            if first && last {
                return Some(block);
            }
        }
        n = n.saturating_add(3);
    }
    None
}

/// Match copy_range's partial paragraphs and whole-table cases without copying a fragment.
fn selection_chars<'a>(doc: &'a Document, a: &Pos, b: &Pos, paths: Vec<Path>) -> impl Iterator<Item = char> + 'a {
    let chars: Box<dyn Iterator<Item = char> + 'a> = if a.story != b.story {
        Box::new(std::iter::empty())
    } else if a.path.parent() == b.path.parent() {
        let (first, last, from, to) = (a.path.last(), b.path.last(), a.off, b.off);
        Box::new(doc.container(a.story, &a.path).into_iter().flat_map(move |blocks| {
            blocks.iter().enumerate().skip(first).take(last.saturating_sub(first).saturating_add(1)).flat_map(move |(i, block)| match &**block {
                Block::Para(p) => Box::new(
                    final_chars(p, if i == first { from } else { 0 }, if i == last { to } else { p.text.len() }).chain(std::iter::once('\n')),
                ) as Box<dyn Iterator<Item = char>>,
                Block::Table(_) => block_chars(block, 0),
            })
        }))
    } else if let Some(table) = selected_table(doc, a, b) {
        block_chars(table, 0)
    } else {
        let (a, b) = (a.clone(), b.clone());
        Box::new(paths.into_iter().flat_map(move |path| {
            let Some(p) = doc.para(a.story, &path) else { return Box::new(std::iter::empty()) as Box<dyn Iterator<Item = char>> };
            Box::new(
                final_chars(p, if path == a.path { a.off } else { 0 }, if path == b.path { b.off } else { p.text.len() })
                    .chain(std::iter::once('\n')),
            ) as Box<dyn Iterator<Item = char>>
        }))
    };
    // Document::final_text removes exactly the final paragraph/row separator.
    let mut chars = chars.peekable();
    std::iter::from_fn(move || {
        let c = chars.next()?;
        if c == '\n' && chars.peek().is_none() { None } else { Some(c) }
    })
}

#[cfg(test)]
mod bounded_tests {
    use super::*;
    use std::cell::Cell;
    use wordcraft_doc::{Block, Paragraph, Run, StoryRef, Table, para_block, props::CharProps};

    #[test]
    fn prefix_stops_reading_when_the_utf8_budget_is_full() {
        let reads = Cell::new(0);
        let chars = "日本語".repeat(1000);
        let chars = chars.chars().inspect(|_| reads.set(reads.get() + 1));
        assert_eq!(prefix(&mut chars.peekable(), 7), ("日本".into(), true));
        assert_eq!(reads.get(), 3, "read only the prefix and the first character that does not fit");
        assert_eq!(prefix(&mut "日本".chars().peekable(), 6), ("日本".into(), false));
        assert_eq!(prefix(&mut "日".chars().peekable(), 0), (String::new(), true));
    }

    #[test]
    fn selected_prefix_matches_copying_for_ranges_deletions_and_tables() {
        let mut doc = Document::from_text("Start日本\nSecond\nLast");
        doc.para_mut(StoryRef::Body, &Path::top(1)).unwrap().format(1, 4, &|p| p.del = Some(1)).unwrap();
        let mut table = Table::new(2, 2, 400.0);
        for (i, row) in table.rows.iter_mut().enumerate() {
            for (j, cell) in row.cells.iter_mut().enumerate() {
                cell.blocks = vec![para_block(Paragraph::with_text(&format!("Cell {i} {j}"), CharProps::default()))];
            }
        }
        doc.body.insert(2, std::sync::Arc::new(Block::Table(table)));
        let a = Pos::body(0, 2);
        let b = Pos::body(3, 2);
        let all = doc.para_paths(StoryRef::Body);
        let mut ranges = vec![(a, b), (Pos::body(0, 1), Pos::body(1, 0))];
        let inside: Vec<_> = all.iter().filter(|p| p.0.len() > 1).collect();
        let first = inside.first().unwrap();
        let last = inside.last().unwrap();
        ranges.push((Pos { story: StoryRef::Body, path: (*first).clone(), off: 0 }, Pos { story: StoryRef::Body, path: (*last).clone(), off: 8 }));
        ranges.push((Pos { story: StoryRef::Body, path: (*first).clone(), off: 2 }, Pos { story: StoryRef::Body, path: (*last).clone(), off: 3 }));
        for (a, b) in ranges {
            let expected = doc.copy_range(&a, &b).to_document().final_text(StoryRef::Body);
            for max in [0, 1, 7, 1000] {
                let caps = Caps { selection_bytes: max, ..Caps::default() };
                let (actual, shortened) = selected(&doc, &a, &b, &caps);
                assert_eq!(actual, cut(expected.clone(), max), "{a:?}..{b:?}, cap {max}");
                assert_eq!(shortened, actual.len() < expected.len());
            }
        }
    }

    #[test]
    fn context_reads_inline_results_without_deletions_and_trims_neighbors() {
        let mut doc = Document::from_text("before\nselected\nafter");
        let p = doc.para_mut(StoryRef::Body, &Path::top(0)).unwrap();
        p.text = "隠す\u{fffc} end  ".into();
        p.runs = vec![
            Run { len: 6, props: CharProps { del: Some(1), ..CharProps::default() } },
            Run { len: p.text.len() - 6, props: CharProps::default() },
        ];
        p.objects = vec![wordcraft_doc::InlineObject::Field {
            instr: String::new(),
            result: "  日本語 ".repeat(1000),
            locked: false,
            dirty: false,
            code: false,
            form: String::new(),
        }];
        p.props.outline_level = Some(0);
        let caps = Caps { paragraph_bytes: 7, heading_bytes: 7, ..Caps::default() };
        let a = Pos::body(1, 0);
        let b = Pos::body(1, 8);
        assert_eq!(around(&doc, &a, &b, &caps).0, ["日本"]);
        assert_eq!(outline(&doc, &caps).0, [(1, "日本".into())]);
        let p = doc.para(StoryRef::Body, &Path::top(0)).unwrap();
        let a = Pos::body(0, 6);
        let b = Pos::body(0, 9);
        let caps = Caps { selection_bytes: 7, ..caps };
        assert_eq!(selected(&doc, &a, &b, &caps), (cut(p.objects[0].plain_text().to_string(), 7), true));
        let expected_lead: String = p.objects[0].plain_text().chars().rev().take(7).collect();
        let expected_lead: String = expected_lead.chars().rev().collect();
        let (lead, _) = edges(&doc, &b, &b, &caps);
        assert_eq!(lead, tail(&expected_lead, caps.paragraph_bytes).trim_start());
        let (_, trail) = edges(&doc, &a, &a, &caps);
        assert_eq!(trail, cut(p.objects[0].plain_text().to_string(), caps.paragraph_bytes).trim_end());
    }

    #[test]
    fn paragraph_caps_preserve_marks_and_trimmed_prefixes_preserve_whitespace() {
        let doc = Document::from_text("first\n日本語\nlast");
        let a = Pos::body(0, 2);
        let b = Pos::body(2, 0);
        let caps = Caps { selection_paragraphs: 1, ..Caps::default() };
        assert_eq!(selected(&doc, &a, &b, &caps), ("rst".into(), true));
        let caps = Caps { selection_paragraphs: 2, ..caps };
        assert_eq!(selected(&doc, &a, &b, &caps), ("rst\n日本語\n".into(), false));
        for text in ["  a   ", "  a b  ", " 日本\t語\n ", "  ", ""] {
            let p = Paragraph::with_text(text, CharProps::default());
            for max in 0..15 {
                let expected = (!text.trim().is_empty()).then(|| cut(text.trim().to_string(), max));
                assert_eq!(trimmed_prefix(&p, max), expected, "{text:?}, cap {max}");
            }
        }
    }

    #[test]
    fn paragraph_walk_reuses_bounded_scratch_and_stops_at_the_requested_neighbor() {
        let doc = Document::from_text(&"paragraph\n".repeat(1000));
        let (mut visited, mut scratch) = (0usize, None);
        visit_paths(&doc, StoryRef::Body, (Some(&Path::top(400)), None), false, |path, _| {
            let address = path.0.as_ptr();
            if let Some(previous) = scratch {
                assert_eq!(address, previous, "one scratch path, without collecting the story");
            }
            scratch = Some(address);
            assert_eq!(path, &Path::top(400 + visited));
            visited += 1;
            visited < 3
        });
        assert_eq!(visited, 3);
    }

    #[test]
    fn selection_metadata_never_collects_beyond_the_paragraph_budget() {
        let doc = Document::from_text(&"paragraph\n".repeat(1000));
        let a = Pos::body(0, 0);
        let b = Pos::body(1000, 0);
        let (paths, cut) = selection_paths(&doc, &a, &b, 2);
        assert!(cut);
        assert!(paths.len() <= 3, "at most the budget plus its ending paragraph mark: {}", paths.len());
        assert_eq!(paths.first(), Some(&a.path));
    }

    #[test]
    fn neighbors_match_reference_across_tables_empty_paragraphs_and_anchors() {
        let mut doc = Document::from_text("first\n\nlast");
        let mut table = Table::new(2, 2, 300.0);
        for (i, row) in table.rows.iter_mut().enumerate() {
            for (j, cell) in row.cells.iter_mut().enumerate() {
                cell.blocks = vec![para_block(Paragraph::new()), para_block(Paragraph::with_text(&format!("Cell {i} {j}"), CharProps::default()))];
            }
        }
        doc.body.insert(1, std::sync::Arc::new(Block::Table(table)));
        let all = doc.para_paths(StoryRef::Body);
        for (i, path) in all.iter().enumerate() {
            for selected in [false, true] {
                for around_count in [0, 1, 3] {
                    let caps = Caps { around: around_count, ..Caps::default() };
                    let b = Pos { story: StoryRef::Body, path: path.clone(), off: 0 };
                    let a = if selected && i > 0 { Pos { path: all[i - 1].clone(), ..b.clone() } } else { b.clone() };
                    let index_a = all.binary_search(&a.path).unwrap();
                    let text = |path: &Path| trimmed_prefix(doc.para(StoryRef::Body, path).unwrap(), caps.paragraph_bytes).unwrap_or_default();
                    let mut before: Vec<_> =
                        all[..index_a].iter().rev().take(caps.around * 8).map(text).filter(|t| !t.is_empty()).take(caps.around).collect();
                    before.reverse();
                    let index_after = if a != b { i } else { i + 1 };
                    let after: Vec<_> =
                        all[index_after..].iter().take(caps.around * 8).map(text).filter(|t| !t.is_empty()).take(caps.around).collect();
                    assert_eq!(around(&doc, &a, &b, &caps), (before, after), "{a:?}..{b:?}");
                }
            }
        }
    }

    #[test]
    fn nested_table_selection_matches_copying_through_empty_cells_and_marks() {
        let mut doc = Document::from_text("before\nafter");
        let mut inner = Table::new(1, 2, 200.0);
        inner.rows[0].cells[1].blocks = vec![para_block(Paragraph::with_text("inner", CharProps::default()))];
        let mut outer = Table::new(2, 2, 400.0);
        outer.rows[0].cells[0].blocks.insert(0, std::sync::Arc::new(Block::Table(inner)));
        outer.rows[1].cells[0].blocks = vec![para_block(Paragraph::with_text("outer", CharProps::default()))];
        doc.body.insert(1, std::sync::Arc::new(Block::Table(outer)));
        let paths = doc.para_paths(StoryRef::Body);
        for (i, a) in paths.iter().enumerate() {
            for b in paths.iter().skip(i) {
                for off in [0, doc.para(StoryRef::Body, b).unwrap().len()] {
                    let (a, b) = (Pos::new(StoryRef::Body, a.clone(), 0), Pos::new(StoryRef::Body, b.clone(), off));
                    let table = doc.whole_table(&a, &b).and_then(|path| doc.block(a.story, &path));
                    assert_eq!(selected_table(&doc, &a, &b), table, "{a:?}..{b:?}");
                    let expected = doc.copy_range(&a, &b).to_document().final_text(StoryRef::Body);
                    assert_eq!(selected(&doc, &a, &b, &Caps::default()), (expected, false), "{a:?}..{b:?}");
                }
            }
        }
    }

    #[test]
    fn paragraph_walk_matches_document_depth_limit_in_both_directions() {
        let mut block = para_block(Paragraph::new());
        for _ in 0..18 {
            let mut table = Table::new(1, 1, 200.0);
            table.rows[0].cells[0].blocks.insert(0, block);
            block = std::sync::Arc::new(Block::Table(table));
        }
        let mut doc = Document::new();
        doc.body = vec![block];
        let expected = doc.para_paths(StoryRef::Body);
        assert_eq!(expected.iter().map(Path::depth).max(), Some(PARA_DEPTH));
        for reverse in [false, true] {
            let mut actual = Vec::new();
            visit_paths(&doc, StoryRef::Body, (None, None), reverse, |path, _| {
                actual.push(path.clone());
                true
            });
            if reverse {
                actual.reverse()
            }
            assert_eq!(actual, expected);
        }
    }
}

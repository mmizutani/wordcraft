//! What a turn tells the model about the document: the selection, the rest of the paragraphs it
//! starts and ends in, the paragraphs around it and the outline, as text, held to hard caps
//! however large the document or the selection is.

use wordcraft_doc::{Document, Path, Pos, StoryRef};
use wordcraft_engine::Session;

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
        let title = if title.is_empty() { session.file_name.clone().unwrap_or_default() } else { title.to_string() };
        let title = cut(label(&title), 200);
        Context { title, selection, selection_cut, lead, trail, before, after, outline, outline_cut, words: session.word_count() }
    }

    /// The context as text for the model, at most `caps.total_bytes` bytes. What doesn't fit is
    /// left out, the farthest first: the outline, then the paragraphs around the selection, then
    /// the rest of its paragraphs. The selection is cut only when it doesn't fit alone, and then
    /// the text says so, so it never ends in the middle of the selection.
    pub fn to_text(&self, caps: &Caps) -> String {
        let total = caps.total_bytes;
        let mut head = String::new();
        if !self.title.is_empty() {
            head.push_str(&format!("Document: {}\n", quoted(&self.title)));
        }
        head.push_str(&format!("Words: {}\n", self.words));
        let mut left = total.saturating_sub(head.len());
        let focus = self.focus(left);
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
        for (level, text) in &self.outline {
            let indent = "  ".repeat(usize::from(level.saturating_sub(1)).min(8));
            let line = format!("{indent}- {text}\n");
            if lines.len().saturating_add(line.len()) > room {
                every_heading = false;
                break;
            }
            lines.push_str(&line);
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
        cut(out, total)
    }

    /// The selection (or where the caret is) with the rest of the paragraphs it is in, in at most
    /// `room` bytes, as far as the selection's own frame fits.
    fn focus(&self, room: usize) -> String {
        if self.selection.is_empty() {
            let bare = "\nThe caret is here (nothing is selected).\n";
            let words = "\nThe caret is here (nothing is selected), in a paragraph that reads  before it and  after it.\n";
            let (lead, trail) = edge_quotes(&self.lead, &self.trail, room.saturating_sub(words.len()));
            return match (lead.is_empty(), trail.is_empty()) {
                (true, true) => bare.to_string(),
                (true, false) => format!("\nThe caret is here (nothing is selected), at the start of a paragraph that goes on {trail}.\n"),
                (false, true) => format!("\nThe caret is here (nothing is selected), at the end of a paragraph that reads {lead}.\n"),
                (false, false) => {
                    format!("\nThe caret is here (nothing is selected), in a paragraph that reads {lead} before it and {trail} after it.\n")
                }
            };
        }
        let (open, close) = ("\nSelected text:\n<<<\n", "\n>>>\n");
        let notice = "(The selection is longer: read the rest with get_text or inspect_document.)\n";
        let frame = open.len().saturating_add(close.len()).saturating_add(notice.len());
        let selection = cut(self.selection.clone(), room.saturating_sub(frame));
        let shortened = selection.len() < self.selection.len();
        let mut out = format!("{open}{selection}{close}");
        if self.selection_cut || shortened {
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
        out
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

/// `text` on one line: its control characters (line breaks, tabs) as spaces.
fn flat(text: &str) -> String {
    text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// The selected text from `a` to `b`, read from at most `caps.selection_paragraphs`
/// paragraphs, cut to `caps.selection_bytes`; and whether it was cut.
fn selected(doc: &Document, a: &Pos, b: &Pos, caps: &Caps) -> (String, bool) {
    if a == b {
        return (String::new(), false);
    }
    let paths = doc.para_paths_in(a, b);
    let mut end = b.clone();
    let mut cut_short = false;
    if paths.len() > caps.selection_paragraphs.max(1)
        && let Some(last) = paths.get(caps.selection_paragraphs.max(1) - 1)
        && let Some(p) = doc.para(a.story, last)
    {
        end = Pos { story: a.story, path: last.clone(), off: p.text.len() };
        cut_short = true;
    }
    let text = doc.copy_range(a, &end).to_document().final_text(StoryRef::Body);
    let was = text.len();
    let text = cut(text, caps.selection_bytes);
    let cut_here = text.len() < was;
    (text, cut_short || cut_here)
}

/// The rest of the paragraphs the selection starts and ends in: the text of `a`'s paragraph
/// before `a` (its end kept) and of `b`'s paragraph after `b`, each cut to
/// `caps.paragraph_bytes` and on one line. A selection that ends at a paragraph's start (Select
/// Paragraph takes the paragraph mark, #418) has no trail: that paragraph comes after it.
fn edges(doc: &Document, a: &Pos, b: &Pos, caps: &Caps) -> (String, String) {
    // No more of a paragraph is read than this, however long it is.
    let span = caps.paragraph_bytes.saturating_mul(4);
    let read = |from: &Pos, to: &Pos| flat(&doc.copy_range(from, to).to_document().final_text(StoryRef::Body));
    let lead = match doc.para(a.story, &a.path) {
        Some(p) if a.off > 0 && caps.paragraph_bytes > 0 => {
            let to = floor_boundary(&p.text, a.off.min(p.text.len()));
            let from = floor_boundary(&p.text, to.saturating_sub(span));
            let text = read(&Pos { off: from, ..a.clone() }, &Pos { off: to, ..a.clone() });
            tail(&text, caps.paragraph_bytes).trim_start().to_string()
        }
        _ => String::new(),
    };
    let ends_at_a_paragraph_start = a != b && b.off == 0;
    let trail = match doc.para(b.story, &b.path) {
        Some(p) if !ends_at_a_paragraph_start && b.off < p.text.len() && caps.paragraph_bytes > 0 => {
            let from = floor_boundary(&p.text, b.off);
            let to = ceil_boundary(&p.text, from.saturating_add(span).min(p.text.len()));
            cut(read(&Pos { off: from, ..b.clone() }, &Pos { off: to, ..b.clone() }), caps.paragraph_bytes).trim_end().to_string()
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
    let text = |path: &Path| doc.para(a.story, path).map(|p| cut(p.final_text().trim().to_string(), caps.paragraph_bytes)).unwrap_or_default();
    let all = doc.para_paths(a.story);
    // Paragraphs with no text don't count, but only so many are looked at.
    let looked = caps.around.saturating_mul(8);
    let mut before: Vec<String> = match all.binary_search(&a.path) {
        Ok(i) => all.get(..i).unwrap_or_default().iter().rev().take(looked).map(text).filter(|t| !t.is_empty()).take(caps.around).collect(),
        Err(_) => Vec::new(),
    };
    before.reverse();
    let first_after = |i: usize| if a != b && b.off == 0 { i } else { i.saturating_add(1) };
    let after = match all.binary_search(&b.path) {
        Ok(i) => all.get(first_after(i)..).unwrap_or_default().iter().take(looked).map(text).filter(|t| !t.is_empty()).take(caps.around).collect(),
        Err(_) => Vec::new(),
    };
    (before, after)
}

/// The body's headings (paragraphs with an outline level), at most `caps.headings`, each cut to
/// `caps.heading_bytes`; and whether there are more.
fn outline(doc: &Document, caps: &Caps) -> (Vec<(u8, String)>, bool) {
    let mut out = Vec::new();
    for block in doc.body.iter() {
        let Some(p) = block.as_para() else { continue };
        let Some(level) = doc.styles.resolve_para(&p.props).outline_level else { continue };
        let t = p.final_text();
        let t = t.trim();
        if t.is_empty() {
            continue;
        }
        if out.len() >= caps.headings {
            return (out, true);
        }
        out.push((level.saturating_add(1), cut(t.to_string(), caps.heading_bytes)));
    }
    (out, false)
}

//! What a turn tells the model about the document: the selection, the paragraphs around it and
//! the outline, as text, held to hard caps however large the document or the selection is.

use wordcraft_doc::{Document, Path, Pos, StoryRef};
use wordcraft_engine::Session;

use crate::backend::cut;

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
    /// Most bytes of the whole context, as text ([`Context::to_text`]).
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
        let (before, after) = around(doc, &a, &b, caps);
        let (outline, outline_cut) = outline(doc, caps);
        let title = doc.core.title.trim();
        let title = if title.is_empty() { session.file_name.clone().unwrap_or_default() } else { title.to_string() };
        Context { title: cut(title, 200), selection, selection_cut, before, after, outline, outline_cut, words: session.word_count() }
    }

    /// The context as text for the model, at most `caps.total_bytes` bytes.
    pub fn to_text(&self, caps: &Caps) -> String {
        let mut out = String::new();
        if !self.title.is_empty() {
            out.push_str(&format!("Document: {}\n", self.title));
        }
        out.push_str(&format!("Words: {}\n", self.words));
        if !self.outline.is_empty() {
            out.push_str("\nOutline:\n");
            for (level, text) in &self.outline {
                let indent = "  ".repeat(usize::from(level.saturating_sub(1)).min(8));
                out.push_str(&format!("{indent}- {text}\n"));
            }
            if self.outline_cut {
                out.push_str("  (more headings)\n");
            }
        }
        if !self.before.is_empty() {
            out.push_str("\nBefore the selection:\n");
            for p in &self.before {
                out.push_str(&format!("{p}\n"));
            }
        }
        if self.selection.is_empty() {
            out.push_str("\nThe caret is here (nothing is selected).\n");
        } else {
            out.push_str("\nSelected text:\n<<<\n");
            out.push_str(&self.selection);
            out.push_str("\n>>>\n");
            if self.selection_cut {
                out.push_str("(The selection is longer: read the rest with get_text or inspect_document.)\n");
            }
        }
        if !self.after.is_empty() {
            out.push_str("\nAfter the selection:\n");
            for p in &self.after {
                out.push_str(&format!("{p}\n"));
            }
        }
        cut(out, caps.total_bytes)
    }
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

/// The paragraphs before `a` (nearest last) and after `b` (nearest first) in their story, at
/// most `caps.around` of each, each cut to `caps.paragraph_bytes`; empty ones skipped.
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
    let after = match all.binary_search(&b.path) {
        Ok(i) => {
            all.get(i.saturating_add(1)..).unwrap_or_default().iter().take(looked).map(text).filter(|t| !t.is_empty()).take(caps.around).collect()
        }
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

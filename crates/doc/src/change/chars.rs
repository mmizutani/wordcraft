//! Edits inside a paragraph ([`ParaEdit`]): the characters an edit replaced, with their
//! formatting and objects, and the paragraph's own properties when they changed. Two edits of
//! other characters of the same paragraph, made at once, both go through
//! ([`ChangeSet::rebase`](super::ChangeSet::rebase)).

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{Container, Fingerprint, fingerprint, fingerprint_of};
use crate::para::{InlineObject, OBJ, Run};
use crate::props::{CharProps, ParaProps};
use crate::section::SectionProps;
use crate::{Block, BlockId, DocError, Paragraph, Result};

/// A paragraph an edit changed in place: it keeps its id and its place, and holds other
/// characters, formatting or properties.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParaEdit {
    /// The list the paragraph is in.
    pub container: Container,
    /// The paragraph.
    pub id: BlockId,
    /// The [`fingerprint`] of the paragraph the edit was made on. None once the change was
    /// moved past another edit of the same paragraph ([`ChangeSet::rebase`](super::ChangeSet::rebase)):
    /// the characters it replaces, and the properties when it changes them, are still checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub was: Option<Fingerprint>,
    /// The characters it replaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chars: Option<CharSplice>,
    /// The paragraph's own properties, when they changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<ParaShell>,
}

/// Characters an edit replaced in a paragraph: `removed` bytes of its text from byte `at`,
/// which held what `hash` is the [`Fingerprint`] of (as [`Chars`]), became `inserted`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CharSplice {
    pub at: usize,
    pub removed: usize,
    pub hash: Fingerprint,
    #[serde(default)]
    pub inserted: Chars,
}

/// A piece of a paragraph: its text, the formatting runs that cover it, and an object for each
/// U+FFFC in it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Chars {
    pub text: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<Run>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<InlineObject>,
}

/// A paragraph's own properties: its paragraph formatting, its mark's formatting and the
/// section it ends. `was` is the [`Fingerprint`] of those the edit replaced.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParaShell {
    pub was: Fingerprint,
    #[serde(default, skip_serializing_if = "ParaProps::is_empty")]
    pub props: ParaProps,
    #[serde(default, skip_serializing_if = "CharProps::is_empty")]
    pub mark: CharProps,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<Box<SectionProps>>,
}

/// A paragraph's own properties as [`ParaShell::was`] fingerprints them.
#[derive(Serialize)]
struct Shell<'a> {
    props: &'a ParaProps,
    mark: &'a CharProps,
    section: &'a Option<Box<SectionProps>>,
}

fn shell_of(p: &Paragraph) -> Fingerprint {
    fingerprint_of(&Shell { props: &p.props, mark: &p.mark, section: &p.section })
}

fn stale(what: String) -> DocError {
    DocError::Stale(what)
}

/// The edit that turns paragraph `was` into `now` (blocks of list `c`), when both are the same
/// paragraph (the same id) in the normal form edits keep paragraphs in ([`normal`]): otherwise
/// none, and the paragraph travels whole.
pub(super) fn para_edit(c: Container, was: &Arc<Block>, now: &Arc<Block>) -> Option<ParaEdit> {
    let (Block::Para(a), Block::Para(b)) = (&**was, &**now) else { return None };
    if !a.id.is_set() || a.id != b.id || !normal(a) || !normal(b) {
        return None;
    }
    let Paragraph { text, runs, props, mark, objects, section, rev: _, id: _ } = b;
    let chars = (*text != a.text || *runs != a.runs || *objects != a.objects).then(|| {
        let (start, end) = common(a, b);
        let (a_end, b_end) = (a.text.len().saturating_sub(end), b.text.len().saturating_sub(end));
        CharSplice {
            at: start,
            removed: a_end.saturating_sub(start),
            hash: fingerprint_of(&chars_of(a, start, a_end)),
            inserted: chars_of(b, start, b_end),
        }
    });
    let shell = (*props != a.props || *mark != a.mark || *section != a.section).then(|| ParaShell {
        was: shell_of(a),
        props: props.clone(),
        mark: mark.clone(),
        section: section.clone(),
    });
    Some(ParaEdit { container: c, id: b.id, was: Some(fingerprint(was)), chars, shell })
}

/// Whether `p` is in the form editing leaves a paragraph in ([`Paragraph::normalize`]): runs
/// that cover its text exactly, each ending on a character boundary, none empty and none with
/// the formatting of the one before; and an object for each U+FFFC. Then its runs follow from
/// its characters' formatting, so an edit of some characters leaves the same runs in every copy.
fn normal(p: &Paragraph) -> bool {
    let mut end = 0usize;
    let mut prev: Option<&CharProps> = None;
    for r in &p.runs {
        if r.len == 0 || prev == Some(&r.props) {
            return false;
        }
        end = match end.checked_add(r.len) {
            Some(e) if p.text.is_char_boundary(e) => e,
            _ => return false,
        };
        prev = Some(&r.props);
    }
    end == p.text.len() && objects_in(&p.text) == p.objects.len()
}

fn objects_in(s: &str) -> usize {
    s.chars().filter(|c| *c == OBJ).count()
}

/// Where each run of `runs` starts.
fn starts(runs: &[Run]) -> Vec<usize> {
    let mut at = 0usize;
    runs.iter()
        .map(|r| {
            let s = at;
            at = at.saturating_add(r.len);
            s
        })
        .collect()
}

/// The formatting of run `i` of `p`, or of its mark when it has no runs.
fn props(p: &Paragraph, i: usize) -> &CharProps {
    p.runs.get(i).map_or(&p.mark, |r| &r.props)
}

/// The run holding byte `off`, from run `i` on (offsets asked in increasing order). The last
/// run holds whatever is past the runs, as [`Paragraph::normalize`] pads them.
fn run_fwd(starts: &[usize], mut i: usize, off: usize) -> usize {
    while starts.get(i + 1).is_some_and(|s| *s <= off) {
        i += 1;
    }
    i
}

/// The run holding byte `off`, from run `i` back (offsets asked in decreasing order).
fn run_back(starts: &[usize], mut i: usize, off: usize) -> usize {
    while i > 0 && starts.get(i).is_some_and(|s| *s > off) {
        i -= 1;
    }
    i
}

/// How many bytes at the start and at the end of `a` and `b` hold the same characters with the
/// same formatting and objects. The end never reaches into the start.
fn common(a: &Paragraph, b: &Paragraph) -> (usize, usize) {
    let (sa, sb) = (starts(&a.runs), starts(&b.runs));
    let mut start = 0;
    let (mut ra, mut rb, mut k) = (0, 0, 0);
    let mut compared = None;
    for ((i, x), (_, y)) in a.text.char_indices().zip(b.text.char_indices()) {
        if x != y {
            break;
        }
        (ra, rb) = (run_fwd(&sa, ra, i), run_fwd(&sb, rb, i));
        if compared != Some((ra, rb)) {
            if props(a, ra) != props(b, rb) {
                break;
            }
            compared = Some((ra, rb));
        }
        if x == OBJ {
            if a.objects.get(k) != b.objects.get(k) {
                break;
            }
            k += 1;
        }
        start = i + x.len_utf8();
    }
    let room = a.text.len().min(b.text.len()).saturating_sub(start);
    let mut end = 0;
    let (mut ra, mut rb, mut k) = (sa.len().saturating_sub(1), sb.len().saturating_sub(1), 0);
    let mut compared = None;
    for ((i, x), (h, y)) in a.text.char_indices().rev().zip(b.text.char_indices().rev()) {
        if x != y || end + x.len_utf8() > room {
            break;
        }
        (ra, rb) = (run_back(&sa, ra, i), run_back(&sb, rb, h));
        if compared != Some((ra, rb)) {
            if props(a, ra) != props(b, rb) {
                break;
            }
            compared = Some((ra, rb));
        }
        if x == OBJ {
            let j = |p: &Paragraph| p.objects.len().checked_sub(k + 1);
            match (j(a).and_then(|j| a.objects.get(j)), j(b).and_then(|j| b.objects.get(j))) {
                (Some(o), Some(q)) if o == q => k += 1,
                _ => break,
            }
        }
        end += x.len_utf8();
    }
    (start, end)
}

/// The characters `a..b` of `p` (character boundaries), with their formatting and objects. The
/// last run covers whatever is past the runs, as [`Paragraph::normalize`] pads them.
fn chars_of(p: &Paragraph, a: usize, b: usize) -> Chars {
    let text = p.text.get(a..b).unwrap_or_default().to_string();
    let first = objects_in(p.text.get(..a).unwrap_or_default());
    let objects = p.objects.get(first..first.saturating_add(objects_in(&text))).map(<[_]>::to_vec).unwrap_or_default();
    let mut runs: Vec<Run> = Vec::new();
    let mut start = 0usize;
    let last = p.runs.len().saturating_sub(1);
    for (i, r) in p.runs.iter().enumerate() {
        let end = if i == last { p.text.len().max(start.saturating_add(r.len)) } else { start.saturating_add(r.len) };
        let (x, y) = (start.max(a), end.min(b));
        if y > x {
            match runs.last_mut() {
                Some(l) if l.props == r.props => l.len += y - x,
                _ => runs.push(Run { len: y - x, props: r.props.clone() }),
            }
        }
        start = end;
    }
    if runs.is_empty() && !text.is_empty() {
        runs.push(Run { len: text.len(), props: p.mark.clone() });
    }
    Chars { text, runs, objects }
}

impl Chars {
    /// Whether the runs cover the text exactly, on character boundaries, with an object for
    /// each U+FFFC.
    fn check(&self) -> Result<()> {
        let mut end = 0usize;
        for r in &self.runs {
            end =
                end.checked_add(r.len).filter(|e| self.text.is_char_boundary(*e)).ok_or_else(|| bad("a run past the text or inside a character"))?;
        }
        if end != self.text.len() {
            return Err(bad("runs that don't cover the text"));
        }
        if objects_in(&self.text) != self.objects.len() {
            return Err(bad("other than one object for each U+FFFC"));
        }
        Ok(())
    }
}

fn bad(what: &str) -> DocError {
    DocError::Invalid(format!("the characters an edit puts in have {what}"))
}

/// Make edit `e` in paragraph `p` (whose fingerprint the caller checked against `e.was`).
pub(super) fn edit_para(p: &mut Paragraph, e: &ParaEdit) -> Result<()> {
    let id = e.id;
    if let Some(s) = &e.shell
        && shell_of(p) != s.was
    {
        return Err(stale(format!("paragraph {id} has other properties")));
    }
    if let Some(c) = &e.chars {
        c.inserted.check()?;
        p.normalize();
        let end = c.at.checked_add(c.removed).filter(|end| c.at <= *end && p.text.is_char_boundary(c.at) && p.text.is_char_boundary(*end));
        let Some(end) = end else { return Err(stale(format!("paragraph {id} is shorter than the edit, or splits a character there"))) };
        if fingerprint_of(&chars_of(p, c.at, end)) != c.hash {
            return Err(stale(format!("paragraph {id} holds other characters where the edit was made")));
        }
        splice(p, c.at, end, &c.inserted)?;
    }
    if let Some(s) = &e.shell {
        p.props = s.props.clone();
        p.mark = s.mark.clone();
        p.section = s.section.clone();
        p.touch();
    }
    Ok(())
}

/// Replace characters `a..b` of `p` (normal, with `a..b` on character boundaries) with `ins`
/// (checked).
fn splice(p: &mut Paragraph, a: usize, b: usize, ins: &Chars) -> Result<()> {
    let first = objects_in(p.text.get(..a).unwrap_or_default());
    let past = first.saturating_add(objects_in(p.text.get(a..b).unwrap_or_default()));
    if past > p.objects.len() {
        return Err(DocError::Invalid(format!("paragraph {} has fewer objects than its text holds", p.id)));
    }
    let mut runs: Vec<Run> = Vec::with_capacity(p.runs.len() + ins.runs.len() + 1);
    let mut start = 0usize;
    for r in &p.runs {
        let end = start.saturating_add(r.len);
        if end.min(a) > start {
            runs.push(Run { len: end.min(a) - start, props: r.props.clone() });
        }
        start = end;
    }
    runs.extend(ins.runs.iter().cloned());
    let mut start = 0usize;
    for r in &p.runs {
        let end = start.saturating_add(r.len);
        if end > start.max(b) {
            runs.push(Run { len: end - start.max(b), props: r.props.clone() });
        }
        start = end;
    }
    p.objects.splice(first..past, ins.objects.iter().cloned());
    p.text.replace_range(a..b, &ins.text);
    p.runs = runs;
    p.normalize();
    p.touch();
    Ok(())
}

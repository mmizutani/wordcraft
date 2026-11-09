use std::collections::BTreeSet;

use super::{CmdError, CmdResult, InlineObject, Paragraph, Pos, Selection, Session, StoryRef, Value, fmt_revisions, json, p, pos_json, sel_result};
use fmt_revisions::Which;

/// Accept or reject the tracked formatting and text of `from..to` in one paragraph (not its mark)
/// that `which` names.
fn resolve_runs(para: &mut wordcraft_doc::Paragraph, from: usize, to: usize, props: bool, accept: bool, which: Which) -> Result<(), CmdError> {
    // A paragraph's list numbering change goes with its formatting change.
    let props = props
        && match para.props.fmt_change.as_deref() {
            Some(ch) => which.has(Some(ch.rev)),
            None => which.has(para.props.num_change.as_ref().map(|n| n.rev)),
        };
    if props {
        fmt_revisions::resolve_para(&mut para.props, accept);
        para.touch();
    }
    let ranges: Vec<(usize, usize, bool, bool, bool)> = para
        .run_ranges()
        .filter(|(r, _)| r.end > from && r.start < to)
        .map(|(r, c)| (r.start.max(from), r.end.min(to), which.has(c.ins), which.has(c.del), which.has_change(c.fmt_change.as_deref())))
        .collect();
    for (a, b, ins, del, fmt) in ranges.into_iter().rev() {
        if (del && accept) || (ins && !accept) {
            para.delete(a, b)?;
        } else if ins || del || fmt {
            para.format(a, b, &|c| {
                if ins {
                    c.ins = None;
                }
                if del {
                    c.del = None;
                }
                if fmt {
                    fmt_revisions::resolve_char(c, accept);
                }
            })?;
        }
    }
    Ok(())
}

/// Accept or reject a revision range in one paragraph; `mark` includes its paragraph mark (and
/// the section break on it), `props` the paragraph's own formatting change. Only the changes
/// `which` names are resolved.
fn resolve_para(
    s: &mut Session,
    story: StoryRef,
    path: &wordcraft_doc::Path,
    (from, to): (usize, usize),
    (mark, props): (bool, bool),
    accept: bool,
    which: Which,
) -> Result<(), CmdError> {
    let para = s.doc.para_mut(story, path)?;
    resolve_runs(para, from, to, props, accept, which)?;
    if !mark {
        return Ok(());
    }
    resolve_mark_formatting(para, accept, which);
    let (ins, del) = (which.has(para.mark.ins), which.has(para.mark.del));
    if !ins && !del {
        return Ok(());
    }
    // An inserted paragraph mark that is rejected, or a deleted one that is accepted, goes away:
    // the next paragraph joins this one (the split is undone). A paragraph left empty goes away
    // whole, so the next one keeps its own style and numbering, as in Word. The mark at the end
    // of a story or cell can't go, so it just stops being a revision.
    if ((del && accept) || (ins && !accept)) && (crate::cmd::remove_empty_para(s, story, path)? || crate::cmd::join_next_para(s, story, path)?) {
        return Ok(());
    }
    let para = s.doc.para_mut(story, path)?;
    if ins {
        para.mark.ins = None;
    }
    if del {
        para.mark.del = None;
    }
    para.touch();
    Ok(())
}

/// Accept or reject the tracked formatting of a paragraph's mark and of the section break on it
/// that `which` names.
fn resolve_mark_formatting(para: &mut wordcraft_doc::Paragraph, accept: bool, which: Which) {
    let mark = which.has_change(para.mark.fmt_change.as_deref());
    let section = which.has_change(para.section.as_ref().and_then(|x| x.fmt_change.as_deref()));
    if mark {
        fmt_revisions::resolve_char(&mut para.mark, accept);
    }
    if section && let Some(sec) = para.section.as_deref_mut() {
        fmt_revisions::resolve_section(sec, accept);
    }
    if mark || section {
        para.touch();
    }
}

/// Accept or reject the changes of the table cell `path` is in: the table's, its row's and the
/// cell's own. Returns whether there were any.
fn resolve_cell(s: &mut Session, story: StoryRef, path: &wordcraft_doc::Path, accept: bool, which: Which) -> Result<bool, CmdError> {
    let Some((tp, r, c)) = path.cell() else { return Ok(false) };
    let has = s.doc.table(story, &tp).is_some_and(|t| {
        let row = t.rows.get(r);
        which.has(t.props.fmt_change.as_ref().map(|ch| ch.rev))
            || which.has(row.and_then(|x| x.props.fmt_change.as_ref()).map(|ch| ch.rev))
            || which.has(row.and_then(|x| x.cells.get(c)).and_then(|x| x.props.fmt_change.as_ref()).map(|ch| ch.rev))
    });
    if !has {
        return Ok(false);
    }
    let t = s.doc.table_mut(story, &tp)?;
    if which.has(t.props.fmt_change.as_ref().map(|ch| ch.rev)) {
        fmt_revisions::resolve_table(t, accept);
    }
    if let Some(row) = t.rows.get_mut(r) {
        if which.has(row.props.fmt_change.as_ref().map(|ch| ch.rev)) {
            fmt_revisions::resolve_row(row, accept);
        }
        if let Some(cell) = row.cells.get_mut(c)
            && which.has(cell.props.fmt_change.as_ref().map(|ch| ch.rev))
        {
            fmt_revisions::resolve_cell(cell, accept);
        }
    }
    Ok(true)
}

/// Resolve the tracked formatting of the paragraph mark at `at`, the caret. Whether there was any.
fn resolve_mark_at(s: &mut Session, at: &Pos, accept: bool, which: Which) -> Result<bool, CmdError> {
    let at_mark = s.doc.para(at.story, &at.path).is_some_and(|p| at.off == p.len() && which.has_change(p.mark.fmt_change.as_deref()));
    if at_mark {
        let para = s.doc.para_mut(at.story, &at.path)?;
        fmt_revisions::resolve_char(&mut para.mark, accept);
        para.touch();
    }
    Ok(at_mark)
}

/// Resolve the section's tracked change at `at`, the caret: the change is listed where the section
/// ends, at the end of its last paragraph or of the document. Whether there was one.
fn resolve_section_at(s: &mut Session, at: &Pos, accept: bool, which: Which) -> Result<bool, CmdError> {
    let in_paragraph = s.doc.para(at.story, &at.path).and_then(|p| p.section.as_ref().map(|x| (p.len(), x)));
    if let Some((len, section)) = in_paragraph
        && at.off == len
        && which.has(section.fmt_change.as_ref().map(|ch| ch.rev))
    {
        if let Some(section) = s.doc.para_mut(at.story, &at.path)?.section.as_mut() {
            fmt_revisions::resolve_section(section, accept);
        }
        return Ok(true);
    }
    if at.story == StoryRef::Body && *at == s.doc.end_of(StoryRef::Body) && which.has(s.doc.last_section.fmt_change.as_ref().map(|ch| ch.rev)) {
        fmt_revisions::resolve_section(&mut s.doc.last_section, accept);
        return Ok(true);
    }
    Ok(false)
}

/// A tracked change, for navigation and the Reviewing Pane: its range, kind (`insert`,
/// `delete`, `format`), revision, and for a formatting change the properties it set.
struct Change {
    a: Pos,
    b: Pos,
    kind: &'static str,
    rev: Option<u32>,
    props: Vec<Value>,
}

impl Change {
    fn format(a: Pos, b: Pos, rev: u32, props: Vec<Value>) -> Change {
        Change { a, b, kind: "format", rev: Some(rev), props }
    }
}

/// The tracked paragraph mark at the end of the paragraph at `path`, as a change range: from
/// the paragraph's end to the start of the next paragraph (or the end itself when none follows).
fn mark_change(s: &Session, story: StoryRef, path: &wordcraft_doc::Path) -> Option<Change> {
    let p = s.doc.para(story, path)?;
    let kind = run_kind(&s.doc, p.mark.ins, p.mark.del);
    if kind.is_empty() {
        return None;
    }
    let a = Pos { story, path: path.clone(), off: p.len() };
    let b = if crate::cmd::has_next_para(s, story, path) {
        Pos { story, path: path.with_last(path.last().saturating_add(1)), off: 0 }
    } else {
        a.clone()
    };
    Some(Change { a, b, kind, rev: p.mark.ins.or(p.mark.del), props: Vec::new() })
}

/// The kind of an insertion / deletion change: `insert`, `delete`, or for one end of a move
/// `moveTo` / `moveFrom`; empty when neither.
fn run_kind(doc: &wordcraft_doc::Document, ins: Option<u32>, del: Option<u32>) -> &'static str {
    match (ins, del) {
        (Some(_), _) if doc.move_name(ins).is_some() => "moveTo",
        (Some(_), _) => "insert",
        (None, Some(_)) if doc.move_name(del).is_some() => "moveFrom",
        (None, Some(_)) => "delete",
        _ => "",
    }
}

/// The moves in the body, for balloons: where each end starts, its author, and whether it is
/// the source (`true`) or the destination.
pub fn move_changes(s: &Session) -> Vec<(Pos, String, bool)> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for c in changes(s) {
        let from = match c.kind {
            "moveFrom" => true,
            "moveTo" => false,
            _ => continue,
        };
        let rev = c.rev.and_then(|r| s.doc.revisions.get(r as usize));
        let name = rev.and_then(|r| r.move_name.clone()).unwrap_or_default();
        // One balloon per end (an end runs on across paragraphs).
        if seen.insert((name, from)) {
            out.push((c.a, rev.map(|r| r.author.clone()).unwrap_or_default(), from));
        }
    }
    out
}

/// Accept or reject the tracked text and paragraph marks of the top-level paragraphs in a block
/// list that is not a story (a note separator) that `which` names.
fn resolve_loose_blocks(blocks: &mut wordcraft_doc::Blocks, accept: bool, which: Which) -> Result<(), CmdError> {
    for b in blocks.iter_mut() {
        let wordcraft_doc::Block::Para(p) = std::sync::Arc::make_mut(b) else { continue };
        let len = p.len();
        resolve_runs(p, 0, len, true, accept, which)?;
        resolve_mark_formatting(p, accept, which);
        // A mark can't be removed here (no neighbour to join), so it just stops being a revision.
        if which.has(p.mark.ins) {
            p.mark.ins = None;
        }
        if which.has(p.mark.del) {
            p.mark.del = None;
        }
        p.touch();
    }
    fmt_revisions::resolve_containers(blocks, accept, which, 0);
    Ok(())
}

/// Accept or reject the tracked changes `which` names, everywhere in the document.
fn resolve_all(s: &mut Session, accept: bool, which: Which) -> CmdResult {
    let stories: Vec<StoryRef> = std::iter::once(StoryRef::Body).chain(s.doc.parts.keys().map(|k| StoryRef::Part(*k))).collect();
    for st in stories {
        for path in s.doc.para_paths(st).into_iter().rev() {
            let len = s.doc.para(st, &path).map(|p| p.len()).unwrap_or(0);
            resolve_para(s, st, &path, (0, len), (true, true), accept, which)?;
        }
    }
    // Tables, rows, cells and sections.
    fmt_revisions::resolve_containers(&mut s.doc.body, accept, which, 0);
    for part in s.doc.parts.values_mut() {
        fmt_revisions::resolve_containers(&mut part.blocks, accept, which, 0);
    }
    if which.has_change(s.doc.last_section.fmt_change.as_deref()) {
        fmt_revisions::resolve_section(&mut s.doc.last_section, accept);
    }
    // The note separators are block lists of their own, not stories; they must not keep ids of
    // revisions that are about to be dropped.
    for sep in [&mut s.doc.footnote_separators, &mut s.doc.endnote_separators] {
        for blocks in [&mut sep.separator, &mut sep.continuation_separator, &mut sep.continuation_notice].into_iter().flatten() {
            resolve_loose_blocks(blocks, accept, which)?;
        }
    }
    // Every change resolved: the revisions go. Some resolved: the others still name theirs by
    // their place in the list, so it stays as it is.
    if matches!(which, Which::All) {
        s.doc.revisions.clear();
    }
    s.clamp_selection();
    sel_result(s)
}

/// Reject every tracked change of the session's document, as Reject All does.
pub(crate) fn reject_all(s: &mut Session) -> Result<(), CmdError> {
    resolve_all(s, false, Which::All).map(|_| ())
}

/// Accept or Reject All; with `stopTracking`, Track Changes is turned off as well.
pub(super) fn resolve_all_then(s: &mut Session, v: &Value, accept: bool) -> CmdResult {
    let out = match v.get("author") {
        None | Some(Value::Null) => resolve_all(s, accept, Which::All)?,
        Some(Value::String(author)) if !author.trim().is_empty() => {
            // One reviewer's changes (Accept All from one assistant): their revisions only.
            let author = author.trim();
            let theirs: BTreeSet<u32> =
                s.doc.revisions.iter().enumerate().filter(|(_, r)| r.author == author).filter_map(|(i, _)| u32::try_from(i).ok()).collect();
            resolve_all(s, accept, Which::Revisions(&theirs))?
        }
        Some(_) => return Err(CmdError::Params("`author` is the name of the reviewer whose changes to resolve".into())),
    };
    if p::bool(v, "stopTracking") == Some(true) {
        s.doc.settings.track_changes = false;
    }
    Ok(out)
}

/// Accept or Reject at the selection; with `next`, the next change is selected afterwards; with
/// `author`, only that reviewer's changes in the selection are resolved, and with `kind` too, only
/// their insertions, deletions or formatting: one text can carry a change of each.
pub(super) fn resolve_sel_then(s: &mut Session, v: &Value, accept: bool) -> CmdResult {
    use wordcraft_doc::RevisionKind;
    let next = p::bool(v, "next") == Some(true);
    let kind = match v.get("kind") {
        None | Some(Value::Null) => None,
        Some(Value::String(k)) if k == "insert" => Some(RevisionKind::Insert),
        Some(Value::String(k)) if k == "delete" => Some(RevisionKind::Delete),
        Some(Value::String(k)) if k == "format" => Some(RevisionKind::Format),
        Some(_) => return Err(CmdError::Params("`kind`: insert, delete or format".into())),
    };
    match v.get("author") {
        None | Some(Value::Null) if kind.is_none() => resolve_sel(s, accept, next, Which::All),
        Some(Value::String(author)) if !author.trim().is_empty() => {
            let author = author.trim();
            let revisions = s.doc.revisions.iter().enumerate();
            let theirs: BTreeSet<u32> = revisions
                .filter(|(_, r)| r.author == author && kind.is_none_or(|k| r.kind == k))
                .filter_map(|(i, _)| u32::try_from(i).ok())
                .collect();
            resolve_sel(s, accept, next, Which::Revisions(&theirs))
        }
        _ => Err(CmdError::Params("`author` is the name of the reviewer whose changes to resolve, and `kind` goes with it".into())),
    }
}

/// The span of the tracked change around `off` in `p`: its run, joined across the comment anchors
/// that split one revision's text into several runs.
fn revision_span(p: &Paragraph, off: usize) -> Option<std::ops::Range<usize>> {
    let runs: Vec<_> = p.run_ranges().collect();
    let i = runs.iter().position(|(r, c)| r.start <= off && off <= r.end && (c.ins.is_some() || c.del.is_some() || c.fmt_change.is_some()))?;
    let (mut start, mut end) = (runs[i].0.start, runs[i].0.end);
    let key = (runs[i].1.ins, runs[i].1.del);
    if key == (None, None) {
        return Some(start..end);
    }
    // Comment anchors split a revision's text into several runs; the revision goes on past them.
    let is_anchor = |k: usize| matches!(p.object_at(runs[k].0.start), Some(InlineObject::CommentStart { .. } | InlineObject::CommentEnd { .. }));
    let same = |k: usize| (runs[k].1.ins, runs[k].1.del) == key;
    let mut k = i;
    while k > 0 {
        if same(k - 1) {
            k -= 1;
        } else if is_anchor(k - 1) && k > 1 && same(k - 2) {
            k -= 2;
        } else {
            break;
        }
        start = runs[k].0.start;
    }
    let mut k = i;
    while k + 1 < runs.len() {
        if same(k + 1) {
            k += 1;
        } else if is_anchor(k + 1) && k + 2 < runs.len() && same(k + 2) {
            k += 2;
        } else {
            break;
        }
        end = runs[k].0.end;
    }
    Some(start..end)
}

fn resolve_sel(s: &mut Session, accept: bool, next: bool, which: Which) -> CmdResult {
    let (a, b) = s.sel.ordered();
    // A caret right before a tracked paragraph mark (and no tracked text around it): that mark.
    let mut caret_mark = None;
    // A caret in a paragraph whose own formatting changed (and no tracked text around it).
    let mut caret_props = None;
    let ranged = a != b;
    // One reviewer's row with no text of its own: someone else's run around the caret mustn't stand
    // in for the table's or the section's change it lists.
    if a == b && !matches!(which, Which::All) {
        // One row can list them together, and takes them all. The run before a mark mustn't stand
        // in for it either.
        let (cell, section) = (resolve_cell(s, a.story, &a.path, accept, which)?, resolve_section_at(s, &a, accept, which)?);
        if resolve_mark_at(s, &a, accept, which)? || cell || section {
            return if next { select_next_unresolved(s) } else { sel_result(s) };
        }
    }
    let (a, b) = if a == b {
        // The change at the caret: the run around it, else the paragraph mark right after it,
        // else the paragraph's formatting, else the table cell's.
        let found = s.doc.para_at(&a).and_then(|p| revision_span(p, a.off));
        let para_fmt = s.doc.para_at(&a).is_some_and(|p| p.props.fmt_change.is_some() || p.props.num_change.is_some());
        let mark_fmt = s.doc.para_at(&a).is_some_and(|p| a.off == p.len() && p.mark.fmt_change.is_some());
        match found {
            Some(r) => (Pos { off: r.start, ..a.clone() }, Pos { off: r.end, ..a }),
            None if mark_fmt || mark_change(s, a.story, &a.path).is_some_and(|m| m.a == a) => {
                caret_mark = Some(a.path.clone());
                (a.clone(), a)
            }
            None if para_fmt => {
                caret_props = Some(a.path.clone());
                (a.clone(), a)
            }
            None => {
                if resolve_cell(s, a.story, &a.path, accept, which)? {
                    return if next { select_next_unresolved(s) } else { sel_result(s) };
                }
                if resolve_section_at(s, &a, accept, which)? {
                    return if next { select_next_unresolved(s) } else { sel_result(s) };
                }
                return nav_change(s, 1);
            }
        }
    } else {
        (a, b)
    };
    // Moves with an end in the range: their other ends go with them.
    let mut moves = crate::cmd::moves::names_in(s, &a, &b, caret_mark.as_ref());
    if !matches!(which, Which::All) {
        // An author's own moves only, every end of them: a move with an end of anyone else's stays.
        let revisions = &s.doc.revisions;
        let theirs = |i: usize| u32::try_from(i).is_ok_and(|i| which.has(Some(i)));
        moves.retain(|name| {
            let mut ends = revisions.iter().enumerate().filter(|(_, r)| r.move_name.as_deref() == Some(name.as_str())).peekable();
            ends.peek().is_some() && ends.all(|(i, _)| theirs(i))
        });
    }
    for path in s.doc.paths_between(&a, &b).into_iter().rev() {
        let len = s.doc.para(a.story, &path).map(|p| p.len()).unwrap_or(0);
        let from = if path == a.path { a.off } else { 0 };
        let to = if path == b.path { b.off } else { len };
        // A paragraph's mark is in the selection when the selection goes on past it.
        let mark = path != b.path || caret_mark.as_ref() == Some(&path);
        // One reviewer's row is the text it names: a paragraph's own change only when that is all
        // of it, and a table's, a row's or a cell's has a row of its own at the caret.
        let all = matches!(which, Which::All);
        let touched = if all { path != b.path || to > from } else { from == 0 && to == len && len > 0 };
        let props = (ranged && touched) || caret_props.as_ref() == Some(&path);
        resolve_para(s, a.story, &path, (from, to), (mark, props), accept, which)?;
        if ranged && all {
            resolve_cell(s, a.story, &path, accept, which)?;
        }
    }
    crate::cmd::moves::resolve(s, &moves, accept)?;
    s.sel = Selection::caret(a);
    s.clamp_selection();
    if next { select_next_unresolved(s) } else { sel_result(s) }
}

fn changes(s: &Session) -> Vec<Change> {
    changes_in(s, StoryRef::Body)
}

fn changes_in(s: &Session, story: StoryRef) -> Vec<Change> {
    let mut out: Vec<Change> = Vec::new();
    let mk = |path: &wordcraft_doc::Path, off| Pos { story, path: path.clone(), off };
    for path in s.doc.para_paths(story) {
        let Some(p) = s.doc.para(story, &path) else { continue };
        // A table's, row's and cell's changes sit at the start of the cell's first paragraph.
        if path.last() == 0
            && let Some((tp, r, c)) = path.cell()
            && let Some(t) = s.doc.table(story, &tp)
        {
            let row = t.rows.get(r);
            let cell = row.and_then(|x| x.cells.get(c));
            let found = [
                (r == 0 && c == 0).then(|| t.props.fmt_change.as_ref().map(|ch| (ch.rev, fmt_revisions::table_diff(&t.props)))).flatten(),
                (c == 0).then(|| row.and_then(|x| x.props.fmt_change.as_ref().map(|ch| (ch.rev, fmt_revisions::row_diff(&x.props))))).flatten(),
                cell.and_then(|x| x.props.fmt_change.as_ref().map(|ch| (ch.rev, fmt_revisions::cell_diff(&x.props)))),
            ];
            for (rev, props) in found.into_iter().flatten() {
                out.push(Change::format(mk(&path, 0), mk(&path, 0), rev, props));
            }
        }
        if let Some(ch) = &p.props.fmt_change {
            out.push(Change::format(mk(&path, 0), mk(&path, p.len()), ch.rev, fmt_revisions::para_diff(&p.props)));
        }
        for (r, c) in p.run_ranges() {
            let kind = run_kind(&s.doc, c.ins, c.del);
            if !kind.is_empty() {
                out.push(Change { a: mk(&path, r.start), b: mk(&path, r.end), kind, rev: c.ins.or(c.del), props: Vec::new() });
            }
            let Some(ch) = &c.fmt_change else { continue };
            let props = fmt_revisions::char_diff(c);
            // Neighbouring runs changed the same way by the same revision are one change.
            if let Some(last) =
                out.last_mut().filter(|l| l.kind == "format" && l.rev == Some(ch.rev) && l.b == mk(&path, r.start) && l.props == props)
            {
                last.b = mk(&path, r.end);
            } else {
                out.push(Change::format(mk(&path, r.start), mk(&path, r.end), ch.rev, props));
            }
        }
        if let Some(ch) = &p.mark.fmt_change {
            let end = mk(&path, p.len());
            out.push(Change::format(end.clone(), end, ch.rev, fmt_revisions::char_diff(&p.mark)));
        }
        out.extend(mark_change(s, story, &path));
        if let Some(ch) = p.section.as_ref().and_then(|x| x.fmt_change.as_ref().map(|ch| (ch.rev, x))) {
            let end = mk(&path, p.len());
            out.push(Change::format(end.clone(), end, ch.0, fmt_revisions::section_diff(ch.1)));
        }
    }
    if story == StoryRef::Body
        && let Some(ch) = &s.doc.last_section.fmt_change
    {
        let end = s.doc.end_of(StoryRef::Body);
        out.push(Change::format(end.clone(), end, ch.rev, fmt_revisions::section_diff(&s.doc.last_section)));
    }
    out
}

/// The formatting changes in the body, for balloons: (position, author, properties set).
pub fn format_changes(s: &Session) -> Vec<(Pos, String, Vec<Value>)> {
    changes(s)
        .into_iter()
        .filter(|c| c.kind == "format")
        .map(|c| {
            let author = c.rev.and_then(|r| s.doc.revisions.get(r as usize)).map(|r| r.author.clone()).unwrap_or_default();
            (c.a, author, c.props)
        })
        .collect()
}

/// Whether the document has any tracked formatting change (cheap: stops at the first).
pub fn has_format_changes(doc: &wordcraft_doc::Document) -> bool {
    if doc.last_section.fmt_change.is_some() {
        return true;
    }
    doc.para_paths(StoryRef::Body).iter().any(|path| {
        doc.para(StoryRef::Body, path).is_some_and(|p| {
            p.props.fmt_change.is_some()
                || p.mark.fmt_change.is_some()
                || p.runs.iter().any(|r| r.props.fmt_change.is_some())
                || p.section.as_ref().is_some_and(|x| x.fmt_change.is_some())
                || path.cell().and_then(|(tp, r, c)| doc.table(StoryRef::Body, &tp).map(|t| (t, r, c))).is_some_and(|(t, r, c)| {
                    t.props.fmt_change.is_some()
                        || t.rows.get(r).is_some_and(|x| x.props.fmt_change.is_some() || x.cells.get(c).is_some_and(|x| x.props.fmt_change.is_some()))
                })
        })
    })
}

/// A tracked deletion in the body shown in a balloon (Show revisions in balloons).
#[derive(Clone, Debug, PartialEq)]
pub struct DeletionBalloon {
    /// Where the deleted text was: the balloon's anchor.
    pub at: Pos,
    pub author: String,
    /// The deleted text, a deleted paragraph mark as ¶, shortened to [`DELETION_TEXT_CHARS`].
    pub text: String,
}

/// At most this many characters of deleted text show in a balloon (a deleted page shows its start).
pub const DELETION_TEXT_CHARS: usize = 200;

/// At most this many deletion balloons (a document of many small deletions can't flood the area).
const MAX_DELETION_BALLOONS: usize = 500;

/// The tracked deletions in the body, for balloons, in document order. Deleted text running on
/// across runs, revisions and paragraph marks by the same author is one balloon. The source of a
/// move is left out: it has its "Moved from" balloon ([`move_changes`]).
pub fn deletion_balloons(s: &Session) -> Vec<DeletionBalloon> {
    let doc = &s.doc;
    let author = |rev: u32| doc.revisions.get(rev as usize).map(|r| r.author.clone()).unwrap_or_default();
    let mk = |path: &wordcraft_doc::Path, off| Pos { story: StoryRef::Body, path: path.clone(), off };
    let mut out = Vec::new();
    for path in doc.para_paths(StoryRef::Body) {
        let Some(p) = doc.para(StoryRef::Body, &path) else { continue };
        for (r, c) in p.run_ranges() {
            // Inserted and then deleted text is a deletion (it's drawn and left out as one).
            let Some(rev) = plain_deletion(doc, c.del) else { continue };
            push_deletion(&mut out, mk(&path, r.start), mk(&path, r.end), author(rev), p.text.get(r).unwrap_or(""));
        }
        if let Some(rev) = plain_deletion(doc, p.mark.del) {
            let at = mk(&path, p.len());
            let next = path.with_last(path.last().saturating_add(1));
            let end = if crate::cmd::has_next_para(s, StoryRef::Body, &path) { mk(&next, 0) } else { at.clone() };
            push_deletion(&mut out, at, end, author(rev), "¶");
        }
    }
    out.into_iter().map(|(d, _, _)| d).collect()
}

/// Add `author`'s deleted `text` (from `at` to `end`) to `out`: to the last balloon when that
/// deletion ends at `at` and is theirs, else as a new one. Each balloon carries where its deletion
/// ends and how many characters its text has.
fn push_deletion(out: &mut Vec<(DeletionBalloon, Pos, usize)>, at: Pos, end: Pos, author: String, text: &str) {
    let joins = out.last().is_some_and(|(d, e, _)| *e == at && d.author == author);
    if !joins {
        if out.len() >= MAX_DELETION_BALLOONS {
            return;
        }
        out.push((DeletionBalloon { at, author, text: String::new() }, end.clone(), 0));
    }
    let Some((d, e, chars)) = out.last_mut() else { return };
    *e = end;
    for c in text.chars() {
        if *chars > DELETION_TEXT_CHARS {
            break;
        }
        let c = match c {
            // Pictures, fields and other objects show no text of their own.
            wordcraft_doc::para::OBJ => continue,
            '\t' | wordcraft_doc::para::LINE_BREAK | wordcraft_doc::para::PAGE_BREAK | wordcraft_doc::para::COLUMN_BREAK => ' ',
            c if c.is_control() => continue,
            c => c,
        };
        *chars += 1;
        d.text.push(if *chars > DELETION_TEXT_CHARS { '…' } else { c });
    }
}

/// A deletion revision that is not the source of a move (`w:moveFrom` is a deletion revision
/// with a move name).
fn plain_deletion(doc: &wordcraft_doc::Document, del: Option<u32>) -> Option<u32> {
    del.filter(|_| doc.move_name(del).is_none())
}

/// Whether the body has any tracked deletion other than a move's source (cheap: stops at the
/// first).
pub fn has_deletions(doc: &wordcraft_doc::Document) -> bool {
    doc.para_paths(StoryRef::Body).iter().any(|path| {
        doc.para(StoryRef::Body, path)
            .is_some_and(|p| plain_deletion(doc, p.mark.del).is_some() || p.runs.iter().any(|r| plain_deletion(doc, r.props.del).is_some()))
    })
}

pub(super) fn nav_change(s: &mut Session, dir: i32) -> CmdResult {
    let list = changes(s);
    let caret = s.sel.ordered();
    let t = if dir > 0 {
        list.iter().find(|c| c.a >= caret.1 && (c.a > caret.0 || c.b > caret.1)).or(list.first())
    } else {
        list.iter().rev().find(|c| c.b <= caret.0 && (c.b < caret.1 || c.a < caret.0)).or(list.last())
    };
    if let Some(c) = t {
        s.sel = Selection { anchor: c.a.clone(), focus: c.b.clone() };
    }
    sel_result(s)
}

/// After a change at the caret is resolved: select the first change from the caret on, including
/// one that starts and ends there (an empty paragraph's, a paragraph mark's or a table's
/// formatting), else the first change in the document.
fn select_next_unresolved(s: &mut Session) -> CmdResult {
    let list = changes(s);
    let caret = s.sel.ordered().0;
    if let Some(c) = list.iter().find(|c| c.a >= caret).or(list.first()) {
        s.sel = Selection { anchor: c.a.clone(), focus: c.b.clone() };
    }
    sel_result(s)
}

/// How many tracked changes `author` made across the document: the assistant pane's Accept all and
/// Reject all of its changes (#15) are greyed out without any.
pub fn changes_by(s: &Session, author: &str) -> usize {
    let theirs = |rev: Option<u32>| rev.and_then(|r| s.doc.revisions.get(r as usize)).is_some_and(|r| r.author == author);
    std::iter::once(StoryRef::Body)
        .chain(s.doc.parts.keys().map(|id| StoryRef::Part(*id)))
        .map(|story| {
            let shown = changes_in(s, story).iter().filter(|c| theirs(c.rev)).count();
            // The reviewing list shows an insertion first; its overlapping deletion is still unresolved.
            let hidden_deletions = s
                .doc
                .para_paths(story)
                .iter()
                .filter_map(|path| s.doc.para(story, path))
                .map(|p| p.runs.iter().map(|r| &r.props).chain(std::iter::once(&p.mark)).filter(|c| c.ins.is_some() && theirs(c.del)).count())
                .sum::<usize>();
            shown + hidden_deletions
        })
        .sum()
}

/// Most characters of a change's or a comment's text [`change_rows`] and [`comment_rows`] keep.
pub const ROW_TEXT_CHARS: usize = 120;

/// One of an author's tracked changes in the document, for a list of them. Neighbouring runs of one
/// change are one here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeRow {
    /// `insert`, `delete`, `format`…, as the Reviewing Pane names it.
    pub kind: &'static str,
    pub a: Pos,
    pub b: Pos,
    /// Its text, at most [`ROW_TEXT_CHARS`] characters, objects left out.
    pub text: String,
}

/// The first `most` tracked changes `author` made across the document, in every story, and how many
/// more there are. Each one's text is read from its paragraph without copying it.
pub fn change_rows(s: &Session, author: &str, most: usize) -> (Vec<ChangeRow>, usize) {
    let theirs = |rev: u32| s.doc.revisions.get(rev as usize).is_some_and(|r| r.author == author);
    let mut rows: Vec<ChangeRow> = Vec::new();
    let mut more = 0usize;
    for story in std::iter::once(StoryRef::Body).chain(s.doc.parts.keys().map(|id| StoryRef::Part(*id))) {
        // The piece before, and whether its change is past the limit: what joins it is too.
        let mut before: Option<(&'static str, u32, Pos, Pos)> = None;
        let mut omitted = false;
        for c in changes_in(s, story) {
            let Some(rev) = c.rev.filter(|r| theirs(*r)) else { continue };
            // `changes_in` joins a formatting change's own pieces: neighbours are others.
            let joins = before.as_ref().is_some_and(|(kind, r, start, end)| {
                *kind == c.kind
                    && if c.kind == "format" {
                        // Accept or Reject over one place takes all its changes, so they are one row:
                        // a table's, a row's and a cell's at a caret, or a paragraph's and all its text.
                        *start == c.a && *end == c.b
                    } else {
                        *r == rev && *end == c.a
                    }
            });
            let same_words = joins && c.kind == "format";
            before = Some((c.kind, rev, c.a.clone(), c.b.clone()));
            if !joins {
                omitted = rows.len() >= most;
                if omitted {
                    more = more.saturating_add(1);
                } else {
                    rows.push(ChangeRow { kind: c.kind, a: c.a.clone(), b: c.a.clone(), text: String::new() });
                }
            }
            if omitted {
                continue;
            }
            let Some(row) = rows.last_mut() else { continue };
            row.b = c.b.clone();
            if c.a.path == c.b.path && !same_words {
                let piece = s.doc.para(c.a.story, &c.a.path).and_then(|p| p.text.get(c.a.off..c.b.off)).unwrap_or_default();
                let room = ROW_TEXT_CHARS.saturating_sub(row.text.chars().count());
                row.text.extend(piece.chars().filter(|ch| *ch != wordcraft_doc::para::OBJ).take(room));
            }
        }
        // Their deletion of someone's insertion shows in the list as the insertion: a change of
        // theirs all the same.
        for path in s.doc.para_paths(story) {
            let Some(p) = s.doc.para(story, &path) else { continue };
            let at = |off| Pos { story, path: path.clone(), off };
            let runs = p.run_ranges().filter(|(_, c)| c.ins.is_some() && c.del.is_some_and(theirs)).map(|(r, _)| {
                let text =
                    p.text.get(r.clone()).unwrap_or_default().chars().filter(|ch| *ch != wordcraft_doc::para::OBJ).take(ROW_TEXT_CHARS).collect();
                (at(r.start), at(r.end), text)
            });
            // A paragraph break of someone's that they took out.
            let mark =
                mark_change(s, story, &path).filter(|_| p.mark.ins.is_some() && p.mark.del.is_some_and(theirs)).map(|m| (m.a, m.b, String::new()));
            for (a, b, text) in runs.chain(mark) {
                if rows.len() >= most {
                    more = more.saturating_add(1);
                } else {
                    rows.push(ChangeRow { kind: "delete", a, b, text });
                }
            }
        }
    }
    (rows, more)
}

pub(super) fn list_changes(s: &mut Session, _: &Value) -> CmdResult {
    let list = changes(s);
    Ok(Value::Array(
        list.iter()
            .map(|c| {
                let rev = c.rev.and_then(|r| s.doc.revisions.get(r as usize));
                let mut v = json!({
                    "kind": c.kind, "start": pos_json(&c.a), "end": pos_json(&c.b),
                    "text": s.doc.copy_range(&c.a, &c.b).plain_text(),
                    "author": rev.map(|r| r.author.clone()), "date": rev.map(|r| r.date.clone()),
                });
                if c.kind == "format" {
                    v["description"] = json!(fmt_revisions::describe(&c.props));
                    v["props"] = json!(c.props);
                }
                // One end of a move: its name links it with the other end.
                if let Some(name) = rev.and_then(|r| r.move_name.as_deref()).filter(|_| c.kind.starts_with("move")) {
                    v["move"] = json!(name);
                    v["description"] = json!(if c.kind == "moveFrom" { "Moved from" } else { "Moved to" });
                }
                v
            })
            .collect(),
    ))
}

/// Track Changes Options: change the fields given, return them all. A per-user preference.
pub(super) fn tracking_options(s: &mut Session, v: &Value) -> CmdResult {
    use wordcraft_doc::Rgb;
    use wordcraft_layout::display::{BalloonMode, ChangeBar, CommentDisplay, DeleteMark, InsertMark};
    let mut m = s.prefs.markup.clone();
    let color = |k: &str, auto: &str, cur: Option<Rgb>| -> Result<Option<Rgb>, CmdError> {
        match p::str(v, k) {
            None => Ok(cur),
            Some(c) if c == auto => Ok(None),
            Some(c) => Rgb::parse(c).map(Some).ok_or_else(|| CmdError::Params(format!("`{k}`: \"{auto}\" or RRGGBB"))),
        }
    };
    let pick = |k: &str| p::str(v, k);
    for (k, f) in [
        ("comments", &mut m.comments),
        ("ink", &mut m.ink),
        ("insertionsDeletions", &mut m.insertions_deletions),
        ("formatting", &mut m.formatting),
        ("trackFormatting", &mut m.track_formatting),
    ] {
        if let Some(b) = p::bool(v, k) {
            *f = b;
        }
    }
    if let Some(x) = pick("balloons") {
        m.balloons = BalloonMode::parse(x).ok_or_else(|| CmdError::Params("`balloons`: revisions|inline|commentsAndFormatting".into()))?;
    }
    if let Some(x) = pick("commentDisplay") {
        m.comment_display = CommentDisplay::parse(x).ok_or_else(|| CmdError::Params("`commentDisplay`: margin|selected".into()))?;
    }
    if let Some(x) = pick("insertMark") {
        m.insert_mark = InsertMark::parse(x).ok_or_else(|| CmdError::Params("unknown `insertMark`".into()))?;
    }
    if let Some(x) = pick("deleteMark") {
        m.delete_mark = DeleteMark::parse(x).ok_or_else(|| CmdError::Params("unknown `deleteMark`".into()))?;
    }
    if let Some(x) = pick("moveFromMark") {
        m.move_from_mark = DeleteMark::parse(x).ok_or_else(|| CmdError::Params("unknown `moveFromMark`".into()))?;
    }
    if let Some(x) = pick("moveToMark") {
        m.move_to_mark = InsertMark::parse(x).ok_or_else(|| CmdError::Params("unknown `moveToMark`".into()))?;
    }
    if let Some(x) = pick("changedLines") {
        m.changed_lines = ChangeBar::parse(x).ok_or_else(|| CmdError::Params("unknown `changedLines`".into()))?;
    }
    m.insert_color = color("insertColor", "byAuthor", m.insert_color)?;
    m.delete_color = color("deleteColor", "byAuthor", m.delete_color)?;
    m.changed_lines_color = color("changedLinesColor", "auto", m.changed_lines_color)?;
    m.move_from_color = color("moveFromColor", "byAuthor", m.move_from_color)?;
    m.move_to_color = color("moveToColor", "byAuthor", m.move_to_color)?;
    // Track moves is the document's (`w:doNotTrackMoves`), so changing it is an undo step; the
    // command is pure, so it records the step itself, once every parameter has been accepted.
    // The other options are the user's own, so the command stays available under Restrict
    // Editing; this one edits the document, so it passes the same check as an edit and isn't
    // limited to an exception.
    if let Some(b) = p::bool(v, "trackMoves")
        && b != s.doc.settings.track_moves
    {
        if crate::cmd::protect::check_before(s, "review.trackingOptions", &json!({}))? {
            return Err(CmdError::Disabled(
                "review.trackingOptions: the document is protected; stop protection to change whether moves are tracked".into(),
            ));
        }
        s.checkpoint("Track Changes Options");
        s.doc.settings.track_moves = b;
        s.touch();
    }
    if m != s.prefs.markup {
        s.prefs.markup = m;
        // Hidden deletions leave the layout.
        s.relayout();
    }
    let mut out = tracking_json(&s.prefs.markup);
    out["trackMoves"] = json!(s.doc.settings.track_moves);
    Ok(out)
}

/// The Track Changes Options as `review.trackingOptions` takes them.
pub fn tracking_json(m: &wordcraft_layout::display::MarkupOptions) -> Value {
    let hex = |c: Option<wordcraft_doc::Rgb>, auto: &str| c.map(|c| c.hex()).unwrap_or_else(|| auto.to_string());
    json!({
        "comments": m.comments,
        "ink": m.ink,
        "insertionsDeletions": m.insertions_deletions,
        "formatting": m.formatting,
        "balloons": m.balloons.name(),
        "commentDisplay": m.comment_display.name(),
        "insertMark": m.insert_mark.name(),
        "insertColor": hex(m.insert_color, "byAuthor"),
        "deleteMark": m.delete_mark.name(),
        "deleteColor": hex(m.delete_color, "byAuthor"),
        "changedLines": m.changed_lines.name(),
        "changedLinesColor": hex(m.changed_lines_color, "auto"),
        "trackFormatting": m.track_formatting,
        "moveFromMark": m.move_from_mark.name(),
        "moveFromColor": hex(m.move_from_color, "byAuthor"),
        "moveToMark": m.move_to_mark.name(),
        "moveToColor": hex(m.move_to_color, "byAuthor"),
    })
}

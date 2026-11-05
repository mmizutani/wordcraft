//! What a participant's [`Role`] lets them change. A viewer changes nothing. A commenter adds
//! comments and replies, edits and deletes their own, and resolves any discussion (as in
//! Word). A suggester also edits, as tracked changes in their name, and withdraws their own;
//! they leave others' tracked changes for someone else to accept or reject. Editors and owners
//! change anything.
//!
//! A copy holds each command of its participant to this ([`Session::run`] undoes one that
//! does more), and the hub each change it is sent ([`super::Hub::submit_all`] sends it back).
//! Both look at the document before and after: with the comments in the participant's name
//! taken out of both and every discussion open, a commenter's must be the same, and a
//! suggester's must be the same once every tracked change is rejected, while others' tracked
//! changes stay as they were. They compare only what the change touched (the blocks from its
//! first difference to its last in each story, and the parts it changed), so a check takes as
//! long as the change, whatever the length of the document.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

use wordcraft_doc::para::OBJ;
use wordcraft_doc::props::{CellProps, ParaProps, RowProps, TableProps};
use wordcraft_doc::section::SectionProps;
use wordcraft_doc::{Block, Blocks, CharProps, Comment, Document, InlineObject, StoryRef};

use super::Role;
use crate::{Registry, Session};

/// Tables nested deeper than this aren't looked into (hostile documents).
const MAX_DEPTH: usize = 64;

/// Why `after` is more than participant `name`, with `role`, may make of `before`.
pub(crate) fn check(role: Role, name: &str, before: &Document, after: &Document) -> Result<(), String> {
    if role >= Role::Editor {
        return Ok(());
    }
    // The rest of the document is the same in both, and stays the same when compared.
    let (before, after) = touched(before, after);
    if before == after {
        return Ok(());
    }
    by_role(role, name, &before, &after)
}

fn by_role(role: Role, name: &str, before: &Document, after: &Document) -> Result<(), String> {
    match role {
        Role::Viewer => Err("a viewer changes nothing".into()),
        Role::Commenter => comments_only(before, after, name),
        Role::Suggester => suggestions_only(before, after, name),
        Role::Editor | Role::Owner => Ok(()),
    }
}

/// [`check`] on the whole documents, for tests to hold it to.
#[cfg(test)]
pub(crate) fn check_whole(role: Role, name: &str, before: &Document, after: &Document) -> Result<(), String> {
    if role >= Role::Editor || before == after {
        return Ok(());
    }
    by_role(role, name, before, after)
}

/// A commenter's change: comments of their own, and nothing else.
fn comments_only(before: &Document, after: &Document, name: &str) -> Result<(), String> {
    own_comments(before, after, name)?;
    if comparable(before, name)? != comparable(after, name)? {
        return Err("a commenter changes only comments of their own".into());
    }
    Ok(())
}

/// A suggester's change: comments of their own, and tracked changes in their name.
fn suggestions_only(before: &Document, after: &Document, name: &str) -> Result<(), String> {
    own_comments(before, after, name)?;
    // Others' tracked changes stay as they were; the suggester's own may go (withdrawn, or
    // undone), and those new, or changed, are in the suggester's name.
    for (i, r) in before.revisions.iter().enumerate() {
        if r.author != name && after.revisions.get(i) != Some(r) {
            return Err(format!("a suggester leaves {}'s tracked changes for someone else to accept or reject", r.author));
        }
    }
    if let Some(r) = after.revisions.iter().enumerate().find(|(i, r)| before.revisions.get(*i) != Some(*r) && r.author != name).map(|(_, r)| r) {
        return Err(format!("a suggester's tracked changes are in their own name, not {}'s", r.author));
    }
    let (before, after) = (comparable(before, name)?, comparable(after, name)?);
    // Others' tracked changes cover what they covered, as it was: none was accepted or
    // rejected, and what they cover wasn't changed but by tracked changes of the suggester's.
    let (was, now) = (covered(&before, name), covered(&after, name));
    for (i, r) in before.revisions.iter().enumerate() {
        let k = u32::try_from(i).unwrap_or(u32::MAX);
        if r.author != name && was.get(&k) != now.get(&k) {
            return Err(format!("a suggester leaves {}'s tracked changes for someone else to accept or reject", r.author));
        }
    }
    // With every tracked change rejected, nothing else changed: the suggester's edits are all
    // tracked changes.
    if rejected(before)? != rejected(after)? {
        return Err("a suggester's edits are tracked changes: Track Changes stays on for them".into());
    }
    Ok(())
}

/// Comments of others are as they were, but for being resolved or not; those added or
/// changed are `name`'s, and so are those taken away.
fn own_comments(before: &Document, after: &Document, name: &str) -> Result<(), String> {
    for (id, c) in &after.comments {
        match before.comments.get(id) {
            None if c.author != name => return Err(format!("comment {id} is in {}'s name, not {name}'s", c.author)),
            Some(old) if *old != Comment { resolved: old.resolved, ..c.clone() } && (old.author != name || c.author != name) => {
                return Err(format!("comment {id} is {}'s: only they change it", old.author));
            }
            _ => {}
        }
    }
    if let Some((id, c)) = before.comments.iter().find(|(id, c)| !after.comments.contains_key(id) && c.author != name) {
        return Err(format!("comment {id} is {}'s: only they take it away", c.author));
    }
    Ok(())
}

/// `before` and `after` with only what the change touched in their stories: in each story the
/// blocks [`window`] gives, and only the parts it changed. The rest of each document stays.
fn touched(before: &Document, after: &Document) -> (Document, Document) {
    let (mut b, mut a) = (before.clone(), after.clone());
    (b.body, a.body) = window(&before.body, &after.body);
    let ids: BTreeSet<u32> = before.parts.keys().chain(after.parts.keys()).copied().collect();
    for id in ids {
        if let (Some(pb), Some(pa)) = (b.parts.get_mut(&id), a.parts.get_mut(&id)) {
            (pb.blocks, pa.blocks) = window(&pb.blocks, &pa.blocks);
            if pb == pa {
                b.parts.remove(&id);
                a.parts.remove(&id);
            }
        }
    }
    (b, a)
}

/// The blocks of one story before and after a change that rejecting their tracked changes
/// may treat differently: from the first that differs to the last, widened past paragraphs
/// whose marks are tracked insertions (rejecting one joins its paragraph to the next). The
/// blocks around them are the same in both, and stay the same once rejected.
fn window(b: &[Arc<Block>], a: &[Arc<Block>]) -> (Blocks, Blocks) {
    // The blocks an edit didn't touch are the same allocations.
    let same = |x: &Arc<Block>, y: &Arc<Block>| Arc::ptr_eq(x, y) || x == y;
    let prefix = b.iter().zip(a).take_while(|(x, y)| same(x, y)).count();
    if prefix == b.len() && prefix == a.len() {
        return (Vec::new(), Vec::new());
    }
    let most = b.len().min(a.len()).saturating_sub(prefix);
    let suffix = b.iter().rev().zip(a.iter().rev()).take(most).take_while(|(x, y)| same(x, y)).count();
    let joins = |x: Option<&Arc<Block>>| matches!(x.map(|x| &**x), Some(Block::Para(p)) if p.mark.ins.is_some());
    let mut start = prefix;
    while start > 0 && joins(b.get(start - 1)) {
        start -= 1;
    }
    let (mut end_b, mut end_a) = (b.len().saturating_sub(suffix), a.len().saturating_sub(suffix));
    while end_b < b.len() && ((end_b > start && joins(b.get(end_b - 1))) || (end_a > start && joins(a.get(end_a - 1)))) {
        end_b += 1;
        end_a += 1;
    }
    let part = |v: &[Arc<Block>], to: usize| v.get(start..to).map(<[_]>::to_vec).unwrap_or_default();
    (part(b, end_b), part(a, end_a))
}

/// `doc` as what a participant called `name` changes in it is compared: without their
/// comments (their marks in the text, and their own text), and with every discussion open.
fn comparable(doc: &Document, name: &str) -> Result<Document, String> {
    let ids: BTreeSet<u32> = doc.comments.iter().filter(|(_, c)| c.author == name).map(|(id, _)| *id).collect();
    let mut d = doc.clone();
    for c in d.comments.values_mut() {
        c.resolved = false;
    }
    if ids.is_empty() {
        return Ok(d);
    }
    let stories: Vec<StoryRef> = std::iter::once(StoryRef::Body).chain(d.parts.keys().map(|k| StoryRef::Part(*k))).collect();
    for story in stories {
        for path in d.para_paths(story) {
            let marks: Vec<usize> = d
                .para(story, &path)
                .map(|p| {
                    p.object_offsets()
                        .into_iter()
                        .filter(|o| {
                            matches!(p.object_at(*o), Some(InlineObject::CommentStart { id } | InlineObject::CommentEnd { id }) if ids.contains(id))
                        })
                        .collect()
                })
                .unwrap_or_default();
            if marks.is_empty() {
                continue;
            }
            let para = d.para_mut(story, &path).map_err(|e| e.to_string())?;
            for o in marks.into_iter().rev() {
                para.delete(o, o.saturating_add(OBJ.len_utf8())).map_err(|e| e.to_string())?;
            }
        }
    }
    for id in &ids {
        if let Some(c) = d.comments.remove(id) {
            d.parts.remove(&c.part);
        }
    }
    Ok(d)
}

/// `doc` with every tracked change rejected (Reject All).
fn rejected(doc: Document) -> Result<Document, String> {
    static NO_COMMANDS: OnceLock<Arc<Registry>> = OnceLock::new();
    let registry = NO_COMMANDS.get_or_init(|| Arc::new(Registry::new(Vec::new()))).clone();
    let mut s = Session::with_registry(doc, registry);
    // Documents come from other participants: whatever they hold, a check never panics.
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| crate::cmd::review::reject_all(&mut s)));
    match out {
        Ok(Ok(())) => Ok(s.doc),
        Ok(Err(e)) => Err(format!("its tracked changes can't be rejected to check it: {e}")),
        Err(_) => Err("its tracked changes can't be rejected to check it".into()),
    }
}

/// What each tracked change covers, in document order: the characters (with the objects among
/// them) and paragraph marks it is on, with their formatting, and the properties of the
/// paragraphs, sections, tables, rows and cells it changed. The tracked changes of `name` on
/// them are taken back first (a deletion of theirs, or their formatting change), so
/// what others' cover looks as it did before they suggested anything.
fn covered(doc: &Document, name: &str) -> BTreeMap<u32, Vec<Covered>> {
    let own: BTreeSet<u32> = doc.revisions.iter().enumerate().filter(|(_, r)| r.author == name).filter_map(|(i, _)| u32::try_from(i).ok()).collect();
    let mut c = Coverage { own, by: BTreeMap::new() };
    c.blocks(&doc.body, 0);
    for part in doc.parts.values() {
        c.blocks(&part.blocks, 0);
    }
    for sep in [&doc.footnote_separators, &doc.endnote_separators] {
        for blocks in [&sep.separator, &sep.continuation_separator, &sep.continuation_notice].into_iter().flatten() {
            c.blocks(blocks, 0);
        }
    }
    if let Some(ch) = &doc.last_section.fmt_change {
        c.push(ch.rev, Covered::Section(Box::new(doc.last_section.clone())));
    }
    c.by
}

/// One thing a tracked change covers ([`covered`]).
#[derive(Clone, Debug, PartialEq)]
enum Covered {
    Chars(String, Box<CharProps>, Vec<InlineObject>),
    Mark(Box<CharProps>),
    Para(Box<ParaProps>),
    Section(Box<SectionProps>),
    Table(Box<TableProps>),
    Row(Box<RowProps>),
    Cell(Box<CellProps>),
}

struct Coverage {
    /// The revisions of the participant checked.
    own: BTreeSet<u32>,
    by: BTreeMap<u32, Vec<Covered>>,
}

impl Coverage {
    fn push(&mut self, rev: u32, what: Covered) {
        if self.own.contains(&rev) {
            return;
        }
        let list = self.by.entry(rev).or_default();
        // Characters split by the participant's own insertions, deletions or formatting are
        // one stretch, as they were.
        if let (Some(Covered::Chars(text, props, objects)), Covered::Chars(t, p, o)) = (list.last_mut(), &what)
            && props == p
        {
            text.push_str(t);
            objects.extend(o.iter().cloned());
            return;
        }
        list.push(what);
    }

    /// `c` with the participant's own deletion and formatting change on it taken back.
    fn as_before(&self, c: &CharProps) -> CharProps {
        let mut c = c.clone();
        if c.del.is_some_and(|r| self.own.contains(&r)) {
            c.del = None;
        }
        if let Some(ch) = c.fmt_change.take() {
            if self.own.contains(&ch.rev) {
                c = CharProps { link: c.link.take(), ins: c.ins, del: c.del, ..ch.old.formatting() };
            } else {
                c.fmt_change = Some(ch);
            }
        }
        c
    }

    fn chars(&mut self, c: &CharProps, what: impl Fn(CharProps) -> Covered) {
        let c = self.as_before(c);
        let revs = [c.ins, c.del, c.fmt_change.as_ref().map(|f| f.rev)];
        for rev in revs.into_iter().flatten() {
            self.push(rev, what(c.clone()));
        }
    }

    fn blocks(&mut self, blocks: &[Arc<Block>], depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        for b in blocks {
            match &**b {
                Block::Para(p) => {
                    let objects = p.object_offsets();
                    for (range, props) in p.run_ranges() {
                        let text = p.text.get(range.clone()).unwrap_or_default().to_string();
                        let here: Vec<InlineObject> = objects.iter().filter(|o| range.contains(o)).filter_map(|o| p.object_at(*o).cloned()).collect();
                        self.chars(props, |c| Covered::Chars(text.clone(), Box::new(c), here.clone()));
                    }
                    self.chars(&p.mark, |c| Covered::Mark(Box::new(c)));
                    for rev in [p.props.fmt_change.as_ref().map(|c| c.rev), p.props.num_change.as_ref().map(|c| c.rev)].into_iter().flatten() {
                        self.push(rev, Covered::Para(Box::new(p.props.clone())));
                    }
                    if let Some(section) = p.section.as_deref()
                        && let Some(ch) = &section.fmt_change
                    {
                        self.push(ch.rev, Covered::Section(Box::new(section.clone())));
                    }
                }
                Block::Table(t) => {
                    if let Some(ch) = &t.props.fmt_change {
                        self.push(ch.rev, Covered::Table(Box::new(t.props.clone())));
                    }
                    for row in &t.rows {
                        if let Some(ch) = &row.props.fmt_change {
                            self.push(ch.rev, Covered::Row(Box::new(row.props.clone())));
                        }
                        for cell in &row.cells {
                            if let Some(ch) = &cell.props.fmt_change {
                                self.push(ch.rev, Covered::Cell(Box::new(cell.props.clone())));
                            }
                            self.blocks(&cell.blocks, depth.saturating_add(1));
                        }
                    }
                }
            }
        }
    }
}

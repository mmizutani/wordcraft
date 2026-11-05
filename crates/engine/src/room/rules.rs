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
use wordcraft_doc::{Block, Blocks, CharProps, Comment, Document, InlineObject, PartKind, StoryRef, StyleSheet};

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
    // Others' tracked changes cover what they covered, as it was and where it was: none was
    // accepted, rejected or moved, and what they cover wasn't changed but by tracked changes
    // of the suggester's.
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
/// changed are `name`'s, and so are those taken away. A comment's text is its own: a new one's
/// is in a new comment part, which no other comment has, and a comment keeps its part. (A
/// change doesn't make a comment of theirs point at someone else's comment text, or a header,
/// which their own comments' texts are left out of the comparison as.)
fn own_comments(before: &Document, after: &Document, name: &str) -> Result<(), String> {
    for (id, c) in &after.comments {
        match before.comments.get(id) {
            None if c.author != name => return Err(format!("comment {id} is in {}'s name, not {name}'s", c.author)),
            Some(old) if *old != Comment { resolved: old.resolved, ..c.clone() } && (old.author != name || c.author != name) => {
                return Err(format!("comment {id} is {}'s: only they change it", old.author));
            }
            None if before.parts.contains_key(&c.part) || !after.parts.get(&c.part).is_some_and(|p| p.kind == PartKind::Comment) => {
                return Err(format!("comment {id}'s text is a comment part of its own"));
            }
            Some(old) if old.part != c.part => return Err(format!("comment {id}'s text stays where it is")),
            _ => {}
        }
        if before.comments.get(id) != Some(c) && after.comments.iter().any(|(other, o)| other != id && o.part == c.part) {
            return Err(format!("comment {id}'s text is a comment part of its own"));
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
    // Their comments' texts go with them: those in comment parts no one else's comment has.
    let others: BTreeSet<u32> = doc.comments.values().filter(|c| c.author != name).map(|c| c.part).collect();
    for id in &ids {
        if let Some(c) = d.comments.remove(id)
            && !others.contains(&c.part)
            && d.parts.get(&c.part).is_some_and(|p| p.kind == PartKind::Comment)
        {
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

/// What each tracked change covers, in document order, and where: the characters (with the
/// objects among them) and paragraph marks it is on, with their formatting, and the properties
/// of the paragraphs, sections, tables, rows and cells it changed. The tracked changes of
/// `name` on them are taken back first (a deletion of theirs, their formatting change, or a
/// paragraph they split), so what others' cover looks as it did before they suggested
/// anything, and is where it was ([`Place`]).
fn covered(doc: &Document, name: &str) -> BTreeMap<u32, Vec<(Place, Covered)>> {
    let own: BTreeSet<u32> = doc.revisions.iter().enumerate().filter(|(_, r)| r.author == name).filter_map(|(i, _)| u32::try_from(i).ok()).collect();
    let mut c = Coverage { own, by: BTreeMap::new(), styles: &doc.styles, at: Place::start(Story::Body) };
    c.blocks(&doc.body, 0);
    for (id, part) in &doc.parts {
        c.at = Place::start(Story::Part(*id));
        c.blocks(&part.blocks, 0);
    }
    let separators = [&doc.footnote_separators, &doc.endnote_separators]
        .into_iter()
        .flat_map(|sep| [&sep.separator, &sep.continuation_separator, &sep.continuation_notice]);
    for (i, blocks) in separators.enumerate() {
        if let Some(blocks) = blocks {
            c.at = Place::start(Story::Separator(i));
            c.blocks(blocks, 0);
        }
    }
    if let Some(ch) = &doc.last_section.fmt_change {
        c.at = Place::start(Story::LastSection);
        c.push(ch.rev, Covered::Section(Box::new(doc.last_section.clone())));
    }
    c.by
}

/// Where something a tracked change covers is ([`covered`]): in which story, in which of its
/// paragraphs (counted in order, through tables, as one once the participant's own split
/// marks are taken back) and at which byte of its text (without the participant's own
/// insertions). Taking back what the participant suggested moves nothing to another place.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Place {
    story: Story,
    para: u64,
    off: usize,
}

impl Place {
    fn start(story: Story) -> Place {
        Place { story, para: 0, off: 0 }
    }
}

/// A story [`covered`] walks.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Story {
    Body,
    Part(u32),
    /// A note separator or continuation notice, by its order.
    Separator(usize),
    LastSection,
}

/// One thing a tracked change covers ([`covered`]).
#[derive(Clone, Debug, PartialEq)]
enum Covered {
    Chars(String, Box<CharProps>, Vec<InlineObject>),
    Mark(Box<CharProps>),
    Para(Box<ParaProps>),
    /// The properties of a paragraph the participant split off another, which rejecting the
    /// split takes away, when they aren't a copy of that one's.
    Split(Box<ParaProps>),
    Section(Box<SectionProps>),
    Table(Box<TableProps>),
    Row(Box<RowProps>),
    Cell(Box<CellProps>),
}

/// What rejecting the participant's own paragraph marks does to a paragraph ([`Coverage::fates`]).
#[derive(Clone, Copy, Default)]
struct Fate {
    /// Its properties go: whether they are a copy of those of the paragraph that stays.
    props: Option<bool>,
    /// Its mark goes: the paragraph goes whole, or the next one joins it.
    mark_goes: bool,
    /// It ends the paragraph joined from the one at this index on, whose properties and mark's
    /// formatting the joined paragraph keeps, with this one's mark's revisions.
    ends: Option<usize>,
}

struct Coverage<'a> {
    /// The revisions of the participant checked.
    own: BTreeSet<u32>,
    by: BTreeMap<u32, Vec<(Place, Covered)>>,
    /// The document's styles (the style Enter gives the paragraph after a heading).
    styles: &'a StyleSheet,
    /// Where the walk is.
    at: Place,
}

impl Coverage<'_> {
    fn push(&mut self, rev: u32, what: Covered) {
        if self.own.contains(&rev) {
            return;
        }
        let at = self.at;
        let list = self.by.entry(rev).or_default();
        // Characters split by the participant's own insertions, deletions or formatting are
        // one stretch, as they were: the next follows on where the last ends.
        if let (Some((was, Covered::Chars(text, props, objects))), Covered::Chars(t, p, o)) = (list.last_mut(), &what)
            && props == p
            && was.story == at.story
            && was.para == at.para
            && was.off.saturating_add(text.len()) == at.off
        {
            text.push_str(t);
            objects.extend(o.iter().cloned());
            return;
        }
        list.push((at, what));
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

    /// Someone's tracked changes of the paragraph properties `props`, as `what`.
    fn para(&mut self, props: &ParaProps, what: fn(Box<ParaProps>) -> Covered) {
        for rev in [props.fmt_change.as_ref().map(|c| c.rev), props.num_change.as_ref().map(|c| c.rev)].into_iter().flatten() {
            self.push(rev, what(Box::new(props.clone())));
        }
    }

    /// What rejecting the participant's own tracked paragraph marks among `blocks` does to each
    /// paragraph, as Reject does it (last mark first): such a mark goes, and the next paragraph
    /// joins the one it ended, which keeps its properties and its mark's formatting, with the
    /// revisions of the last joined paragraph's mark; or, when nothing but the participant's
    /// insertions is in that one, it goes whole and the next stays as it is. Enter, splitting
    /// a paragraph, copies its properties (someone else's tracked change on them too) to the
    /// paragraph it makes, after the last character with the style's next style and no
    /// numbering, so those of a split-off paragraph cover nothing the original doesn't.
    fn fates(&self, blocks: &[Arc<Block>]) -> BTreeMap<usize, Fate> {
        let para = |i: usize| match blocks.get(i).map(|b| &**b) {
            Some(Block::Para(p)) => Some(p),
            _ => None,
        };
        let mut fates: BTreeMap<usize, Fate> = BTreeMap::new();
        // The paragraph after the one looked at, once the marks after it are rejected, and the
        // last one joined to it.
        let mut next: Option<(usize, usize)> = None;
        for i in (0..blocks.len()).rev() {
            let (Some(p), Some((j, last))) = (para(i), next) else {
                next = para(i).map(|_| (i, i));
                continue;
            };
            let Some(q) = para(j) else { continue };
            if !p.mark.ins.is_some_and(|r| self.own.contains(&r)) {
                next = Some((i, i));
            } else if p.section.is_none() && p.run_ranges().all(|(_, c)| c.ins.is_some_and(|r| self.own.contains(&r))) {
                // This one goes whole.
                fates.insert(i, Fate { props: Some(p.props == q.props), mark_goes: true, ends: None });
            } else {
                // The next joins this one.
                let copy = q.props == p.props || q.props == self.after_end(&p.props);
                fates.entry(j).or_default().props = Some(copy);
                fates.entry(i).or_default().mark_goes = true;
                fates.entry(last).or_default().ends = Some(i);
                next = Some((i, last));
            }
        }
        fates
    }

    /// The properties Enter gives the paragraph it makes after the last character of one with
    /// `props`: the style's next style, without numbering, when it has another.
    fn after_end(&self, props: &ParaProps) -> ParaProps {
        let next = props.style.as_deref().and_then(|st| self.styles.get(st).and_then(|x| x.next.clone()).filter(|n| n != st));
        match next {
            Some(n) => ParaProps { style: Some(n), numbering: None, ..props.clone() },
            None => props.clone(),
        }
    }

    fn blocks(&mut self, blocks: &[Arc<Block>], depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        let fates = self.fates(blocks);
        for (i, b) in blocks.iter().enumerate() {
            match &**b {
                Block::Para(p) => {
                    let objects = p.object_offsets();
                    for (range, props) in p.run_ranges() {
                        let text = p.text.get(range.clone()).unwrap_or_default().to_string();
                        let here: Vec<InlineObject> = objects.iter().filter(|o| range.contains(o)).filter_map(|o| p.object_at(*o).cloned()).collect();
                        self.chars(props, |c| Covered::Chars(text.clone(), Box::new(c), here.clone()));
                        // The participant's own insertions aren't there once taken back: what
                        // comes after them is where it was.
                        if !props.ins.is_some_and(|r| self.own.contains(&r)) {
                            self.at.off = self.at.off.saturating_add(range.len());
                        }
                    }
                    let fate = fates.get(&i).copied().unwrap_or_default();
                    // The paragraph joined from `first` on ends here: its mark and properties
                    // come after the text of all of them, as they will.
                    let first = fate.ends.and_then(|f| match blocks.get(f).map(|b| &**b) {
                        Some(Block::Para(f)) => Some(f),
                        _ => None,
                    });
                    if let Some(f) = first {
                        self.chars(&CharProps { ins: p.mark.ins, del: p.mark.del, ..f.mark.clone() }, |c| Covered::Mark(Box::new(c)));
                        self.para(&f.props, Covered::Para);
                    } else if !fate.mark_goes {
                        self.chars(&p.mark, |c| Covered::Mark(Box::new(c)));
                    }
                    // A split-off paragraph's copy of someone's change covers nothing more; one
                    // that isn't a copy is told apart (rejecting the split takes it away).
                    match (fate.props, fate.mark_goes) {
                        (None, false) => self.para(&p.props, Covered::Para),
                        (Some(false), _) => self.para(&p.props, Covered::Split),
                        // Kept, at the end of the paragraphs joined to it; or a copy.
                        (None, true) | (Some(true), _) => {}
                    }
                    if let Some(section) = p.section.as_deref()
                        && let Some(ch) = &section.fmt_change
                    {
                        self.push(ch.rev, Covered::Section(Box::new(section.clone())));
                    }
                    // The next paragraph starts after a mark that stays, and goes on from where
                    // this one ends when its mark goes (joined to this one).
                    if !fate.mark_goes {
                        self.at.para = self.at.para.saturating_add(1);
                        self.at.off = 0;
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

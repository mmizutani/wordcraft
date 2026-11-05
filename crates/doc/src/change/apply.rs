//! [`Document::apply`]: making a [`ChangeSet`]'s edit in a copy of the document.

use std::collections::HashMap;
use std::sync::Arc;

use super::chars::edit_para;
use super::{ChangeSet, Container, FieldChange, NewBlock, OldBlock, ParaEdit, PartChange, Splice, TableShell, fingerprint};
use crate::{Block, BlockId, Blocks, DocError, Document, Part, Path, Result, StoryRef};

/// Most paragraph edits, splices, table changes, part changes and field changes one change may
/// hold; and most blocks it may take away or put in. A change from a real edit stays far below;
/// the bound keeps a hostile one from making applying it slow.
pub const MAX_CHANGE_ITEMS: usize = 1 << 20;

fn stale(what: String) -> DocError {
    DocError::Stale(what)
}

impl Document {
    /// Make the edit `cs` describes in this copy of the document: the copy then holds what the
    /// copy it was made in held after it, under the same ids, wherever its other blocks are.
    ///
    /// The change must have been made on the content it replaces: the blocks it takes away must
    /// be in this copy, in order, holding what they held; so must the paragraphs it edits in
    /// place, or, for an edit moved past another edit of the same paragraph
    /// ([`ChangeSet::rebase`]), the characters it replaces and the properties it changes; the
    /// parts it removes or changes, and the tables it changes, must be here; the revisions must
    /// be as many as it saw. Otherwise it is an error ([`DocError::Stale`]) and the document is
    /// left as it was. So is a change that would leave a story or a table cell without a
    /// paragraph at its end.
    ///
    /// It does not check that the ids of the blocks it puts in are new to this copy: copies make
    /// random ids, and the room's hub, which sees every change, sends back the rare one that
    /// repeats an id ([`Document::repeated_id`]), and its copy makes the edit again.
    pub fn apply(&mut self, cs: &ChangeSet) -> Result<()> {
        let ChangeSet { edits, splices, tables, parts, fields } = cs;
        let items = edits.len() + splices.len() + tables.len() + parts.len() + fields.len();
        let blocks: usize = splices.iter().map(|s| s.removed.len() + s.inserted.len()).sum();
        if items > MAX_CHANGE_ITEMS || blocks > MAX_CHANGE_ITEMS {
            return Err(DocError::Invalid(format!("a change of {items} items and {blocks} blocks is more than one edit makes")));
        }
        let mut d = self.clone();
        let mut tables_at = TableIndex::default();
        for t in tables {
            d.apply_shell(t, &mut tables_at)?;
        }
        d.apply_edits(edits, &mut tables_at)?;
        d.apply_splices(splices, &mut tables_at)?;
        for p in parts {
            d.apply_part(p)?;
        }
        for f in fields {
            d.apply_field(f)?;
        }
        *self = d;
        Ok(())
    }

    fn apply_shell(&mut self, t: &TableShell, at: &mut TableIndex) -> Result<()> {
        let (story, path) = at.find(self, t.id)?;
        let table = self.table_mut(story, &path)?;
        let same_shape = table.rows.len() == t.rows.len() && table.rows.iter().zip(&t.rows).all(|(a, b)| a.cells.len() == b.cells.len());
        if !same_shape {
            return Err(stale(format!("table {} has other rows or cells", t.id)));
        }
        table.props = t.props.clone();
        table.grid = t.grid.clone();
        table.controls = t.controls.clone();
        for (row, shell) in table.rows.iter_mut().zip(&t.rows) {
            row.props = shell.props.clone();
            row.controls = shell.controls.clone();
            for (cell, shell) in row.cells.iter_mut().zip(&shell.cells) {
                cell.props = shell.props.clone();
                cell.controls = shell.controls.clone();
            }
        }
        Ok(())
    }

    /// The paragraphs edited in place, found by id in their lists.
    fn apply_edits(&mut self, edits: &[ParaEdit], at: &mut TableIndex) -> Result<()> {
        let mut lists: Vec<(Container, Vec<&ParaEdit>)> = Vec::new();
        for e in edits {
            match lists.iter_mut().find(|(c, _)| *c == e.container) {
                Some((_, v)) => v.push(e),
                None => lists.push((e.container, vec![e])),
            }
        }
        for (c, v) in lists {
            let (bl, what) = match c {
                Container::Story(story) => (self.story_mut(story).map_err(|_| stale(format!("no story {story:?}")))?, format!("{story:?}")),
                Container::Cell { table, row, col } => {
                    let (story, mut path) = at.find(self, table)?;
                    path.0.extend([row, col, 0]);
                    let cell = format!("cell {row}, {col} of table {table}");
                    (self.container_mut(story, &path).map_err(|_| stale(format!("no {cell}")))?, cell)
                }
            };
            let index: HashMap<BlockId, usize> = bl.iter().enumerate().map(|(i, b)| (b.id(), i)).collect();
            for e in v {
                let b = index.get(&e.id).and_then(|i| bl.get_mut(*i)).ok_or_else(|| stale(format!("paragraph {} isn't in {what}", e.id)))?;
                if e.was.is_some_and(|was| fingerprint(b) != was) {
                    return Err(stale(format!("paragraph {} of {what} holds other content", e.id)));
                }
                match Arc::make_mut(b) {
                    Block::Para(p) => edit_para(p, e)?,
                    Block::Table(_) => return Err(stale(format!("block {} of {what} is a table", e.id))),
                }
            }
        }
        Ok(())
    }

    /// The splices, list by list: the cells of the most deeply nested tables first, so a list
    /// is found before anything around it changed, then the stories.
    fn apply_splices(&mut self, splices: &[Splice], at: &mut TableIndex) -> Result<()> {
        let mut lists: Vec<(Container, Vec<&Splice>)> = Vec::new();
        for s in splices {
            match lists.iter_mut().find(|(c, _)| *c == s.container) {
                Some((_, v)) => v.push(s),
                None => lists.push((s.container, vec![s])),
            }
        }
        let mut cells = Vec::new();
        let mut stories = Vec::new();
        for (c, v) in lists {
            match c {
                Container::Story(story) => stories.push((story, v)),
                Container::Cell { table, row, col } => {
                    let (story, path) = at.find(self, table)?;
                    cells.push((path.0.len(), table, row, col, story, v));
                }
            }
        }
        cells.sort_by_key(|c| std::cmp::Reverse(c.0));
        for (_, table, row, col, _, v) in cells {
            let (story, mut path) = at.find(self, table)?;
            path.0.extend([row, col, 0]);
            let cell = format!("cell {row}, {col} of table {table}");
            let bl = self.container_mut(story, &path).map_err(|_| stale(format!("no {cell}")))?;
            replace(bl, &v, &cell)?;
            if !matches!(bl.last().map(|b| &**b), Some(Block::Para(_))) {
                return Err(DocError::Invalid(format!("the change leaves {cell} without a paragraph at its end")));
            }
        }
        for (story, v) in stories {
            let bl = self.story_mut(story).map_err(|_| stale(format!("no story {story:?}")))?;
            replace(bl, &v, &format!("{story:?}"))?;
            if !matches!(bl.last().map(|b| &**b), Some(Block::Para(_))) {
                return Err(DocError::Invalid(format!("the change leaves {story:?} without a paragraph at its end")));
            }
        }
        Ok(())
    }

    fn apply_part(&mut self, p: &PartChange) -> Result<()> {
        match p {
            PartChange::Added { id, kind, body, blocks } => {
                if self.parts.contains_key(id) {
                    return Err(stale(format!("part {id} is here already")));
                }
                if !matches!(blocks.last().map(|b| &*b.0), Some(Block::Para(_))) {
                    return Err(DocError::Invalid(format!("part {id} doesn't end with a paragraph")));
                }
                let blocks = blocks.iter().map(|b| b.0.clone()).collect();
                self.parts.insert(*id, Part { kind: *kind, blocks, body: *body });
            }
            PartChange::Removed { id, blocks } => {
                let Some(part) = self.parts.get(id) else { return Err(stale(format!("no part {id} to take away"))) };
                if part.blocks.len() != blocks.len() {
                    return Err(stale(format!("part {id} holds other blocks")));
                }
                // An edit made meanwhile keeps its blocks' ids, so their content is checked too:
                // taking the part away would take that edit with it.
                let what = format!("part {id}");
                for (b, old) in part.blocks.iter().zip(blocks) {
                    check(Some(b), old, &what)?;
                }
                self.parts.remove(id);
            }
            PartChange::Changed { id, kind, body } => {
                let Some(part) = self.parts.get_mut(id) else { return Err(stale(format!("no part {id} to change"))) };
                part.kind = *kind;
                part.body = *body;
            }
        }
        Ok(())
    }

    fn apply_field(&mut self, f: &FieldChange) -> Result<()> {
        match f {
            FieldChange::LastSection(v) => self.last_section = v.clone(),
            FieldChange::Styles(v) => self.styles = v.clone(),
            FieldChange::Numbering(v) => self.numbering = v.clone(),
            FieldChange::Comment { id, comment } => match comment {
                Some(c) => {
                    self.comments.insert(*id, c.clone());
                }
                None => {
                    self.comments.remove(id);
                }
            },
            FieldChange::Revisions { base, keep, append } => {
                if self.revisions.len() != *base || keep > base {
                    return Err(stale(format!("{} revisions here, {base} where the change was made", self.revisions.len())));
                }
                self.revisions.truncate(*keep);
                self.revisions.extend(append.iter().cloned());
            }
            FieldChange::Settings(v) => self.settings = (**v).clone(),
            FieldChange::Core(v) => self.core = v.clone(),
            FieldChange::Sources(v) => self.sources = v.clone(),
            FieldChange::CustomProps(v) => self.custom_props = v.clone(),
            FieldChange::FootnoteSeparators(v) => self.footnote_separators = v.clone(),
            FieldChange::EndnoteSeparators(v) => self.endnote_separators = v.clone(),
            FieldChange::Media { key, bytes } => match bytes {
                Some(b) => {
                    self.media.insert(key.clone(), b.clone());
                }
                None => {
                    self.media.remove(key);
                }
            },
            FieldChange::Passthrough { key, bytes } => match bytes {
                Some(b) => {
                    self.passthrough.insert(key.clone(), b.clone());
                }
                None => {
                    self.passthrough.remove(key);
                }
            },
            FieldChange::Glossary(v) => self.glossary = v.clone(),
        }
        Ok(())
    }
}

/// Make `splices` (all in list `bl`, named `what` in errors) in one pass over the list: each
/// replaces the blocks it took away, found by id, or goes after the block it follows.
fn replace(bl: &mut Blocks, splices: &[&Splice], what: &str) -> Result<()> {
    let at: HashMap<BlockId, usize> = bl.iter().enumerate().map(|(i, b)| (b.id(), i)).collect();
    let mut spans = Vec::with_capacity(splices.len());
    for s in splices {
        let start = match s.removed.first() {
            Some(first) => *at.get(&first.id).ok_or_else(|| stale(format!("block {} isn't in {what}", first.id)))?,
            None => match s.after {
                Some(a) => at.get(&a).ok_or_else(|| stale(format!("block {a} isn't in {what}")))? + 1,
                None => 0,
            },
        };
        for (k, old) in s.removed.iter().enumerate() {
            check(bl.get(start + k), old, what)?;
        }
        spans.push((start, start + s.removed.len(), &s.inserted));
    }
    spans.sort_by_key(|s| s.0);
    if spans.windows(2).any(|w| matches!(w, [a, b] if b.0 < a.1)) {
        return Err(DocError::Invalid(format!("the change replaces blocks of {what} twice")));
    }
    let mut out: Blocks = Vec::with_capacity(bl.len() + spans.iter().map(|s| s.2.len()).sum::<usize>());
    let mut i = 0;
    for (start, end, inserted) in spans {
        out.extend(bl.get(i..start).unwrap_or_default().iter().cloned());
        out.extend(inserted.iter().map(|NewBlock(b)| b.clone()));
        i = end;
    }
    out.extend(bl.get(i..).unwrap_or_default().iter().cloned());
    *bl = out;
    Ok(())
}

/// Whether `b`, in this copy, is the block `old` was where the change was made.
fn check(b: Option<&Arc<Block>>, old: &OldBlock, what: &str) -> Result<()> {
    match b {
        Some(b) if b.id() == old.id && fingerprint(b) == old.hash => Ok(()),
        Some(b) if b.id() == old.id => Err(stale(format!("block {} of {what} holds other content", old.id))),
        _ => Err(stale(format!("block {} isn't where the change was made in {what}", old.id))),
    }
}

/// Where the tables of a document are, by id: found by walking the document when a table is
/// first asked for, and again when it moved.
#[derive(Default)]
struct TableIndex(Option<HashMap<BlockId, (StoryRef, Path)>>);

impl TableIndex {
    fn find(&mut self, d: &Document, id: BlockId) -> Result<(StoryRef, Path)> {
        let holds = |d: &Document, (story, path): &(StoryRef, Path)| d.block(*story, path).is_some_and(|b| b.id() == id && b.as_table().is_some());
        if let Some(hit) = self.0.as_ref().and_then(|m| m.get(&id)).filter(|hit| holds(d, hit)) {
            return Ok(hit.clone());
        }
        let mut m = HashMap::new();
        d.each_block(&mut |story, path, b| {
            if b.as_table().is_some() {
                m.entry(b.id()).or_insert((story, Path(path.to_vec())));
            }
        });
        let hit = m.get(&id).cloned();
        self.0 = Some(m);
        hit.ok_or_else(|| stale(format!("no table {id}")))
    }
}

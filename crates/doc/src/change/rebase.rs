//! [`ChangeSet::rebase`]: a change made at the same time as another, moved to apply after it.
//!
//! Two changes made on the same version **clash** when they change the same thing: the same
//! characters of a paragraph (a place where one puts characters in, inside characters the other
//! replaces, counts), the same blocks, the properties of the same paragraph or table, the same
//! part or document field; or when one changes what the other takes away, or puts blocks in
//! between blocks the other takes away. Otherwise either applies after the other, moved past
//! it: an edit of the same paragraph moves past the characters the other put in or took away
//! before it, and blocks put in next to the other's blocks keep their place beside them. Both
//! orders then give the same document under the same ids.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use super::{ChangeSet, CharSplice, Container, FieldChange, ParaEdit, PartChange, Splice};
use crate::{BlockId, DocError, Result, StoryRef};

/// Which of two changes made at once goes first where both put text or blocks in at the same
/// place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// The change moved goes first.
    Before,
    /// The change it is moved past goes first: what a hub accepts later goes after what it
    /// accepted before.
    After,
}

impl Side {
    /// The side the other change takes.
    pub fn other(self) -> Side {
        match self {
            Side::Before => Side::After,
            Side::After => Side::Before,
        }
    }
}

impl ChangeSet {
    /// This change, made on the same version as `other`, moved to apply after it. An error
    /// ([`DocError::Stale`]) when the two clash (see the [module](self) docs), whichever is
    /// moved past the other: this change must then be made again on the version after `other`.
    ///
    /// For changes `a` and `b` that don't clash, `a` then `b.rebase(&a, side)` gives the same
    /// document, under the same ids, as `b` then `a.rebase(&b, side.other())`.
    pub fn rebase(&self, other: &ChangeSet, side: Side) -> Result<ChangeSet> {
        if let Some(why) = clash(self, other) {
            return Err(DocError::Stale(format!("a change made at the same time clashes with it: {why}")));
        }
        let mut out = self.clone();
        let theirs = by_paragraph(&other.edits);
        for e in &mut out.edits {
            let Some(fs) = theirs.get(&e.id) else { continue };
            // The paragraph holds the other's edit too: what this edit replaces is still checked.
            e.was = None;
            if let Some(c) = &mut e.chars {
                let mut at = c.at;
                for o in fs.iter().filter_map(|f| f.chars.as_ref()) {
                    if goes_before(o, c, side) {
                        at = at.saturating_sub(o.removed).saturating_add(o.inserted.text.len());
                    }
                }
                c.at = at;
            }
        }
        for s in &mut out.splices {
            let was = s.clone();
            for o in other.splices.iter().filter(|o| o.container == was.container) {
                let tie = o.removed.is_empty() && was.removed.is_empty() && o.after == was.after;
                // `o` ends where this splice starts: what `o` put in comes before it.
                let after_o = if tie { side == Side::After } else { end(o) == was.after };
                // This splice ends where `o` starts: what `o` put in comes after it.
                let before_o = if tie { side == Side::Before } else { end(&was) == o.after };
                if after_o {
                    s.after = o.inserted.last().map(|b| b.0.id()).or(o.after);
                }
                if before_o {
                    s.before = o.inserted.first().map(|b| b.0.id()).or(o.before);
                }
            }
        }
        Ok(out)
    }
}

/// The block before where splice `s`'s blocks end: its last block taken away, or (putting
/// blocks in only) the block it puts them after.
fn end(s: &Splice) -> Option<BlockId> {
    s.removed.last().map(|b| b.id).or(s.after)
}

/// Whether `o`'s characters come before `c`'s, both edits of the same paragraph made at once,
/// so `c` moves past them: `o` ends where `c` starts or before; where both put characters in at
/// the same place, the one `side` puts first.
fn goes_before(o: &CharSplice, c: &CharSplice, side: Side) -> bool {
    if o.removed == 0 { o.at < c.at || (o.at == c.at && (c.removed > 0 || side == Side::After)) } else { o.at.saturating_add(o.removed) <= c.at }
}

/// Whether two edits of the same paragraph, made at once, change the same characters: their
/// ranges overlap, or one puts characters in inside the range the other replaces.
fn chars_clash(x: &CharSplice, y: &CharSplice) -> bool {
    let (x_end, y_end) = (x.at.saturating_add(x.removed), y.at.saturating_add(y.removed));
    match (x.removed, y.removed) {
        (0, 0) => false,
        (0, _) => y.at < x.at && x.at < y_end,
        (_, 0) => x.at < y.at && y.at < x_end,
        _ => x.at < y_end && y.at < x_end,
    }
}

/// The paragraph edits of a change by paragraph.
fn by_paragraph(edits: &[ParaEdit]) -> BTreeMap<BlockId, Vec<&ParaEdit>> {
    let mut m: BTreeMap<BlockId, Vec<&ParaEdit>> = BTreeMap::new();
    for e in edits {
        m.entry(e.id).or_default().push(e);
    }
    m
}

/// A document field a change replaces, for finding two changes that both replace it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Field<'a> {
    Named(&'static str),
    Comment(u32),
    Media(&'a str),
    Passthrough(&'a str),
}

impl fmt::Display for Field<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Field::Named(n) => write!(f, "the document's {n}"),
            Field::Comment(id) => write!(f, "comment {id}"),
            Field::Media(key) => write!(f, "picture {key}"),
            Field::Passthrough(key) => write!(f, "kept part {key}"),
        }
    }
}

fn field(f: &FieldChange) -> Field<'_> {
    match f {
        FieldChange::LastSection(_) => Field::Named("last section"),
        FieldChange::Styles(_) => Field::Named("styles"),
        FieldChange::Numbering(_) => Field::Named("lists"),
        FieldChange::Comment { id, .. } => Field::Comment(*id),
        FieldChange::Revisions { .. } => Field::Named("tracked changes"),
        FieldChange::Settings(_) => Field::Named("settings"),
        FieldChange::Core(_) => Field::Named("properties"),
        FieldChange::Sources(_) => Field::Named("sources"),
        FieldChange::CustomProps(_) => Field::Named("custom properties"),
        FieldChange::FootnoteSeparators(_) => Field::Named("footnote separators"),
        FieldChange::EndnoteSeparators(_) => Field::Named("endnote separators"),
        FieldChange::Media { key, .. } => Field::Media(key),
        FieldChange::Passthrough { key, .. } => Field::Passthrough(key),
        FieldChange::Glossary(_) => Field::Named("building blocks"),
    }
}

/// What a change touches.
#[derive(Default)]
struct Touches<'a> {
    /// Blocks it takes away, with the blocks inside them; and the blocks of the parts it takes
    /// away.
    gone: BTreeSet<BlockId>,
    /// Blocks it needs to stay: the paragraphs it edits in place and the tables it edits in
    /// place (their properties, or the blocks in their cells).
    held: BTreeSet<BlockId>,
    /// Tables whose properties it changes.
    tables: BTreeSet<BlockId>,
    /// Parts whose blocks it edits.
    in_parts: BTreeSet<u32>,
    /// Parts it adds, takes away or changes.
    parts: BTreeSet<u32>,
    /// Parts it takes away.
    removed_parts: BTreeSet<u32>,
    /// Document fields it changes.
    fields: BTreeSet<Field<'a>>,
}

impl<'a> Touches<'a> {
    fn of(cs: &'a ChangeSet) -> Touches<'a> {
        let mut t = Touches::default();
        let ChangeSet { edits, splices, tables, parts, fields } = cs;
        for s in splices {
            t.container(s.container);
            for b in &s.removed {
                t.gone.insert(b.id);
                t.gone.extend(b.inside.iter().copied());
            }
        }
        for e in edits {
            t.container(e.container);
            t.held.insert(e.id);
        }
        for tb in tables {
            t.tables.insert(tb.id);
            t.held.insert(tb.id);
        }
        for p in parts {
            match p {
                PartChange::Added { id, .. } | PartChange::Changed { id, .. } => {
                    t.parts.insert(*id);
                }
                PartChange::Removed { id, blocks } => {
                    t.parts.insert(*id);
                    t.removed_parts.insert(*id);
                    for b in blocks {
                        t.gone.insert(b.id);
                        t.gone.extend(b.inside.iter().copied());
                    }
                }
            }
        }
        t.fields.extend(fields.iter().map(field));
        t
    }

    /// An edit in list `c`: its part, or its table, must stay.
    fn container(&mut self, c: Container) {
        match c {
            Container::Story(StoryRef::Body) => {}
            Container::Story(StoryRef::Part(id)) => {
                self.in_parts.insert(id);
            }
            Container::Cell { table, .. } => {
                self.held.insert(table);
            }
        }
    }
}

/// Why changes `a` and `b`, made on the same version, can't both apply, if they can't.
fn clash(a: &ChangeSet, b: &ChangeSet) -> Option<String> {
    let (ta, tb) = (Touches::of(a), Touches::of(b));
    for (x, y) in [(&ta, &tb), (&tb, &ta)] {
        if let Some(id) = x.gone.intersection(&y.held).next() {
            return Some(format!("one takes away block {id}, which the other edits"));
        }
        if let Some(id) = x.removed_parts.intersection(&y.in_parts).next() {
            return Some(format!("one takes away part {id}, which the other edits"));
        }
    }
    if let Some(id) = ta.gone.intersection(&tb.gone).next() {
        return Some(format!("both take away block {id}"));
    }
    if let Some(id) = ta.parts.intersection(&tb.parts).next() {
        return Some(format!("both change part {id}"));
    }
    if let Some(f) = ta.fields.intersection(&tb.fields).next() {
        return Some(format!("both change {f}"));
    }
    if let Some(id) = ta.tables.intersection(&tb.tables).next() {
        return Some(format!("both change the properties of table {id}"));
    }
    for (x, y, ty) in [(a, b, &tb), (b, a, &ta)] {
        if let Some(why) = puts_in_among_taken_away(x, y, ty) {
            return Some(why);
        }
    }
    let theirs = by_paragraph(&b.edits);
    for e in &a.edits {
        for f in theirs.get(&e.id).into_iter().flatten() {
            if e.shell.is_some() && f.shell.is_some() {
                return Some(format!("both change the properties of paragraph {}", e.id));
            }
            if let (Some(x), Some(y)) = (&e.chars, &f.chars)
                && chars_clash(x, y)
            {
                return Some(format!("both change the same characters of paragraph {}", e.id));
            }
        }
    }
    None
}

/// Whether `x` puts blocks in after a block `y` takes away (`ty`: what `y` touches), other than
/// the last of a run `y` takes away from that list: so between blocks `y` takes away.
fn puts_in_among_taken_away(x: &ChangeSet, y: &ChangeSet, ty: &Touches) -> Option<String> {
    for s in x.splices.iter().filter(|s| s.removed.is_empty()) {
        let Some(after) = s.after else { continue };
        if !ty.gone.contains(&after) {
            continue;
        }
        let at_the_end = y.splices.iter().any(|o| o.container == s.container && o.removed.last().is_some_and(|b| b.id == after));
        if !at_the_end {
            return Some(format!("one puts blocks in after block {after}, which the other takes away"));
        }
    }
    None
}

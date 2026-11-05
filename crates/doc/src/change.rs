//! Change capture: what one edit changed in a document, as a [`ChangeSet`] that makes the same
//! edit in another copy of the document ([`Document::apply`]).
//!
//! A change names blocks by their [`BlockId`], never by their place, so it still applies to a
//! copy in which other blocks moved, came or went meanwhile. It holds:
//!
//! - **Paragraphs edited in place** ([`ParaEdit`]): a paragraph that kept its id and its place
//!   but holds other characters, formatting or properties sends only the characters the edit
//!   replaced, with their formatting and objects, and its properties when they changed. So
//!   typing a letter in a 188-page document makes a change holding that letter, and two people
//!   typing in the same paragraph don't stop each other ([`ChangeSet::rebase`]).
//! - **Splices** of block lists. In each story, and in each cell of a table edited in place, the
//!   blocks between what both versions share at the start and at the end (the same allocations,
//!   found by address as [`Document::same_content`] does) were taken away and others put in,
//!   whole and with their ids, unless they are paragraphs edited in place. A splice also holds
//!   the window's text before and after as one character splice, with paragraph marks as U+2029
//!   and tables as U+FFFC, which maps positions through the edit ([`ChangeSet::map`]). Enter,
//!   joining paragraphs, and moving or deleting blocks are splices.
//! - The properties of **tables edited in place**: the same rows and cells, whose blocks change
//!   through splices of their own.
//! - **Parts** (headers, footers, notes, comments' text, text boxes) added, removed or changed.
//! - **Document fields** that changed: comments, pictures and kept parts by key, revisions as
//!   appended, the rest (styles, lists, settings…) whole.
//!
//! Applying a change checks it was made on the content it replaces: the blocks it takes away
//! must be there, in order, holding what they held, and so must the paragraphs it edits in place
//! (compared by a fingerprint of their content). A copy whose content differs gets an error and
//! is left as it was, so a change never lands on text it wasn't made on. A change made at the
//! same time as another is moved past it with [`ChangeSet::rebase`], which says when the two
//! clash.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::block_id::{MAX_ID_DEPTH, inside_ids, set_inside_ids};
use crate::control::ControlWrap;
use crate::props::{CellProps, RowProps, TableProps};
use crate::{
    Block, BlockId, BuildingBlock, Comment, CoreProps, CustomProp, Document, NoteSeparators, Numbering, Part, PartKind, Revision, SectionProps,
    Settings, Source, StoryRef, StyleSheet, Table, TextBody,
};

mod apply;
mod chars;
mod map;
mod rebase;
#[cfg(test)]
mod tests;

pub use chars::{CharSplice, Chars, KeptChars, ParaEdit, ParaShell};
pub use map::{Anchor, Mapped};
pub use rebase::Side;

/// A paragraph mark in a window's text ([`TextSplice`]).
pub const MARK: char = '\u{2029}';
/// A table in a window's text ([`TextSplice`]).
pub const TABLE: char = '\u{FFFC}';

/// What one edit changed (see the [module](self) docs).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChangeSet {
    /// Paragraphs edited in place, in document order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub edits: Vec<ParaEdit>,
    /// Block lists whose blocks the edit replaced, in document order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub splices: Vec<Splice>,
    /// Tables edited in place whose own properties changed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<TableShell>,
    /// Parts added, removed or changed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<PartChange>,
    /// Document fields that changed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldChange>,
}

/// A block list: a story, or a cell of a table, named by the table's id (so the cell is found
/// wherever the table is).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Container {
    Story(StoryRef),
    Cell { table: BlockId, row: u32, col: u32 },
}

/// Blocks an edit replaced in one block list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Splice {
    pub container: Container,
    /// The block before the replaced ones; none at the start of the list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<BlockId>,
    /// The block after them; none at the end of the list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<BlockId>,
    /// The blocks taken away, in order.
    #[serde(default)]
    pub removed: Vec<OldBlock>,
    /// The blocks put in their place, in order.
    #[serde(default)]
    pub inserted: Vec<NewBlock>,
    /// The replaced blocks' text before and after.
    pub text: TextSplice,
}

/// A block an edit took away: which it was, how long it was in the window's text, a
/// fingerprint of what it held, and (a table) the blocks inside it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OldBlock {
    pub id: BlockId,
    /// Bytes in the window's text: a paragraph's text, or [`TABLE`].
    pub len: usize,
    /// [`fingerprint`] of the block.
    pub hash: Fingerprint,
    /// The ids of the blocks in a table's cells, as [`Document::each_block`] walks them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inside: Vec<BlockId>,
}

/// A block an edit put in: whole, with its id and the ids of the blocks inside it.
#[derive(Clone, Debug)]
pub struct NewBlock(pub Arc<Block>);

impl PartialEq for NewBlock {
    /// The same content under the same ids.
    fn eq(&self, other: &Self) -> bool {
        self.0.id() == other.0.id() && inside_ids(&self.0) == inside_ids(&other.0) && *self.0 == *other.0
    }
}

#[derive(Serialize)]
struct NewBlockOut<'a> {
    id: BlockId,
    block: &'a Block,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    inside: Vec<BlockId>,
}

#[derive(Deserialize)]
struct NewBlockIn {
    id: BlockId,
    block: Block,
    #[serde(default)]
    inside: Vec<BlockId>,
}

impl Serialize for NewBlock {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        NewBlockOut { id: self.0.id(), block: &self.0, inside: inside_ids(&self.0) }.serialize(s)
    }
}

impl<'de> Deserialize<'de> for NewBlock {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let NewBlockIn { id, mut block, inside } = NewBlockIn::deserialize(d)?;
        *block.id_mut() = id;
        if !set_inside_ids(&mut block, &inside) {
            return Err(serde::de::Error::custom(format!("block {id}: `inside` doesn't list one id for each block inside it")));
        }
        Ok(NewBlock(Arc::new(block)))
    }
}

/// A window's text before and after an edit: `removed` bytes at byte `at` became `inserted`.
/// Paragraph marks are [`MARK`] and tables [`TABLE`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextSplice {
    pub at: usize,
    pub removed: usize,
    pub inserted: String,
}

/// The properties of a table edited in place: its own, and its rows' and cells'.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableShell {
    pub id: BlockId,
    pub props: TableProps,
    pub grid: Vec<f32>,
    #[serde(default, skip_serializing_if = "ControlWrap::is_empty")]
    pub controls: ControlWrap,
    pub rows: Vec<RowShell>,
}

/// A row of a [`TableShell`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowShell {
    pub props: RowProps,
    #[serde(default, skip_serializing_if = "ControlWrap::is_empty")]
    pub controls: ControlWrap,
    pub cells: Vec<CellShell>,
}

/// A cell of a [`RowShell`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CellShell {
    pub props: CellProps,
    #[serde(default, skip_serializing_if = "ControlWrap::is_empty")]
    pub controls: ControlWrap,
}

impl TableShell {
    fn of(t: &Table) -> TableShell {
        let Table { props, grid, rows, controls, id } = t;
        TableShell {
            id: *id,
            props: props.clone(),
            grid: grid.clone(),
            controls: controls.clone(),
            rows: rows
                .iter()
                .map(|r| RowShell {
                    props: r.props.clone(),
                    controls: r.controls.clone(),
                    cells: r.cells.iter().map(|c| CellShell { props: c.props.clone(), controls: c.controls.clone() }).collect(),
                })
                .collect(),
        }
    }
}

/// A part an edit added, removed or changed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PartChange {
    /// A new part, whole.
    #[serde(rename_all = "camelCase")]
    Added { id: u32, kind: PartKind, body: TextBody, blocks: Vec<NewBlock> },
    /// A part taken away, with the blocks it held (for mapping positions in them).
    #[serde(rename_all = "camelCase")]
    Removed { id: u32, blocks: Vec<OldBlock> },
    /// A part whose kind or text body changed (its blocks change through splices).
    #[serde(rename_all = "camelCase")]
    Changed { id: u32, kind: PartKind, body: TextBody },
}

/// A document field an edit changed: the new value, whole or for one key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FieldChange {
    LastSection(SectionProps),
    Styles(Arc<StyleSheet>),
    Numbering(Arc<Numbering>),
    /// A comment's details, or none for a comment taken away.
    #[serde(rename_all = "camelCase")]
    Comment {
        id: u32,
        comment: Option<Comment>,
    },
    /// The revisions: `base` of them before, the first `keep` kept, then `append`.
    #[serde(rename_all = "camelCase")]
    Revisions {
        base: usize,
        keep: usize,
        append: Vec<Revision>,
    },
    Settings(Box<Settings>),
    Core(CoreProps),
    Sources(Vec<Source>),
    CustomProps(Vec<CustomProp>),
    FootnoteSeparators(NoteSeparators),
    EndnoteSeparators(NoteSeparators),
    /// A picture's bytes (base64), or none for one taken away.
    #[serde(rename_all = "camelCase")]
    Media {
        key: String,
        #[serde(with = "b64")]
        bytes: Option<Arc<Vec<u8>>>,
    },
    /// A kept part's bytes (base64), or none for one taken away.
    #[serde(rename_all = "camelCase")]
    Passthrough {
        key: String,
        #[serde(with = "b64")]
        bytes: Option<Arc<Vec<u8>>>,
    },
    Glossary(Vec<BuildingBlock>),
}

/// Bytes as one base64 string, as the document's own JSON writes pictures.
mod b64 {
    use std::sync::Arc;

    use base64::Engine as _;
    use base64::display::Base64Display;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<Arc<Vec<u8>>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(b) => s.collect_str(&Base64Display::new(b, &STANDARD)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Arc<Vec<u8>>>, D::Error> {
        let s: Option<String> = Option::deserialize(d)?;
        s.map(|s| STANDARD.decode(s.as_bytes()).map(Arc::new).map_err(serde::de::Error::custom)).transpose()
    }
}

/// What a block holds, as 64 bits: FNV-1a over its JSON, which leaves ids out. The same in every
/// copy and every build that writes blocks the same way. Written as 16 hexadecimal digits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Fingerprint(pub u64);

impl Serialize for Fingerprint {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_str(&format_args!("{:016x}", self.0))
    }
}

impl<'de> Deserialize<'de> for Fingerprint {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        if s.is_empty() || s.len() > 16 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(serde::de::Error::custom(format!("`{s}` is not a fingerprint (16 hexadecimal digits)")));
        }
        u64::from_str_radix(&s, 16).map(Fingerprint).map_err(serde::de::Error::custom)
    }
}

/// The [`Fingerprint`] of what `b` holds.
pub fn fingerprint(b: &Block) -> Fingerprint {
    fingerprint_of(b)
}

/// The [`Fingerprint`] of `v`'s JSON.
pub(crate) fn fingerprint_of<T: Serialize + ?Sized>(v: &T) -> Fingerprint {
    /// Writes into the hash as serde_json writes, so no copy of the JSON is made.
    struct Fnv(u64);
    impl std::io::Write for Fnv {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            for b in bytes {
                self.0 = (self.0 ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut h = Fnv(0xcbf2_9ce4_8422_2325);
    match serde_json::to_writer(&mut h, v) {
        Ok(()) => Fingerprint(h.0),
        // Nothing fingerprinted fails to serialize; a value no fingerprint is, so nothing
        // matches it.
        Err(_) => Fingerprint(0),
    }
}

/// How long `b` is in a window's text: a paragraph's text, or [`TABLE`].
pub(crate) fn window_len(b: &Block) -> usize {
    match b {
        Block::Para(p) => p.text.len(),
        Block::Table(_) => TABLE.len_utf8(),
    }
}

/// The text of `blocks` as one window: paragraphs' text, [`TABLE`] for tables, [`MARK`] between.
fn window_text(blocks: &[Arc<Block>]) -> String {
    let mut s = String::with_capacity(blocks.iter().map(|b| window_len(b) + MARK.len_utf8()).sum());
    for (i, b) in blocks.iter().enumerate() {
        if i > 0 {
            s.push(MARK);
        }
        match &**b {
            Block::Para(p) => s.push_str(&p.text),
            Block::Table(_) => s.push(TABLE),
        }
    }
    s
}

/// The one splice that turns `a` into `b`: their common start and end kept, the rest replaced.
pub(crate) fn text_splice(a: &str, b: &str) -> TextSplice {
    let mut at = 0;
    for (x, y) in a.chars().zip(b.chars()) {
        if x != y {
            break;
        }
        at += x.len_utf8();
    }
    let room = a.len().min(b.len()).saturating_sub(at);
    let mut end = 0;
    if let (Some(ta), Some(tb)) = (a.get(at..), b.get(at..)) {
        for (x, y) in ta.chars().rev().zip(tb.chars().rev()) {
            if x != y || end + x.len_utf8() > room {
                break;
            }
            end += x.len_utf8();
        }
    }
    let inserted = b.get(at..b.len().saturating_sub(end)).unwrap_or_default().to_string();
    TextSplice { at, removed: a.len().saturating_sub(at + end), inserted }
}

impl OldBlock {
    fn of(b: &Block) -> OldBlock {
        OldBlock { id: b.id(), len: window_len(b), hash: fingerprint(b), inside: inside_ids(b) }
    }
}

/// Whether `x` (now) is table `y` (before) edited in place: the same id and the same rows with
/// the same number of cells, so its cells can be compared one by one.
fn in_place(x: &Block, y: &Block) -> bool {
    match (x, y) {
        (Block::Table(tx), Block::Table(ty)) => {
            tx.id.is_set()
                && tx.id == ty.id
                && tx.rows.len() == ty.rows.len()
                && tx.rows.iter().zip(&ty.rows).all(|(a, b)| a.cells.len() == b.cells.len())
        }
        _ => false,
    }
}

/// Whether two lists hold the same blocks (the same allocations).
fn same_list(a: &[Arc<Block>], b: &[Arc<Block>]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| Arc::ptr_eq(x, y))
}

impl ChangeSet {
    /// What changed from `before` to `after`, a later version of the same document (with the
    /// ids [`Document::ensure_block_ids`] gives).
    pub fn between(before: &Document, after: &Document) -> ChangeSet {
        let mut cs = ChangeSet::default();
        cs.blocks(&before.body, &after.body, Container::Story(StoryRef::Body), 0);
        cs.parts_between(&before.parts, &after.parts);
        cs.fields_between(before, after);
        cs
    }

    /// Whether a block the change puts in, or one inside it, has no id (a document's blocks
    /// get theirs from [`Document::ensure_block_ids`]).
    pub fn puts_in_unnamed_blocks(&self) -> bool {
        let unnamed = |b: &NewBlock| !b.0.id().is_set() || inside_ids(&b.0).iter().any(|id| !id.is_set());
        self.splices.iter().flat_map(|s| &s.inserted).any(unnamed)
            || self.parts.iter().any(|p| matches!(p, PartChange::Added { blocks, .. } if blocks.iter().any(unnamed)))
    }

    /// Whether the change changes nothing.
    pub fn is_empty(&self) -> bool {
        let ChangeSet { edits, splices, tables, parts, fields } = self;
        edits.is_empty() && splices.is_empty() && tables.is_empty() && parts.is_empty() && fields.is_empty()
    }

    /// The splices for list `was` becoming `now` (container `c`, at nesting `depth`).
    fn blocks(&mut self, was: &[Arc<Block>], now: &[Arc<Block>], c: Container, depth: usize) {
        let start = was.iter().zip(now).take_while(|(x, y)| unchanged(x, y)).count();
        let room = was.len().min(now.len()).saturating_sub(start);
        let end = was.iter().rev().zip(now.iter().rev()).take(room).take_while(|(x, y)| unchanged(x, y)).count();
        let wa = was.get(start..was.len().saturating_sub(end)).unwrap_or_default();
        let wb = now.get(start..now.len().saturating_sub(end)).unwrap_or_default();
        if wa.is_empty() && wb.is_empty() {
            return;
        }
        // The blocks around a run of `now`, from `i` for `n` blocks.
        let around = |i: usize, n: usize| (i.checked_sub(1).and_then(|j| now.get(j)).map(|b| b.id()), now.get(i + n).map(|b| b.id()));
        if wa.len() != wb.len() {
            let (after, before) = around(start, wb.len());
            self.splices.push(splice(c, after, before, wa, wb));
            return;
        }
        // As many blocks before as after: each against the one in its place. A table edited in
        // place is compared cell by cell, a paragraph edited in place character by character;
        // runs of other changed blocks are spliced.
        let mut run: Option<usize> = None;
        for k in 0..=wa.len() {
            let (x, y) = (wb.get(k), wa.get(k));
            let same = matches!((x, y), (Some(x), Some(y)) if unchanged(y, x));
            let table = match (x, y) {
                (Some(x), Some(y)) if depth < MAX_ID_DEPTH && !same && in_place(x, y) => Some((x, y)),
                _ => None,
            };
            let edit = match (x, y) {
                (Some(x), Some(y)) if !same && table.is_none() => chars::para_edit(c, y, x),
                _ => None,
            };
            let changed = x.is_some() && !same && table.is_none() && edit.is_none();
            if let Some(e) = edit {
                self.edits.push(e);
            }
            match (changed, run) {
                (true, None) => run = Some(k),
                (false, Some(r)) => {
                    let (after, before) = around(start + r, k - r);
                    self.splices.push(splice(c, after, before, wa.get(r..k).unwrap_or_default(), wb.get(r..k).unwrap_or_default()));
                    run = None;
                }
                _ => {}
            }
            if let Some((x, y)) = table {
                self.table_in_place(y, x, depth);
            }
        }
    }

    /// A table edited in place: its properties if they changed, and splices for its cells.
    fn table_in_place(&mut self, was: &Block, now: &Block, depth: usize) {
        let (Block::Table(ty), Block::Table(tx)) = (was, now) else { return };
        let shell = TableShell::of(tx);
        if shell != TableShell::of(ty) {
            self.tables.push(shell);
        }
        for (r, (rx, ry)) in tx.rows.iter().zip(&ty.rows).enumerate() {
            for (col, (cx, cy)) in rx.cells.iter().zip(&ry.cells).enumerate() {
                if same_list(&cy.blocks, &cx.blocks) {
                    continue;
                }
                let cell = Container::Cell { table: tx.id, row: u32::try_from(r).unwrap_or(u32::MAX), col: u32::try_from(col).unwrap_or(u32::MAX) };
                self.blocks(&cy.blocks, &cx.blocks, cell, depth + 1);
            }
        }
    }

    /// Parts in both versions (spliced), only in the later one (added) or the earlier (removed).
    fn parts_between(&mut self, before: &BTreeMap<u32, Part>, after: &BTreeMap<u32, Part>) {
        for (id, p) in after {
            match before.get(id) {
                Some(was) => {
                    if was.kind != p.kind || was.body != p.body {
                        self.parts.push(PartChange::Changed { id: *id, kind: p.kind, body: p.body });
                    }
                    self.blocks(&was.blocks, &p.blocks, Container::Story(StoryRef::Part(*id)), 0);
                }
                None => {
                    self.parts.push(PartChange::Added {
                        id: *id,
                        kind: p.kind,
                        body: p.body,
                        blocks: p.blocks.iter().cloned().map(NewBlock).collect(),
                    });
                }
            }
        }
        for (id, was) in before {
            if !after.contains_key(id) {
                self.parts.push(PartChange::Removed { id: *id, blocks: was.blocks.iter().map(|b| OldBlock::of(b)).collect() });
            }
        }
    }

    /// The document fields that changed. Every field is listed, so a new one does not build
    /// until someone decides how a change to it travels.
    fn fields_between(&mut self, before: &Document, after: &Document) {
        let Document {
            body: _,
            parts: _,
            last_section,
            styles,
            numbering,
            comments,
            revisions,
            settings,
            core,
            sources,
            custom_props,
            footnote_separators,
            endnote_separators,
            media,
            passthrough,
            glossary,
            media_index: _,
            kept: _,
        } = after;
        let f = &mut self.fields;
        if *last_section != before.last_section {
            f.push(FieldChange::LastSection(last_section.clone()));
        }
        if !Arc::ptr_eq(styles, &before.styles) && styles != &before.styles {
            f.push(FieldChange::Styles(styles.clone()));
        }
        if !Arc::ptr_eq(numbering, &before.numbering) && numbering != &before.numbering {
            f.push(FieldChange::Numbering(numbering.clone()));
        }
        for (id, c) in comments {
            if before.comments.get(id) != Some(c) {
                f.push(FieldChange::Comment { id: *id, comment: Some(c.clone()) });
            }
        }
        for id in before.comments.keys().filter(|id| !comments.contains_key(id)) {
            f.push(FieldChange::Comment { id: *id, comment: None });
        }
        if *revisions != before.revisions {
            let base = before.revisions.len();
            let keep = if revisions.starts_with(&before.revisions) { base } else { 0 };
            f.push(FieldChange::Revisions { base, keep, append: revisions.get(keep..).unwrap_or_default().to_vec() });
        }
        if *settings != before.settings {
            f.push(FieldChange::Settings(Box::new(settings.clone())));
        }
        if *core != before.core {
            f.push(FieldChange::Core(core.clone()));
        }
        if *sources != before.sources {
            f.push(FieldChange::Sources(sources.clone()));
        }
        if *custom_props != before.custom_props {
            f.push(FieldChange::CustomProps(custom_props.clone()));
        }
        if *footnote_separators != before.footnote_separators {
            f.push(FieldChange::FootnoteSeparators(footnote_separators.clone()));
        }
        if *endnote_separators != before.endnote_separators {
            f.push(FieldChange::EndnoteSeparators(endnote_separators.clone()));
        }
        let bytes = |was: &BTreeMap<String, Arc<Vec<u8>>>, now: &BTreeMap<String, Arc<Vec<u8>>>| {
            let changed =
                now.iter().filter(move |(k, v)| was.get(*k).is_none_or(|w| !Arc::ptr_eq(w, v) && w != *v)).map(|(k, v)| (k.clone(), Some(v.clone())));
            let gone = was.keys().filter(move |k| !now.contains_key(*k)).map(|k| (k.clone(), None));
            changed.chain(gone).collect::<Vec<_>>()
        };
        for (key, bytes) in bytes(&before.media, media) {
            f.push(FieldChange::Media { key, bytes });
        }
        for (key, bytes) in bytes(&before.passthrough, passthrough) {
            f.push(FieldChange::Passthrough { key, bytes });
        }
        if *glossary != before.glossary {
            f.push(FieldChange::Glossary(glossary.clone()));
        }
    }
}

/// Whether block `now` is block `was` as it was: the same block, or a copy of it holding the
/// same under the same ids (a command that rebuilt a paragraph and changed nothing in it).
fn unchanged(was: &Arc<Block>, now: &Arc<Block>) -> bool {
    Arc::ptr_eq(was, now) || (was.id() == now.id() && **was == **now && (was.as_table().is_none() || inside_ids(was) == inside_ids(now)))
}

/// A splice in `c` replacing `was` with `now`, between blocks `after` and `before`.
fn splice(c: Container, after: Option<BlockId>, before: Option<BlockId>, was: &[Arc<Block>], now: &[Arc<Block>]) -> Splice {
    Splice {
        container: c,
        after,
        before,
        removed: was.iter().map(|b| OldBlock::of(b)).collect(),
        inserted: now.iter().cloned().map(NewBlock).collect(),
        text: text_splice(&window_text(was), &window_text(now)),
    }
}

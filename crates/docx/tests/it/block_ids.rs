//! Block ids survive a save and a reopen (#38): paragraphs keep theirs as Word's paragraph ids
//! (`w14:paraId`), with the whole id beside them (`wc:blockId`) when a paragraph id can't hold
//! it, and tables keep theirs in `wc:blockId`. Hand-written fixtures, no files produced by Word.

use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Write};
use std::sync::Arc;

use wordcraft_doc::table::{Cell, Table};
use wordcraft_doc::{Block, BlockId, Comment, Document, Paragraph, PartKind, StoryRef};

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const W14: &str = "http://schemas.microsoft.com/office/word/2010/wordml";

fn para(text: &str) -> Arc<Block> {
    Arc::new(Block::Para(Paragraph::with_text(text, Default::default())))
}

/// Body text, a table with text in its cells, and a comment.
fn sample() -> Document {
    let mut doc = Document::new();
    let mut t = Table::new(2, 2, 200.0);
    for (r, row) in t.rows.iter_mut().enumerate() {
        for (c, cell) in row.cells.iter_mut().enumerate() {
            *cell = Cell::with_text(&format!("cell {r}{c}"));
        }
    }
    doc.body = vec![para("First"), Arc::new(Block::Table(t)), para("Last")];
    let part = doc.add_part(PartKind::Comment, vec![para("A remark")]);
    doc.comments.insert(0, Comment { author: "Ann".into(), part, ..Default::default() });
    doc.ensure_block_ids(None);
    doc
}

/// Each block's text (a table's: `table` and its first cell's) and id.
fn ids_by_text(doc: &Document) -> BTreeMap<String, BlockId> {
    let mut out = BTreeMap::new();
    doc.each_block(&mut |_, _, b| {
        let key = match &**b {
            Block::Para(p) => p.plain_text(),
            Block::Table(t) => format!(
                "table {}",
                t.rows
                    .first()
                    .and_then(|r| r.cells.first())
                    .and_then(|c| c.blocks.first())
                    .and_then(|b| b.as_para())
                    .map(|p| p.plain_text())
                    .unwrap_or_default()
            ),
        };
        if !key.trim().is_empty() {
            out.insert(key, b.id());
        }
    });
    out
}

fn unzip(bytes: &[u8]) -> BTreeMap<String, String> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    (0..z.len())
        .filter_map(|i| {
            let mut f = z.by_index(i).unwrap();
            let mut s = String::new();
            f.read_to_string(&mut s).ok()?;
            Some((f.name().to_string(), s))
        })
        .collect()
}

/// Every value of `attr="…"` in `xml`.
fn values<'a>(xml: &'a str, attr: &str) -> Vec<&'a str> {
    let key = format!(" {attr}=\"");
    xml.match_indices(&key).filter_map(|(at, _)| xml.get(at + key.len()..)?.split('"').next()).collect()
}

fn package(body: &str) -> Vec<u8> {
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="{W}" xmlns:w14="{W14}" xmlns:wc="urn:wordcraft:docx:2026"><w:body>{body}</w:body></w:document>"#
    );
    let rels = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in [("_rels/.rels", rels.as_bytes()), ("word/document.xml", doc.as_bytes())] {
        zw.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
        zw.write_all(bytes).unwrap();
    }
    zw.finish().unwrap().into_inner()
}

#[test]
fn block_ids_survive_a_save_and_a_reopen() {
    let mut doc = sample();
    // One id that a paragraph id holds as it is (as Word's own do), the rest 64 bits.
    let mut ids = doc.block_ids();
    ids[0] = BlockId::from_bits(0x0123_4567);
    doc.set_block_ids(&ids).unwrap();
    let before = ids_by_text(&doc);
    assert_eq!(before.len(), 8, "{before:?}");

    let bytes = wordcraft_docx::write(&doc).unwrap();
    let mut back = wordcraft_docx::read(&bytes).unwrap();
    assert_eq!(ids_by_text(&back), before);
    // Opening it changes none of them: they are set and unique.
    assert_eq!(back.ensure_block_ids(None), 0);
    // And again: a second save writes the same ids.
    let again = wordcraft_docx::read(&wordcraft_docx::write(&back).unwrap()).unwrap();
    assert_eq!(ids_by_text(&again), before);
}

#[test]
fn saved_paragraph_ids_are_ones_word_takes() {
    let doc = sample();
    let files = unzip(&wordcraft_docx::write(&doc).unwrap());
    let mut seen = HashSet::new();
    for (name, xml) in files.iter().filter(|(n, _)| n.starts_with("word/") && n.ends_with(".xml")) {
        for pid in values(xml, "w14:paraId") {
            let v = u32::from_str_radix(pid, 16).unwrap();
            assert!(pid.len() == 8 && v > 0 && v < 0x8000_0000, "{name}: {pid}");
            assert!(seen.insert(pid), "{name}: paragraph id {pid} twice");
        }
        if xml.contains("wc:blockId") {
            assert!(xml.contains(r#"xmlns:wc="urn:wordcraft:docx:2026""#), "{name}");
            let ignorable = values(xml, "mc:Ignorable");
            assert!(ignorable.iter().any(|v| v.split(' ').any(|p| p == "wc")), "{name}: wc must be ignorable for Word: {ignorable:?}");
        }
    }
    // Every paragraph of the body, the cells and the comment.
    assert_eq!(seen.len(), 7, "{seen:?}");
    let body = files.get("word/document.xml").unwrap();
    assert_eq!(values(body, "wc:blockId").len(), 7, "64-bit ids ride beside the paragraph ids, and the table has one: {body}");
    // The comment's extended part names it by its paragraph's id.
    let comments = files.get("word/comments.xml").unwrap();
    let pid = values(comments, "w14:paraId");
    assert_eq!(pid.len(), 1);
    assert!(files.get("word/commentsExtended.xml").unwrap().contains(&format!(r#"w15:paraId="{}""#, pid[0])));
}

#[test]
fn word_paragraph_ids_become_block_ids_and_go_back_as_they_were() {
    let body = r#"<w:p w14:paraId="1A2B3C4D" w14:textId="77777777"><w:r><w:t>One</w:t></w:r></w:p><w:p w14:paraId="00C0FFEE"><w:r><w:t>Two</w:t></w:r></w:p>"#;
    let doc = wordcraft_docx::read(&package(body)).unwrap();
    let ids = ids_by_text(&doc);
    assert_eq!(ids.get("One"), Some(&BlockId::from_bits(0x1A2B_3C4D)));
    assert_eq!(ids.get("Two"), Some(&BlockId::from_bits(0x00C0_FFEE)));
    let files = unzip(&wordcraft_docx::write(&doc).unwrap());
    let out = files.get("word/document.xml").unwrap();
    assert_eq!(values(out, "w14:paraId"), ["1A2B3C4D", "00C0FFEE"]);
    assert!(!out.contains("wc:blockId="), "Word's ids fit as they are: {out}");
}

#[test]
fn the_whole_id_wins_over_the_paragraph_id() {
    // As WordCraft writes a 64-bit id; the paragraph id alone (Word dropped `wc:blockId`) is
    // what the paragraph is named by after Word saved it.
    let body = r#"<w:p w14:paraId="12345678" wc:blockId="abcdef0192345678"><w:r><w:t>Kept</w:t></w:r></w:p><w:tbl wc:blockId="00000000000000aa"><w:tr><w:tc><w:p><w:r><w:t>In</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/>"#;
    let doc = wordcraft_docx::read(&package(body)).unwrap();
    let ids = ids_by_text(&doc);
    assert_eq!(ids.get("Kept"), Some(&BlockId::from_bits(0xABCD_EF01_9234_5678)));
    assert_eq!(ids.get("table In"), Some(&BlockId::from_bits(0xAA)));
    assert_eq!(ids.get("In"), Some(&BlockId::UNSET), "no ids saved: the session gives it one");
}

#[test]
fn hostile_or_repeated_ids_are_left_to_the_session() {
    let body = [
        r#"<w:p w14:paraId="zz"><w:r><w:t>a</w:t></w:r></w:p>"#,
        r#"<w:p w14:paraId="123456789"><w:r><w:t>b</w:t></w:r></w:p>"#,
        r#"<w:p w14:paraId="00000000" wc:blockId="0"><w:r><w:t>c</w:t></w:r></w:p>"#,
        r#"<w:p w14:paraId="+1" wc:blockId="12345678901234567"><w:r><w:t>d</w:t></w:r></w:p>"#,
        r#"<w:p w14:paraId="0000ABCD" wc:blockId="-1"><w:r><w:t>e</w:t></w:r></w:p>"#,
        r#"<w:p w14:paraId="0000ABCD"><w:r><w:t>f</w:t></w:r></w:p>"#,
        r#"<w:p w14:paraId="FFFFFFFF"><w:r><w:t>g</w:t></w:r></w:p>"#,
    ]
    .concat();
    let mut doc = wordcraft_docx::read(&package(&body)).unwrap();
    let ids = ids_by_text(&doc);
    for unset in ["a", "b", "c", "d"] {
        assert_eq!(ids.get(unset), Some(&BlockId::UNSET), "{unset}");
    }
    assert_eq!(ids.get("e"), Some(&BlockId::from_bits(0xABCD)), "a bad whole id falls back to the paragraph id");
    assert_eq!(ids.get("f"), Some(&BlockId::from_bits(0xABCD)), "read as the file says");
    assert_eq!(ids.get("g"), Some(&BlockId::from_bits(0xFFFF_FFFF)));
    // Opening tells them apart: the first of two sharing an id keeps it.
    doc.ensure_block_ids(None);
    let ids = ids_by_text(&doc);
    assert_eq!(ids.get("e"), Some(&BlockId::from_bits(0xABCD)));
    let all: HashSet<BlockId> = ids.values().copied().collect();
    assert_eq!(all.len(), ids.len());
    assert!(all.iter().all(|id| id.is_set()));
    // And a save gives every paragraph an id Word takes, once.
    let files = unzip(&wordcraft_docx::write(&doc).unwrap());
    let pids = values(files.get("word/document.xml").unwrap(), "w14:paraId");
    assert_eq!(pids.iter().collect::<HashSet<_>>().len(), pids.len());
    assert!(pids.iter().all(|p| u32::from_str_radix(p, 16).is_ok_and(|v| v > 0 && v < 0x8000_0000)), "{pids:?}");
}

#[test]
fn a_comment_paragraph_keeps_its_id_in_its_story() {
    // Not every story is the body: the comment's paragraph comes back with its id, in its part.
    let doc = sample();
    let comment_id = doc.parts.values().find_map(|p| p.blocks.first()).map(|b| b.id()).unwrap();
    let back = wordcraft_docx::read(&wordcraft_docx::write(&doc).unwrap()).unwrap();
    let part = back.comments.values().next().unwrap().part;
    let mut found = None;
    back.each_block(&mut |story, _, b| {
        if story == StoryRef::Part(part) {
            found = Some(b.id());
        }
    });
    assert_eq!(found, Some(comment_id));
}

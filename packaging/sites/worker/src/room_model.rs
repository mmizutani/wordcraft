//! The hosted room's WordCraft document model and DOCX checkpoints.

use wordcraft_doc::{ChangeSet, Document, change::Side};
use wordcraft_docx::{DocxError, VBA_PROJECT_PART};

const MAX_DOCX_BYTES: usize = 16 * 1024 * 1024;
const MAX_EXPANDED_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RoomModelError {
    Invalid,
    Unsupported,
    TooLarge,
    Conflict,
}

pub(crate) fn checkpoint(bytes: &[u8]) -> Result<Vec<u8>, RoomModelError> {
    let mut doc = read_docx(bytes)?;
    doc.ensure_block_ids(None);
    let bytes = encode(&doc)?;
    let reopened = replay(&bytes, &[])?;
    if reopened.block_ids() != doc.block_ids() || reopened.revisions != doc.revisions {
        return Err(RoomModelError::Unsupported);
    }
    Ok(bytes)
}

pub(crate) fn replay(checkpoint: &[u8], history: &[ChangeSet]) -> Result<Document, RoomModelError> {
    let mut doc = read_docx(checkpoint)?;
    let ids = doc.block_ids();
    if ids.iter().any(|id| !id.is_set()) || doc.repeated_id(&ids).is_some() {
        return Err(RoomModelError::Invalid);
    }
    for change in history {
        doc = apply(&doc, change)?;
    }
    Ok(doc)
}

pub(crate) fn propose(doc: &mut Document, missed: &[ChangeSet], mut change: ChangeSet) -> Result<ChangeSet, RoomModelError> {
    for accepted in missed {
        accepted.rebase(&change, Side::Before).map_err(|_| RoomModelError::Conflict)?;
        change = change.rebase(accepted, Side::After).map_err(|_| RoomModelError::Conflict)?;
    }
    let candidate = apply(doc, &change)?;
    // Derive the journal entry, including position maps, from the state that actually changed.
    let canonical = ChangeSet::between(doc, &candidate);
    if canonical.is_empty() {
        return Err(RoomModelError::Invalid);
    }
    *doc = candidate;
    Ok(canonical)
}

pub(crate) fn write_checkpoint(doc: &Document) -> Result<Vec<u8>, RoomModelError> {
    let bytes = encode(doc)?;
    let reopened = replay(&bytes, &[])?;
    // The next journal entry checks fingerprints from this exact model.
    if reopened != *doc || reopened.block_ids() != doc.block_ids() {
        return Err(RoomModelError::Unsupported);
    }
    Ok(bytes)
}

fn read_docx(bytes: &[u8]) -> Result<Document, RoomModelError> {
    check_size(bytes)?;
    let doc = wordcraft_docx::read_bounded(bytes, MAX_EXPANDED_BYTES).map_err(docx_error)?;
    supported(&doc)?;
    Ok(doc)
}

fn encode(doc: &Document) -> Result<Vec<u8>, RoomModelError> {
    supported(doc)?;
    let bytes = wordcraft_docx::write(doc).map_err(docx_error)?;
    check_size(&bytes)?;
    Ok(bytes)
}

fn docx_error(error: DocxError) -> RoomModelError {
    match error {
        DocxError::Limit(_) => RoomModelError::TooLarge,
        DocxError::PasswordRequired => RoomModelError::Unsupported,
        _ => RoomModelError::Invalid,
    }
}

fn check_size(bytes: &[u8]) -> Result<(), RoomModelError> {
    if bytes.len() > MAX_DOCX_BYTES {
        return Err(RoomModelError::TooLarge);
    }
    Ok(())
}

fn supported(doc: &Document) -> Result<(), RoomModelError> {
    if doc.settings.protection.is_some()
        || doc.settings.protect_formatting
        || !doc.settings.protect_hash.is_empty()
        || doc.passthrough.contains_key(VBA_PROJECT_PART)
        || doc.missing_pictures.total > 0
    {
        return Err(RoomModelError::Unsupported);
    }
    Ok(())
}

fn apply(doc: &Document, change: &ChangeSet) -> Result<Document, RoomModelError> {
    if change.is_empty() || change.puts_in_unnamed_blocks() {
        return Err(RoomModelError::Invalid);
    }
    let mut candidate = doc.clone();
    candidate.apply(change).map_err(|_| RoomModelError::Invalid)?;
    if candidate.repeated_id(&change.ids_put_in()).is_some() {
        return Err(RoomModelError::Invalid);
    }
    supported(&candidate)?;
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read, Write};
    use std::sync::Arc;

    use wordcraft_doc::{Block, ChangeSet, Document, Paragraph, Pos, StoryRef};

    use super::{MAX_DOCX_BYTES, MAX_EXPANDED_BYTES, RoomModelError, checkpoint, propose, replay, write_checkpoint};

    const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
    const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    const RETAINED: &str = r#"<w:docVars><w:docVar w:name="RoomFixture" w:val="original"/></w:docVars>"#;

    fn zip_parts(parts: &[(&str, &[u8])]) -> Vec<u8> {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in parts {
            let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            archive.start_file(*name, opts).unwrap();
            archive.write_all(content).unwrap();
        }
        archive.finish().unwrap().into_inner()
    }

    fn docx(body: &str, settings: &str) -> Vec<u8> {
        let doc = format!(r#"<w:document xmlns:w="{W}"><w:body>{body}</w:body></w:document>"#);
        let settings = format!(r#"<w:settings xmlns:w="{W}">{settings}</w:settings>"#);
        let rels = format!(
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{R}/settings" Target="settings.xml"/></Relationships>"#
        );
        zip_parts(&[
            ("word/document.xml", doc.as_bytes()),
            ("word/settings.xml", settings.as_bytes()),
            ("word/_rels/document.xml.rels", rels.as_bytes()),
        ])
    }

    fn settings_xml(bytes: &[u8]) -> String {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut settings = String::new();
        archive.by_name("word/settings.xml").unwrap().read_to_string(&mut settings).unwrap();
        settings
    }

    fn text_edit(doc: &Document, at: usize, text: &str) -> ChangeSet {
        let mut after = doc.clone();
        after.insert_text(&Pos::body(0, at), text, &Default::default()).unwrap();
        ChangeSet::between(doc, &after)
    }

    fn base() -> (Vec<u8>, Document) {
        let bytes = checkpoint(&docx("<w:p><w:r><w:t>abcd</w:t></w:r></w:p>", "")).unwrap();
        let doc = replay(&bytes, &[]).unwrap();
        (bytes, doc)
    }

    #[test]
    fn checkpoint_replay_and_compaction_keep_imported_ids_revisions_and_xml() {
        let imported = docx(
            r#"<w:p><w:r><w:t>abcd</w:t></w:r></w:p><w:p><w:ins w:id="4" w:author="Test"><w:r><w:t>Tracked</w:t></w:r></w:ins></w:p>"#,
            RETAINED,
        );
        assert!(wordcraft_docx::read(&imported).unwrap().block_ids().iter().all(|id| !id.is_set()));
        let bytes = checkpoint(&imported).unwrap();
        let base = replay(&bytes, &[]).unwrap();
        let ids = base.block_ids();
        assert!(ids.iter().all(|id| id.is_set()));
        assert_eq!(base.revisions.len(), 1);

        let change = text_edit(&base, 2, "é");
        let edited = replay(&bytes, &[change]).unwrap();
        assert_eq!(edited.plain_text(StoryRef::Body), "abécd\nTracked");
        let compacted = write_checkpoint(&edited).unwrap();
        let reopened = replay(&compacted, &[]).unwrap();
        assert_eq!(reopened.block_ids(), ids);
        assert_eq!(reopened.revisions, edited.revisions);
        assert_eq!(reopened.plain_text(StoryRef::Body), edited.plain_text(StoryRef::Body));
        assert!(settings_xml(&compacted).contains(RETAINED));

        let next = text_edit(&edited, 4, "!");
        assert_eq!(replay(&compacted, &[next]).unwrap().plain_text(StoryRef::Body), "abé!cd\nTracked");
    }

    #[test]
    fn concurrent_inserts_rebase_and_replay_in_commit_order() {
        let (bytes, base) = base();
        let mut head = base.clone();
        let one = propose(&mut head, &[], text_edit(&base, 1, "é")).unwrap();
        let two = propose(&mut head, std::slice::from_ref(&one), text_edit(&base, 3, "Y")).unwrap();

        assert_eq!(head.plain_text(StoryRef::Body), "aébcYd");
        let recovered = replay(&bytes, &[one, two]).unwrap();
        assert_eq!(head, recovered);
    }

    #[test]
    fn overlapping_and_invalid_proposals_leave_the_head_intact() {
        let (_, base) = base();
        let mut head = base.clone();
        let mut replacement = base.clone();
        replacement.body[0] = Arc::new(Block::Para(Paragraph::with_text("aXcd", Default::default())));
        replacement.set_block_ids(&base.block_ids()).unwrap();
        let one = propose(&mut head, &[], ChangeSet::between(&base, &replacement)).unwrap();
        let before = head.clone();
        replacement.body[0] = Arc::new(Block::Para(Paragraph::with_text("aYcd", Default::default())));
        replacement.set_block_ids(&base.block_ids()).unwrap();
        assert_eq!(propose(&mut head, &[one], ChangeSet::between(&base, &replacement)), Err(RoomModelError::Conflict));

        let mut duplicate = head.clone();
        duplicate.body.push(duplicate.body[0].clone());
        assert_eq!(propose(&mut head, &[], ChangeSet::between(&before, &duplicate)), Err(RoomModelError::Invalid));

        let mut unnamed = head.clone();
        unnamed.body.push(Arc::new(Block::Para(Paragraph::new())));
        assert_eq!(propose(&mut head, &[], ChangeSet::between(&before, &unnamed)), Err(RoomModelError::Invalid));

        assert_eq!(propose(&mut head, &[], ChangeSet::default()), Err(RoomModelError::Invalid));
        assert_eq!(head, before);
    }

    #[test]
    fn accepted_changes_rebuild_mapping_from_the_applied_edit() {
        let (_, mut head) = base();
        let mut after = head.clone();
        after.body.insert(0, Arc::new(Block::Para(Paragraph::with_text("First", Default::default()))));
        after.ensure_block_ids(Some(&head));
        let expected = ChangeSet::between(&head, &after);
        let mut submitted = expected.clone();
        submitted.splices[0].text.at = usize::MAX;
        let accepted = propose(&mut head, &[], submitted).unwrap();
        assert_eq!(accepted, expected);
        assert_eq!(head.plain_text(StoryRef::Body), "First\nabcd");
    }

    #[test]
    fn compaction_refuses_docx_that_changes_the_model() {
        let (_, mut head) = base();
        head.settings.default_tab = 0.0;
        assert_eq!(write_checkpoint(&head).err(), Some(RoomModelError::Unsupported));
    }

    #[test]
    fn malformed_protected_and_oversized_documents_are_refused() {
        assert_eq!(checkpoint(b"invalid"), Err(RoomModelError::Invalid));
        let body = "<w:p><w:r><w:t>abcd</w:t></w:r></w:p>";
        let protected = docx(body, r#"<w:documentProtection w:edit="readOnly" w:enforcement="1"/>"#);
        assert_eq!(checkpoint(&protected).err(), Some(RoomModelError::Unsupported));

        let original = docx(body, "");
        let encrypted = wordcraft_docx::encrypt_with_spin_count(&original, "synthetic-password", 1).unwrap();
        assert_eq!(checkpoint(&encrypted).err(), Some(RoomModelError::Unsupported));

        assert_eq!(checkpoint(&vec![0; MAX_DOCX_BYTES + 1]).err(), Some(RoomModelError::TooLarge));
        let compressed = zip_parts(&[("data", &vec![0; MAX_EXPANDED_BYTES as usize + 1])]);
        assert!(compressed.len() < MAX_DOCX_BYTES);
        assert_eq!(checkpoint(&compressed).err(), Some(RoomModelError::TooLarge));
    }
}

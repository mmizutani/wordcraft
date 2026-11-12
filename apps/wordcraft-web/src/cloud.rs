mod room;
#[cfg(target_arch = "wasm32")]
mod transport;
#[cfg(target_arch = "wasm32")]
pub(super) use transport::service;

use serde::Deserialize;
use wordcraft_ui_egui::cloud::{CloudEntry, CloudFailure, CloudFailureKind};

const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_REVISION: u64 = (1 << 53) - 1;
const PAGE_SIZE: usize = 50;

#[derive(Deserialize)]
struct Entry {
    id: String,
    name: String,
    revision: u64,
    byte_size: u64,
    saved_at: u64,
}

impl Entry {
    fn validated(self) -> Option<CloudEntry> {
        if !valid_id(&self.id)
            || !valid_name(&self.name)
            || !(1..=MAX_REVISION).contains(&self.revision)
            || !(1..=MAX_BYTES as u64).contains(&self.byte_size)
            || self.saved_at > MAX_REVISION
        {
            return None;
        }
        Some(CloudEntry { id: self.id, name: self.name, revision: self.revision, byte_size: self.byte_size, saved_at: self.saved_at })
    }
}

#[derive(Deserialize)]
struct Page {
    documents: Vec<Entry>,
    next_cursor: Option<String>,
}

impl Page {
    fn validated(self) -> Option<(Vec<CloudEntry>, Option<String>)> {
        if self.documents.len() > PAGE_SIZE || self.next_cursor.as_deref().is_some_and(|cursor| !valid_cursor(cursor)) {
            return None;
        }
        Some((self.documents.into_iter().map(Entry::validated).collect::<Option<_>>()?, self.next_cursor))
    }
}

fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn valid_cursor(cursor: &str) -> bool {
    cursor.split_once('.').is_some_and(|(time, id)| {
        valid_id(id)
            && time.len() <= 16
            && time.bytes().all(|byte| byte.is_ascii_digit())
            && time.parse::<u64>().is_ok_and(|time| time <= MAX_REVISION)
    })
}

fn valid_name(name: &str) -> bool {
    name.len() <= 255
        && name.trim() == name
        && !name.chars().any(|ch| ch.is_control() || matches!(ch, '/' | '\\'))
        && name.rsplit_once('.').is_some_and(|(stem, extension)| !stem.trim().is_empty() && extension.eq_ignore_ascii_case("docx"))
}

fn etag(revision: u64) -> Option<String> {
    (1..=MAX_REVISION).contains(&revision).then(|| format!("\"{revision}\""))
}

fn revision(tag: &str) -> Option<u64> {
    let digits = tag.strip_prefix('"')?.strip_suffix('"')?;
    if digits.starts_with('0') || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u64>().ok().filter(|value| *value <= MAX_REVISION)
}

fn failure(kind: CloudFailureKind, message: &str) -> CloudFailure {
    CloudFailure { kind, message: message.to_owned() }
}

fn status_failure(status: u16, code: Option<&str>) -> CloudFailure {
    use CloudFailureKind::*;
    match (status, code) {
        (401 | 403, Some("invalid_origin")) => failure(Invalid, "The Site rejected this request origin."),
        (401 | 403, _) => failure(Authentication, "Sign in to the Site, then try again."),
        (404, _) => failure(Missing, "The cloud document is no longer available."),
        (412, _) => failure(Stale, "A newer cloud revision is available."),
        (409, Some("room_required")) => failure(RoomRequired, "Open the shared room to edit this document."),
        (409, Some("operation_abandoned")) => failure(Abandoned, "The cloud save expired before it could commit."),
        (409, Some("operation_mismatch")) => failure(Invalid, "The Site rejected this cloud operation ID."),
        (202 | 408 | 429 | 500..=599 | 409, _) => failure(Retryable, "The Site could not confirm the cloud operation. Try again."),
        (413, _) => failure(Invalid, "Cloud documents can be at most 16 MiB."),
        _ => failure(Invalid, "The Site rejected the cloud request."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_single_canonical_strong_revision_validators() {
        for value in [1, 12, MAX_REVISION] {
            assert_eq!(etag(value).as_deref().and_then(revision), Some(value));
        }
        for invalid in ["*", "W/\"1\"", "\"1\", \"2\"", "\"0\"", "\"01\"", "\"+1\"", "\"9007199254740992\""] {
            assert_eq!(revision(invalid), None, "{invalid}");
        }
        assert_eq!(etag(0), None);
        assert_eq!(etag(MAX_REVISION + 1), None);
    }

    #[test]
    fn rejects_unbounded_metadata_and_invalid_path_components() {
        let id = "123456789012345678901234567890ab";
        let entry = || Entry { id: id.into(), name: "Résumé.DOCX".into(), revision: 1, byte_size: MAX_BYTES as u64, saved_at: 2 };
        assert!(Page { documents: vec![entry()], next_cursor: Some(format!("2.{id}")) }.validated().is_some());
        assert!(Page { documents: (0..=PAGE_SIZE).map(|_| entry()).collect(), next_cursor: None }.validated().is_none());
        assert!(Page { documents: vec![entry()], next_cursor: Some("1.//elsewhere".into()) }.validated().is_none());
        assert!(Entry { byte_size: MAX_BYTES as u64 + 1, ..entry() }.validated().is_none());
        assert!(Entry { name: "../notes.docx".into(), ..entry() }.validated().is_none());
        assert!(Entry { id: "../notes.docx".into(), ..entry() }.validated().is_none());
    }

    #[test]
    fn requires_a_definitive_abandonment_code() {
        assert!(matches!(status_failure(409, None).kind, CloudFailureKind::Retryable));
        assert!(matches!(status_failure(409, Some("operation_abandoned")).kind, CloudFailureKind::Abandoned));
        assert!(matches!(status_failure(409, Some("operation_mismatch")).kind, CloudFailureKind::Invalid));
        assert!(matches!(status_failure(503, Some("operation_abandoned")).kind, CloudFailureKind::Retryable));
    }
}

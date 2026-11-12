use super::store::{Store, number};
use serde::Deserialize;
use worker::{Result, wasm_bindgen::JsValue};

pub(super) const MAX_CHANGE_BYTES: usize = 256 * 1024;
pub(super) const MAX_TAIL_BYTES: u64 = 32 * 1024 * 1024;
pub(super) const MAX_TAIL_CHANGES: u64 = 2_048;

const MAX_PAGE_CHANGES: u32 = 32;
const MAX_PAGE_BYTES: u32 = 512 * 1024;
const CANDIDATE_LEASE_SECONDS: u32 = 120;

#[derive(Deserialize)]
pub(super) struct Room {
    pub document_id: String,
    pub file_revision: u64,
    pub published_room_revision: u64,
    pub checkpoint_revision: u64,
    pub checkpoint_key: String,
    pub checkpoint_size: u64,
    pub checkpoint_sha256: String,
    pub head: u64,
    pub tail_bytes: u64,
}

#[derive(Deserialize)]
pub(super) struct RoomChange {
    pub revision: u64,
    pub operation_id: String,
    pub change_json: String,
}

#[derive(Deserialize)]
pub(super) struct RoomReceipt {
    pub revision: u64,
    pub actor: String,
    pub fingerprint: String,
}

pub(super) struct RoomAppend<'a> {
    pub actor: &'a str,
    pub grant_version: u64,
    pub operation_id: &'a str,
    pub fingerprint: &'a str,
    pub change_json: &'a str,
}

#[derive(Deserialize)]
pub(super) struct RoomCleanup {
    pub object_key: String,
    pub active: u32,
    pub settled: u32,
}

impl Store {
    pub async fn room(&self, id: &str) -> Result<Option<Room>> {
        self.statement(
            "SELECT r.* FROM cloud_rooms r
             JOIN cloud_documents d ON d.owner = r.owner AND d.id = r.document_id
             WHERE r.owner = ?1 AND r.document_id = ?2 AND d.deleted = 0",
            &[id.into()],
        )?
        .first(None)
        .await
    }

    pub async fn reserve_checkpoint(&self, id: &str, file_revision: u64) -> Result<Option<String>> {
        self.statement(
            "INSERT INTO cloud_room_candidates (owner, document_id, file_revision, object_key, lease_until)
             SELECT d.owner, d.id, d.revision, 'wordcraft-room/' || lower(hex(randomblob(16))), unixepoch() + ?4
             FROM cloud_documents d WHERE d.owner = ?1 AND d.id = ?2 AND d.revision = ?3 AND d.deleted = 0
               AND NOT EXISTS (SELECT 1 FROM cloud_rooms r WHERE r.owner = ?1 AND r.document_id = ?2)
               AND NOT EXISTS (
                 SELECT 1 FROM cloud_room_candidates c WHERE c.owner = ?1 AND c.document_id = ?2
                   AND c.file_revision = ?3 AND c.lease_until > unixepoch()
               )
             RETURNING object_key",
            &[id.into(), number(file_revision), CANDIDATE_LEASE_SECONDS.into()],
        )?
        .first(Some("object_key"))
        .await
    }

    pub async fn publish_checkpoint(&self, id: &str, key: &str, bytes: u64, digest: &str) -> Result<Option<Room>> {
        let args = &[id.into(), key.into()];
        let statements = vec![
            self.statement(
                "INSERT INTO cloud_rooms (owner, document_id, file_revision, checkpoint_key, checkpoint_size, checkpoint_sha256)
                 SELECT c.owner, c.document_id, c.file_revision, c.object_key, ?4, ?5
                 FROM cloud_room_candidates c
                 JOIN cloud_documents d ON d.owner = c.owner AND d.id = c.document_id
                 WHERE c.owner = ?1 AND c.document_id = ?2 AND c.object_key = ?3 AND c.lease_until > unixepoch()
                   AND d.deleted = 0 AND d.revision = c.file_revision
                 ON CONFLICT (owner, document_id) DO NOTHING",
                &[id.into(), key.into(), number(bytes), digest.into()],
            )?,
            self.statement(
                "DELETE FROM cloud_room_candidates
                 WHERE owner = ?1 AND document_id = ?2 AND object_key = ?3
                   AND EXISTS (SELECT 1 FROM cloud_rooms WHERE owner = ?1 AND document_id = ?2 AND checkpoint_key = ?3)",
                args,
            )?,
            self.statement(
                "UPDATE cloud_room_candidates SET settled = 1, fenced = 0, lease_until = 0
                 WHERE owner = ?1 AND document_id = ?2 AND object_key = ?3",
                args,
            )?,
        ];
        let write = self.db.batch(statements).await;
        let room = self.room(id).await?;
        if room.as_ref().is_none_or(|room| room.checkpoint_key != key) {
            write?;
        }
        Ok(room)
    }

    pub async fn settle_checkpoint(&self, key: &str) -> Result<()> {
        self.statement(
            "UPDATE cloud_room_candidates SET settled = 1, fenced = 0, lease_until = 0
             WHERE owner = ?1 AND object_key = ?2",
            &[key.into()],
        )?
        .run()
        .await?;
        Ok(())
    }

    pub async fn room_cleanup(&self, id: Option<&str>) -> Result<Vec<RoomCleanup>> {
        self.statement(
            "SELECT r.checkpoint_key AS object_key, 1 AS active, 1 AS settled
             FROM cloud_rooms r JOIN cloud_documents d ON d.owner = r.owner AND d.id = r.document_id
             WHERE r.owner = ?1 AND (?2 IS NULL OR r.document_id = ?2) AND d.deleted = 1
             UNION ALL
             SELECT c.object_key, 0 AS active, c.settled
             FROM cloud_room_candidates c JOIN cloud_documents d ON d.owner = c.owner AND d.id = c.document_id
             WHERE c.owner = ?1 AND (?2 IS NULL OR c.document_id = ?2) AND c.fenced = 0
               AND (d.deleted = 1 OR c.lease_until <= unixepoch())
             LIMIT 4",
            &[id.map_or(JsValue::NULL, JsValue::from_str)],
        )?
        .all()
        .await?
        .results()
    }

    pub async fn confirm_room_cleanup(&self, item: &RoomCleanup) -> Result<()> {
        let sql = match (item.active, item.settled) {
            (1, _) => "DELETE FROM cloud_rooms WHERE owner = ?1 AND checkpoint_key = ?2",
            (_, 1) => "DELETE FROM cloud_room_candidates WHERE owner = ?1 AND object_key = ?2",
            // Keep the marker until the uploader has stopped and settled its candidate.
            _ => "UPDATE cloud_room_candidates SET fenced = 1 WHERE owner = ?1 AND object_key = ?2 AND settled = 0",
        };
        self.statement(sql, &[item.object_key.as_str().into()])?.run().await?;
        Ok(())
    }

    pub async fn room_receipt(&self, id: &str, operation: &str) -> Result<Option<RoomReceipt>> {
        self.statement(
            "SELECT c.revision, c.actor, c.fingerprint FROM cloud_room_changes c
             JOIN cloud_documents d ON d.owner = c.owner AND d.id = c.document_id
             WHERE c.owner = ?1 AND c.document_id = ?2 AND c.operation_id = ?3 AND d.deleted = 0",
            &[id.into(), operation.into()],
        )?
        .first(None)
        .await
    }

    pub async fn room_page(&self, id: &str, after: u64, through: u64) -> Result<Vec<RoomChange>> {
        self.statement(
            "WITH page AS (
               SELECT c.revision, c.operation_id, c.change_json,
                      SUM(c.change_bytes + 128) OVER (ORDER BY c.revision) AS page_bytes
               FROM cloud_room_changes c
               JOIN cloud_documents d ON d.owner = c.owner AND d.id = c.document_id
               WHERE c.owner = ?1 AND c.document_id = ?2 AND c.revision > ?3 AND c.revision <= ?4 AND d.deleted = 0
               ORDER BY c.revision LIMIT ?5
             )
             SELECT revision, operation_id, change_json FROM page WHERE page_bytes <= ?6 ORDER BY revision",
            &[id.into(), number(after), number(through), MAX_PAGE_CHANGES.into(), MAX_PAGE_BYTES.into()],
        )?
        .all()
        .await?
        .results()
    }

    pub async fn append_room_change(&self, room: &Room, change: &RoomAppend<'_>) -> Result<Option<RoomReceipt>> {
        let id = room.document_id.as_str();
        let next = room.head + 1;
        let bytes = change.change_json.len() as u64;
        let statements = vec![
            self.statement(
                "INSERT INTO cloud_room_changes (owner, document_id, revision, actor, operation_id, fingerprint, change_json, change_bytes)
                 SELECT r.owner, r.document_id, ?3, ?4, ?5, ?6, ?7, ?8
                 FROM cloud_rooms r JOIN cloud_documents d ON d.owner = r.owner AND d.id = r.document_id
                 WHERE r.owner = ?1 AND r.document_id = ?2 AND r.head = ?3 - 1 AND d.deleted = 0
                   AND r.head - r.checkpoint_revision < ?9 AND r.tail_bytes + ?8 <= ?10
                   AND (?4 = r.owner OR EXISTS (
                     SELECT 1 FROM cloud_room_grants g
                     WHERE (g.owner, g.document_id, g.actor) = (r.owner, r.document_id, ?4)
                       AND g.role = 'editor' AND g.grant_version = ?11
                   ))
                 ON CONFLICT (owner, document_id, operation_id) DO NOTHING",
                &[
                    id.into(),
                    number(next),
                    change.actor.into(),
                    change.operation_id.into(),
                    change.fingerprint.into(),
                    change.change_json.into(),
                    number(bytes),
                    number(MAX_TAIL_CHANGES),
                    number(MAX_TAIL_BYTES),
                    number(change.grant_version),
                ],
            )?,
            self.statement(
                "UPDATE cloud_rooms SET head = ?3, tail_bytes = tail_bytes + ?4
                 WHERE owner = ?1 AND document_id = ?2 AND head = ?3 - 1 AND EXISTS (
                   SELECT 1 FROM cloud_room_changes WHERE owner = ?1 AND document_id = ?2 AND revision = ?3 AND operation_id = ?5
                 )",
                &[id.into(), number(next), number(bytes), change.operation_id.into()],
            )?,
        ];
        let write = self.db.batch(statements).await;
        let receipt = self.room_receipt(id, change.operation_id).await?;
        if receipt.is_none() {
            write?;
        }
        Ok(receipt)
    }
}

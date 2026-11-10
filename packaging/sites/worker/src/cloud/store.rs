use super::protocol::MAX_REVISION;
use serde::{Deserialize, Serialize};
use worker::{D1Database, D1PreparedStatement, Result, wasm_bindgen::JsValue};

const UPLOAD_LEASE_SECONDS: u32 = 120;
pub(super) const PAGE_SIZE: usize = 50;
const MAINTENANCE_BATCH: u32 = 4;

pub(super) struct Store {
    pub db: D1Database,
    pub owner: String,
}

#[derive(Deserialize, Serialize)]
pub(super) struct Entry {
    pub id: String,
    pub name: String,
    pub revision: u64,
    pub byte_size: u64,
    pub saved_at: u64,
}

#[derive(Deserialize)]
pub(super) struct Document {
    #[serde(flatten)]
    pub entry: Entry,
    pub object_key: String,
}

pub(super) struct SaveRequest {
    pub operation_id: String,
    pub document_id: Option<String>,
    pub base_revision: u64,
    pub name: Option<String>,
    pub byte_size: u64,
    pub sha256: String,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Outcome {
    Pending,
    Committed,
    Stale,
    Abandoned,
    Invalid,
    TooLarge,
    Deleted,
}

#[derive(Clone, Deserialize)]
pub(super) struct Save {
    pub operation_id: String,
    pub document_id: String,
    pub base_revision: u64,
    pub name: String,
    pub byte_size: u64,
    pub sha256: String,
    pub object_key: String,
    pub outcome: Outcome,
    pub saved_at: Option<u64>,
    pub expired: u32,
    pub deleted: u32,
}

impl Save {
    pub fn matches(&self, request: &SaveRequest) -> bool {
        let target_matches = match &request.document_id {
            Some(id) => self.document_id == *id,
            None => self.base_revision == 0,
        };
        target_matches
            && self.base_revision == request.base_revision
            && request.name.as_ref().is_none_or(|name| self.name == *name)
            && self.byte_size == request.byte_size
            && self.sha256 == request.sha256
    }

    pub fn receipt(&self) -> Result<Entry> {
        Ok(Entry {
            id: self.document_id.clone(),
            name: self.name.clone(),
            revision: self.base_revision.checked_add(1).filter(|revision| *revision <= MAX_REVISION).ok_or("Invalid saved revision")?,
            byte_size: self.byte_size,
            saved_at: self.saved_at.ok_or("Missing save receipt time")?,
        })
    }
}

#[derive(Deserialize)]
pub(super) struct Cleanup {
    pub document_id: String,
    pub operation_id: String,
    pub object_key: String,
    pub settled: u32,
}

const READ_SAVE: &str = "SELECT s.*, CAST(s.lease_until <= unixepoch() AS INTEGER) AS expired, COALESCE(d.deleted, 0) AS deleted
                         FROM cloud_saves s LEFT JOIN cloud_documents d ON d.owner = s.owner AND d.id = s.document_id";

impl Store {
    fn statement(&self, sql: &str, args: &[JsValue]) -> Result<D1PreparedStatement> {
        let bindings = [vec![JsValue::from_str(&self.owner)], args.to_vec()].concat();
        self.db.prepare(sql).bind(&bindings)
    }

    pub async fn list(&self, before: Option<(u64, &str)>) -> Result<Vec<Entry>> {
        let (saved_at, id) = before.map_or((JsValue::NULL, JsValue::NULL), |(time, id)| (number(time), id.into()));
        self.statement(
            "SELECT id, name, revision, byte_size, saved_at FROM cloud_documents
             WHERE owner = ?1 AND deleted = 0 AND (?2 IS NULL OR (saved_at, id) < (?2, ?3))
             ORDER BY saved_at DESC, id DESC LIMIT ?4",
            &[saved_at, id, number((PAGE_SIZE + 1) as u64)],
        )?
        .all()
        .await?
        .results()
    }

    pub async fn document(&self, id: &str) -> Result<Option<Document>> {
        self.statement(
            "SELECT id, name, revision, object_key, byte_size, saved_at FROM cloud_documents
             WHERE owner = ?1 AND id = ?2 AND deleted = 0",
            &[id.into()],
        )?
        .first(None)
        .await
    }

    pub async fn save(&self, operation_id: &str) -> Result<Option<Save>> {
        self.statement(&format!("{READ_SAVE} WHERE s.owner = ?1 AND s.operation_id = ?2"), &[operation_id.into()])?.first(None).await
    }

    pub async fn create_tombstone(&self, operation_id: &str) -> Result<bool> {
        self.statement("SELECT id FROM cloud_documents WHERE owner = ?1 AND first_operation_id = ?2 AND deleted = 1", &[operation_id.into()])?
            .first::<String>(Some("id"))
            .await
            .map(|id| id.is_some())
    }

    pub async fn reserve(&self, request: &SaveRequest) -> Result<Option<Save>> {
        let document = request.document_id.as_deref().map_or(JsValue::NULL, JsValue::from_str);
        let name = request.name.as_deref().map_or(JsValue::NULL, JsValue::from_str);
        // Only the request that inserts the row can upload. Retries only reconcile.
        self.statement(
            "INSERT INTO cloud_saves (owner, operation_id, document_id, base_revision, name, byte_size, sha256, object_key, lease_until)
             SELECT ?1, ?2, COALESCE(?3, lower(hex(randomblob(16)))), ?4, COALESCE(?5, d.name), ?6, ?7,
                    'wordcraft-cloud/' || lower(hex(randomblob(16))), unixepoch() + ?8
             FROM (SELECT 1) LEFT JOIN cloud_documents d ON d.owner = ?1 AND d.id = ?3 AND d.deleted = 0 AND d.revision = ?4
             WHERE (?3 IS NULL OR d.id IS NOT NULL)
               AND NOT EXISTS (SELECT 1 FROM cloud_documents WHERE owner = ?1 AND first_operation_id = ?2 AND deleted = 1)
             ON CONFLICT (owner, operation_id) DO NOTHING
             RETURNING *, 0 AS expired, 0 AS deleted",
            &[
                request.operation_id.as_str().into(),
                document,
                number(request.base_revision),
                name,
                number(request.byte_size),
                request.sha256.as_str().into(),
                UPLOAD_LEASE_SECONDS.into(),
            ],
        )?
        .first(None)
        .await
    }

    pub async fn publish(&self, save: &Save) -> Result<Save> {
        let args = &[save.operation_id.as_str().into(), save.object_key.as_str().into()];
        let statements = vec![
            self.statement(
                "INSERT INTO cloud_documents (owner, id, first_operation_id, name, revision, object_key, byte_size, saved_at)
                 SELECT owner, document_id, operation_id, name, base_revision + 1, object_key, byte_size, unixepoch()
                 FROM cloud_saves s WHERE s.owner = ?1 AND s.operation_id = ?2 AND s.object_key = ?3 AND s.outcome = 'pending'
                   AND (s.base_revision = 0 OR EXISTS (
                     SELECT 1 FROM cloud_documents d
                     WHERE d.owner = ?1 AND d.id = s.document_id AND d.deleted = 0 AND d.revision = s.base_revision
                   ))
                 ON CONFLICT (owner, id) DO UPDATE SET revision = excluded.revision, object_key = excluded.object_key,
                     byte_size = excluded.byte_size, saved_at = excluded.saved_at
                 WHERE cloud_documents.deleted = 0 AND cloud_documents.revision = excluded.revision - 1",
                args,
            )?,
            // The receipt is conditional on the pointer, so a lost CAS cannot commit the ledger.
            self.statement(
                "UPDATE cloud_saves AS s
                 SET outcome = 'committed', settled = 1, saved_at = d.saved_at
                 FROM cloud_documents d
                 WHERE s.owner = ?1 AND s.operation_id = ?2 AND s.object_key = ?3 AND s.outcome = 'pending'
                   AND d.owner = ?1 AND d.id = s.document_id AND d.deleted = 0 AND d.object_key = s.object_key",
                args,
            )?,
            self.statement(
                "UPDATE cloud_saves AS previous SET cleanup = 'pending'
                 FROM cloud_saves winner JOIN cloud_documents d ON d.owner = winner.owner AND d.id = winner.document_id
                 WHERE previous.owner = ?1 AND winner.owner = ?1 AND winner.operation_id = ?2 AND winner.object_key = ?3 AND winner.outcome = 'committed'
                   AND d.deleted = 0 AND d.object_key = winner.object_key
                   AND previous.document_id = winner.document_id AND previous.base_revision = winner.base_revision - 1
                   AND previous.outcome = 'committed' AND previous.cleanup = 'none'",
                args,
            )?,
            self.statement(
                "UPDATE cloud_saves AS s SET outcome = CASE WHEN EXISTS (
                    SELECT 1 FROM cloud_documents WHERE owner = ?1 AND id = s.document_id AND deleted = 1
                 ) THEN 'deleted' ELSE 'stale' END, settled = 1, cleanup = 'pending'
                 WHERE s.owner = ?1 AND s.operation_id = ?2 AND s.object_key = ?3 AND s.outcome = 'pending'",
                args,
            )?,
        ];
        // An unavailable acknowledgement can follow a successful transaction.
        let write = self.db.batch(statements).await;
        self.resolve_write(save, write).await
    }

    pub async fn seal(&self, save: &Save, outcome: &str, settled: bool) -> Result<Save> {
        let statement = self.statement(
            "UPDATE cloud_saves SET outcome = ?4, settled = ?5, cleanup = 'pending'
             WHERE owner = ?1 AND operation_id = ?2 AND object_key = ?3 AND outcome = 'pending'",
            &[save.operation_id.as_str().into(), save.object_key.as_str().into(), outcome.into(), u32::from(settled).into()],
        )?;
        let write = statement.run().await;
        self.resolve_write(save, write).await
    }

    async fn resolve_write<T>(&self, save: &Save, write: Result<T>) -> Result<Save> {
        match self.save(&save.operation_id).await? {
            Some(row) if row.object_key == save.object_key && !matches!(row.outcome, Outcome::Pending) => Ok(row),
            _ if self.deleted(&save.document_id).await? == Some(1) => Ok(Save { deleted: 1, outcome: Outcome::Deleted, ..save.clone() }),
            _ => Err(write.err().unwrap_or_else(|| "Save outcome is still pending".into())),
        }
    }

    pub async fn deleted(&self, document_id: &str) -> Result<Option<u32>> {
        self.statement("SELECT deleted FROM cloud_documents WHERE owner = ?1 AND id = ?2", &[document_id.into()])?.first(Some("deleted")).await
    }

    pub async fn tombstone(&self, document_id: &str, revision: u64) -> Result<Option<u32>> {
        let statements = vec![
            self.statement(
                "UPDATE cloud_documents SET deleted = 1, name = NULL, revision = NULL, object_key = NULL, byte_size = NULL, saved_at = NULL
                 WHERE owner = ?1 AND id = ?2 AND deleted = 0 AND revision = ?3",
                &[document_id.into(), number(revision)],
            )?,
            self.statement(
                "UPDATE cloud_saves SET outcome = CASE WHEN outcome = 'pending' THEN 'deleted' ELSE outcome END, cleanup = 'pending'
                 WHERE owner = ?1 AND document_id = ?2 AND outcome IN ('pending', 'committed') AND cleanup = 'none'
                   AND EXISTS (SELECT 1 FROM cloud_documents WHERE owner = ?1 AND id = ?2 AND deleted = 1)",
                &[document_id.into()],
            )?,
        ];
        let write = self.db.batch(statements).await;
        let deleted = self.deleted(document_id).await?;
        if deleted != Some(1) {
            write?;
        }
        Ok(deleted)
    }

    pub async fn expired(&self) -> Result<Vec<Save>> {
        self.statement(
            &format!(
                "{READ_SAVE} WHERE s.owner = ?1 AND s.outcome = 'pending' AND s.lease_until <= unixepoch()
                      ORDER BY s.lease_until, s.operation_id LIMIT ?2"
            ),
            &[MAINTENANCE_BATCH.into()],
        )?
        .all()
        .await?
        .results()
    }

    pub async fn cleanup(&self, document_id: Option<&str>) -> Result<Vec<Cleanup>> {
        self.statement(
            "SELECT operation_id, document_id, object_key, settled FROM cloud_saves
             WHERE owner = ?1 AND cleanup = 'pending' AND (?2 IS NULL OR document_id = ?2)
             ORDER BY document_id, operation_id LIMIT ?3",
            &[document_id.map_or(JsValue::NULL, JsValue::from_str), MAINTENANCE_BATCH.into()],
        )?
        .all()
        .await?
        .results()
    }

    pub async fn confirm_cleanup(&self, item: &Cleanup) -> Result<()> {
        let statements = vec![
            self.statement(
                "UPDATE cloud_saves SET cleanup = 'done' WHERE owner = ?1 AND operation_id = ?2 AND object_key = ?3 AND cleanup = 'pending'",
                &[item.operation_id.as_str().into(), item.object_key.as_str().into()],
            )?,
            self.statement(
                "DELETE FROM cloud_saves WHERE owner = ?1 AND document_id = ?2 AND cleanup = 'done'
                 AND EXISTS (SELECT 1 FROM cloud_documents WHERE owner = ?1 AND id = ?2 AND deleted = 1)",
                &[item.document_id.as_str().into()],
            )?,
        ];
        self.db.batch(statements).await?;
        Ok(())
    }

    pub async fn has_cleanup(&self, document_id: &str) -> Result<bool> {
        self.statement(
            "SELECT operation_id FROM cloud_saves WHERE owner = ?1 AND document_id = ?2 AND cleanup = 'pending' LIMIT 1",
            &[document_id.into()],
        )?
        .first::<String>(Some("operation_id"))
        .await
        .map(|row| row.is_some())
    }
}

fn number(value: u64) -> JsValue {
    JsValue::from_f64(value as f64)
}

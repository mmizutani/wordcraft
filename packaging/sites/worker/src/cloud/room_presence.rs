//! Room presence is a short-lived view of authenticated browser sessions.

use futures_util::StreamExt as _;
use serde::{Deserialize, Serialize};
use wordcraft_doc::change::Anchor;
use worker::wasm_bindgen::JsValue;
use worker::{D1Database, Date, Request, Response};

use super::{
    protocol::{Failure, INVALID, MAX_BYTES, MAX_REVISION, TOO_LARGE, operation_id},
    room_access::{RoomAccess, RoomRole, display_name, resolve},
    routes::{ApiResult, STORAGE_ERROR},
};

const MAX_SESSIONS: u32 = 64;
const TTL_MS: u64 = 15_000;
const MIN_UPDATE_MS: u64 = 250;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Position {
    anchor: Option<Anchor>,
    focus: Option<Anchor>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Heartbeat {
    session_id: String,
    revision: u64,
    position: Position,
}

#[derive(Deserialize)]
struct StoredPresence {
    session_id: String,
    role: RoomRole,
    display_name: Option<String>,
    revision: u64,
    position_json: String,
}

#[derive(Serialize)]
struct Participant {
    session_id: String,
    role: RoomRole,
    display_name: Option<String>,
    revision: u64,
    position: Position,
}

#[derive(Serialize)]
struct Presence {
    participants: Vec<Participant>,
}

pub(super) async fn dispatch(db: &D1Database, actor: &str, request: &mut Request, id: &str) -> ApiResult<Response> {
    let access = resolve(db, id, actor).await?;
    let now = Date::now().as_millis();
    match request.inner().method().as_str() {
        "GET" => list(db, id, actor, &access, now).await,
        "POST" => {
            let heartbeat = read_heartbeat(request).await?;
            update(db, id, actor, &access, now, heartbeat, display_name(request)?).await
        }
        _ => Err(Failure(405, "method_not_allowed")),
    }
}

async fn list(db: &D1Database, id: &str, actor: &str, access: &RoomAccess, now: u64) -> ApiResult<Response> {
    let rows = db
        .prepare(
            "SELECT p.session_id, CASE WHEN p.actor = p.owner THEN 'owner' ELSE g.role END AS role,
                    COALESCE(p.display_name, g.display_name) AS display_name, p.revision, p.position_json
             FROM cloud_room_presence p
             JOIN cloud_documents d ON (d.owner, d.id) = (p.owner, p.document_id)
             LEFT JOIN cloud_room_grants g ON (g.owner, g.document_id, g.actor) = (p.owner, p.document_id, p.actor)
             WHERE (p.owner, p.document_id) = (?1, ?2) AND d.deleted = 0 AND p.expires_ms > ?5
               AND (p.actor = p.owner OR (g.role IN ('viewer', 'editor') AND g.grant_version = p.grant_version))
               AND (?3 = p.owner OR EXISTS (
                    SELECT 1 FROM cloud_room_grants mine
                    WHERE (mine.owner, mine.document_id, mine.actor) = (p.owner, p.document_id, ?3)
                      AND mine.role IN ('viewer', 'editor') AND mine.grant_version = ?4))
             ORDER BY p.session_id LIMIT ?6",
        )
        .bind(&[access.owner.as_str().into(), id.into(), actor.into(), number(access.grant_version), number(now), MAX_SESSIONS.into()])?
        .all()
        .await?
        .results::<StoredPresence>()?;
    let participants = rows
        .into_iter()
        .map(|row| {
            Ok(Participant {
                session_id: row.session_id,
                role: row.role,
                display_name: row.display_name,
                revision: row.revision,
                position: serde_json::from_str(&row.position_json).map_err(|_| STORAGE_ERROR)?,
            })
        })
        .collect::<ApiResult<Vec<_>>>()?;
    Ok(Response::from_json(&Presence { participants })?)
}

async fn update(
    db: &D1Database,
    id: &str,
    actor: &str,
    access: &RoomAccess,
    now: u64,
    heartbeat: Heartbeat,
    name: Option<String>,
) -> ApiResult<Response> {
    let position = serde_json::to_string(&heartbeat.position).map_err(|_| INVALID)?;
    let owner = access.owner.as_str();
    let name = name.as_deref().map_or(JsValue::NULL, JsValue::from_str);
    let cleanup = db.prepare("DELETE FROM cloud_room_presence WHERE (owner, document_id) = (?1, ?2) AND expires_ms <= ?3").bind(&[
        owner.into(),
        id.into(),
        number(now),
    ])?;
    let insert = db
        .prepare(
            "INSERT INTO cloud_room_presence
                (owner, document_id, actor, session_id, grant_version, revision, position_json, display_name, updated_ms, expires_ms)
             SELECT r.owner, r.document_id, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10
             FROM cloud_rooms r JOIN cloud_documents d ON (d.owner, d.id) = (r.owner, r.document_id)
             WHERE (r.owner, r.document_id) = (?1, ?2) AND d.deleted = 0 AND ?6 BETWEEN r.checkpoint_revision AND r.head
               AND (?3 = r.owner OR EXISTS (
                    SELECT 1 FROM cloud_room_grants g WHERE (g.owner, g.document_id, g.actor) = (r.owner, r.document_id, ?3)
                       AND g.role IN ('editor', 'viewer') AND g.grant_version = ?5))
               AND ((SELECT count(*) FROM cloud_room_presence WHERE (owner, document_id) = (?1, ?2)) < ?12
                    OR EXISTS (SELECT 1 FROM cloud_room_presence WHERE (owner, document_id, session_id, actor) = (?1, ?2, ?4, ?3)))
             ON CONFLICT (owner, document_id, session_id) DO UPDATE SET
                 grant_version = excluded.grant_version, revision = excluded.revision,
                 position_json = excluded.position_json, display_name = excluded.display_name,
                 updated_ms = excluded.updated_ms, expires_ms = excluded.expires_ms
             WHERE actor = ?3 AND updated_ms <= ?11 AND revision <= excluded.revision",
        )
        .bind(&[
            owner.into(),
            id.into(),
            actor.into(),
            heartbeat.session_id.as_str().into(),
            number(access.grant_version),
            number(heartbeat.revision),
            position.as_str().into(),
            name,
            number(now),
            number(now.saturating_add(TTL_MS)),
            number(now.saturating_sub(MIN_UPDATE_MS)),
            MAX_SESSIONS.into(),
        ])?;
    db.batch(vec![cleanup, insert]).await?;
    Ok(Response::empty()?.with_status(204))
}

async fn read_heartbeat(request: &mut Request) -> ApiResult<Heartbeat> {
    let mut stream = request.stream()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if chunk.len() > 2048usize.saturating_sub(bytes.len()) {
            return Err(TOO_LARGE);
        }
        bytes.extend_from_slice(&chunk);
    }
    let mut data: Heartbeat = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
    data.session_id = operation_id(Some(&data.session_id))?;
    if data.revision > MAX_REVISION
        || data.position.anchor.is_some() != data.position.focus.is_some()
        || [data.position.anchor, data.position.focus].into_iter().flatten().any(|anchor| anchor.off as u64 > MAX_BYTES)
    {
        return Err(INVALID);
    }
    Ok(data)
}

fn number(value: u64) -> JsValue {
    JsValue::from_f64(value as f64)
}

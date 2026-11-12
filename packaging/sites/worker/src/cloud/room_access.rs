//! An admitted Site visitor may request access. The document owner grants a role
//! to the Site-scoped principal, independently of the display name.

use futures_util::StreamExt as _;
use serde::{Deserialize, Serialize};
use worker::wasm_bindgen::JsValue;
use worker::{D1Database, D1PreparedStatement, Request, Response};

use super::protocol::{Failure, INVALID, TOO_LARGE, opaque_id};
use super::routes::ApiResult;

const UNAVAILABLE: Failure = Failure(404, "room_not_found");
const LIMIT: Failure = Failure(409, "room_access_limit");
const MAX_GRANTS: u32 = 32;
const MAX_LABEL: usize = 256;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RoomRole {
    Owner,
    Editor,
    Viewer,
}

#[derive(Deserialize)]
pub(super) struct RoomAccess {
    pub owner: String,
    pub role: RoomRole,
    pub grant_version: u64,
}

#[derive(Deserialize, Serialize)]
struct Grant {
    id: String,
    display_name: Option<String>,
    role: String,
}

#[derive(Serialize)]
struct AccessStatus<'a> {
    state: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_code: Option<&'a str>,
}

#[derive(Serialize)]
struct Grants {
    grants: Vec<Grant>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantChange {
    role: GrantedRole,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum GrantedRole {
    Editor,
    Viewer,
    Revoked,
}

impl GrantedRole {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Editor => "editor",
            Self::Viewer => "viewer",
            Self::Revoked => "revoked",
        }
    }
}

pub(super) async fn resolve(db: &D1Database, id: &str, actor: &str) -> ApiResult<RoomAccess> {
    statement(
        db,
        "SELECT r.owner, CASE WHEN r.owner = ?2 THEN 'owner' ELSE g.role END AS role,
                COALESCE(g.grant_version, 0) AS grant_version
         FROM cloud_rooms r JOIN cloud_documents d ON (d.owner, d.id) = (r.owner, r.document_id)
         LEFT JOIN cloud_room_grants g ON (g.owner, g.document_id, g.actor) = (r.owner, r.document_id, ?2)
         WHERE r.document_id = ?1 AND d.deleted = 0 AND (r.owner = ?2 OR g.role IN ('editor', 'viewer'))",
        &[id.into(), actor.into()],
    )?
    .first(None)
    .await?
    .ok_or(UNAVAILABLE)
}

pub(super) async fn dispatch(db: &D1Database, actor: &str, request: &mut Request, id: &str, route: &str) -> ApiResult<Response> {
    let owner = owner(db, id).await?;
    let method = request.inner().method();
    if route == "access" {
        return match method.as_str() {
            "GET" => access_status(db, &owner, id, actor).await,
            "POST" => request_access(db, &owner, id, actor, display_name(request)?).await,
            _ => Err(Failure(405, "method_not_allowed")),
        };
    }
    if actor != owner {
        return Err(UNAVAILABLE);
    }
    match (method.as_str(), route) {
        ("GET", "grants") => list_grants(db, &owner, id).await,
        ("PUT", path) => {
            let grant = path.strip_prefix("grants/").filter(|grant| opaque_id(grant)).ok_or(UNAVAILABLE)?;
            let change = read_grant_change(request).await?;
            change_grant(db, &owner, id, grant, &change.role).await
        }
        _ => Err(Failure(405, "method_not_allowed")),
    }
}

async fn owner(db: &D1Database, id: &str) -> ApiResult<String> {
    statement(
        db,
        "SELECT r.owner FROM cloud_rooms r
         JOIN cloud_documents d ON (d.owner, d.id) = (r.owner, r.document_id)
         WHERE r.document_id = ?1 AND d.deleted = 0",
        &[id.into()],
    )?
    .first(Some("owner"))
    .await?
    .ok_or(UNAVAILABLE)
}

async fn access_status(db: &D1Database, owner: &str, id: &str, actor: &str) -> ApiResult<Response> {
    if actor == owner {
        return status("owner", None);
    }
    match own_grant(db, owner, id, actor).await? {
        Some(grant) => status(&grant.role, Some(&grant.id)),
        None => status("unrequested", None),
    }
}

async fn request_access(db: &D1Database, owner: &str, id: &str, actor: &str, display_name: Option<String>) -> ApiResult<Response> {
    if actor == owner {
        return status("owner", None);
    }
    if let Some(grant) = own_grant(db, owner, id, actor).await? {
        return status(&grant.role, Some(&grant.id));
    }
    let display_name = display_name.as_deref().map_or(JsValue::NULL, JsValue::from_str);
    statement(
        db,
        "INSERT INTO cloud_room_grants (owner, document_id, actor, id, display_name)
         SELECT r.owner, r.document_id, ?3, lower(hex(randomblob(16))), ?4
         FROM cloud_rooms r JOIN cloud_documents d ON (d.owner, d.id) = (r.owner, r.document_id)
         WHERE (r.owner, r.document_id) = (?1, ?2) AND d.deleted = 0
           AND (SELECT count(*) FROM cloud_room_grants WHERE (owner, document_id) = (?1, ?2)) < ?5
         ON CONFLICT (owner, document_id, actor) DO NOTHING",
        &[owner.into(), id.into(), actor.into(), display_name, MAX_GRANTS.into()],
    )?
    .run()
    .await?;
    let grant = own_grant(db, owner, id, actor).await?.ok_or(LIMIT)?;
    status(&grant.role, Some(&grant.id))
}

async fn own_grant(db: &D1Database, owner: &str, id: &str, actor: &str) -> ApiResult<Option<Grant>> {
    Ok(statement(
        db,
        "SELECT g.id, g.display_name, g.role FROM cloud_room_grants g
         JOIN cloud_documents d ON (d.owner, d.id) = (g.owner, g.document_id)
         WHERE (g.owner, g.document_id, g.actor) = (?1, ?2, ?3) AND d.deleted = 0",
        &[owner.into(), id.into(), actor.into()],
    )?
    .first(None)
    .await?)
}

async fn list_grants(db: &D1Database, owner: &str, id: &str) -> ApiResult<Response> {
    let rows = statement(
        db,
        "SELECT g.id, g.display_name, g.role FROM cloud_room_grants g
         JOIN cloud_documents d ON (d.owner, d.id) = (g.owner, g.document_id)
         WHERE (g.owner, g.document_id) = (?1, ?2) AND d.deleted = 0 ORDER BY g.display_name, g.id LIMIT ?3",
        &[owner.into(), id.into(), MAX_GRANTS.into()],
    )?
    .all()
    .await?
    .results()?;
    Ok(Response::from_json(&Grants { grants: rows })?)
}

async fn change_grant(db: &D1Database, owner: &str, id: &str, grant: &str, role: &GrantedRole) -> ApiResult<Response> {
    let row: Grant = statement(
        db,
        "UPDATE cloud_room_grants
         SET role = ?4, grant_version = grant_version + CASE WHEN role = ?4 THEN 0 ELSE 1 END
         WHERE (owner, document_id, id) = (?1, ?2, ?3)
           AND EXISTS (SELECT 1 FROM cloud_documents WHERE (owner, id) = (?1, ?2) AND deleted = 0)
         RETURNING id, display_name, role",
        &[owner.into(), id.into(), grant.into(), role.as_str().into()],
    )?
    .first(None)
    .await?
    .ok_or(UNAVAILABLE)?;
    Ok(Response::from_json(&row)?)
}

async fn read_grant_change(request: &mut Request) -> ApiResult<GrantChange> {
    let mut stream = request.stream()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if chunk.len() > 1024usize.saturating_sub(bytes.len()) {
            return Err(TOO_LARGE);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| INVALID)
}

fn status(state: &str, request_code: Option<&str>) -> ApiResult<Response> {
    Ok(Response::from_json(&AccessStatus { state, request_code })?)
}

fn statement(db: &D1Database, sql: &str, values: &[JsValue]) -> ApiResult<D1PreparedStatement> {
    Ok(db.prepare(sql).bind(values)?)
}

pub(super) fn display_name(request: &Request) -> ApiResult<Option<String>> {
    let name = request.headers().get("oai-authenticated-user-full-name")?;
    let encoding = request.headers().get("oai-authenticated-user-full-name-encoding")?;
    let name = match encoding.as_deref() {
        Some("percent-encoded-utf-8") => name.as_deref().and_then(decode_percent),
        None => name,
        _ => None,
    };
    let fallback = request.headers().get("oai-authenticated-user-email")?;
    Ok(name.and_then(label).or_else(|| fallback.and_then(label)))
}

fn label(text: String) -> Option<String> {
    let text = text.trim();
    (!text.is_empty() && text.len() <= MAX_LABEL && !text.chars().any(char::is_control)).then(|| text.to_owned())
}

fn decode_percent(text: &str) -> Option<String> {
    if text.len() > MAX_LABEL * 3 {
        return None;
    }
    let mut input = text.bytes();
    let mut bytes = Vec::with_capacity(text.len());
    while let Some(byte) = input.next() {
        let decoded = if byte == b'%' {
            let hi = char::from(input.next()?).to_digit(16)?;
            let lo = char::from(input.next()?).to_digit(16)?;
            u8::try_from(hi * 16 + lo).ok()?
        } else {
            byte
        };
        bytes.push(decoded);
    }
    String::from_utf8(bytes).ok()
}

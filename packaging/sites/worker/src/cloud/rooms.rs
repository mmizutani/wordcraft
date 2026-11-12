use super::{
    protocol::{Failure, INVALID, MAX_BYTES, MAX_REVISION, TOO_LARGE, digest, opaque_id, operation_id},
    room_access::{self, RoomAccess, RoomRole},
    room_presence,
    room_store::{MAX_CHANGE_BYTES, MAX_TAIL_BYTES, MAX_TAIL_CHANGES, Room, RoomAppend, RoomChange, RoomReceipt},
    routes::{
        Api, ApiResult, DOCX, ErrorBody, INVALID_BODY, OPERATION_MISMATCH, STALE, STORAGE_ERROR, create_only, etag, save_response, valid_candidate,
    },
    store::{Save, SaveRequest, Store},
};
use crate::room_model::{RoomModelError, checkpoint, propose, replay, write_checkpoint};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::fmt::Write;
use wordcraft_doc::{ChangeSet, Document};
use worker::{D1Database, Env, Request, Response, console_log};

const ROOT: &str = "/api/cloud/rooms";
const ROLE_CHANGED: Failure = Failure(403, "room_role_changed");
const READ_ONLY: Failure = Failure(403, "room_read_only");
const GONE: Failure = Failure(404, "room_not_found");
const CHANGE_CONFLICT: Failure = Failure(409, "change_conflict");
pub(super) const STALE_ROOM: Failure = Failure(409, "stale_room_revision");
const ROOM_FULL: Failure = Failure(409, "room_storage_limit");
const ROOM_CHANGED: Failure = Failure(409, "room_changed");
const INVALID_CHECKPOINT: Failure = Failure(422, "invalid_checkpoint");
const UNSUPPORTED: Failure = Failure(422, "unsupported_room_document");

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Activate {
    file_revision: u64,
}

#[derive(Serialize)]
struct Metadata<'a> {
    id: &'a str,
    role: RoomRole,
    file_revision: u64,
    published_room_revision: u64,
    checkpoint_revision: u64,
    head: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishRoom {
    operation_id: String,
    room_revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Submission {
    operation_id: String,
    base_revision: u64,
    change: serde_json::Value,
}

#[derive(Serialize)]
struct Accepted {
    revision: u64,
    head: u64,
}

#[derive(Serialize)]
struct Change {
    revision: u64,
    operation_id: String,
    change: ChangeSet,
}

#[derive(Serialize)]
struct Page {
    role: RoomRole,
    head: u64,
    changes: Vec<Change>,
    next_after: Option<u64>,
}

pub(super) fn handles(path: &str) -> bool {
    path == ROOT || path.strip_prefix(ROOT).is_some_and(|rest| rest.starts_with('/'))
}

struct Caller<'a> {
    actor: &'a str,
    access: RoomAccess,
}

impl Caller<'_> {
    async fn check(&self, db: &D1Database, id: &str) -> ApiResult<()> {
        let current = room_access::resolve(db, id, self.actor).await?;
        if current.owner != self.access.owner || current.role != self.access.role || current.grant_version != self.access.grant_version {
            return Err(ROLE_CHANGED);
        }
        Ok(())
    }
}

pub(super) async fn dispatch(api: &Api, request: &mut Request, path: &str, env: &Env) -> ApiResult<Response> {
    let tail = path.strip_prefix(ROOT).and_then(|tail| tail.strip_prefix('/')).ok_or(GONE)?;
    let (id, part) = tail.split_once('/').unwrap_or((tail, ""));
    if !opaque_id(id) {
        return Err(GONE);
    }
    let method = request.inner().method();
    let actor = api.store.owner.as_str();
    if part == "access" || part == "grants" || part.starts_with("grants/") {
        return room_access::dispatch(&api.store.db, actor, request, id, part).await;
    }
    if part == "presence" {
        return room_presence::dispatch(&api.store.db, actor, request, id).await;
    }
    if method == "POST" && part.is_empty() {
        return api.activate_room(id, read_json::<Activate>(request, 256).await?).await;
    }
    let caller = Caller { actor, access: room_access::resolve(&api.store.db, id, actor).await? };
    let scoped = Api { store: Store { db: env.d1("DB")?, owner: caller.access.owner.clone() }, bucket: env.bucket("BUCKET")? };
    let response = match (method.as_str(), part) {
        ("GET", "") => metadata(&scoped.require_room(id).await?, caller.access.role, 200),
        ("GET", "checkpoint") => scoped.get_checkpoint(id).await,
        ("GET", "changes") => scoped.get_changes(id, request.url()?.query(), caller.access.role).await,
        ("POST", "changes") => {
            if caller.access.role == RoomRole::Viewer {
                return Err(READ_ONLY);
            }
            scoped.submit_change(id, read_json::<Submission>(request, MAX_CHANGE_BYTES + 256).await?, &caller).await
        }
        ("POST", "save") => {
            if caller.access.role != RoomRole::Owner {
                return Err(Failure(403, "room_owner_required"));
            }
            scoped.publish_room(id, read_json::<PublishRoom>(request, 256).await?).await
        }
        (_, "" | "changes" | "checkpoint" | "save") => {
            let mut response = Response::from_json(&ErrorBody { code: "method_not_allowed" })?.with_status(405);
            let allowed = match part {
                "checkpoint" => "GET",
                "save" => "POST",
                _ => "GET, POST",
            };
            response.headers_mut().set("Allow", allowed)?;
            Ok(response)
        }
        _ => Err(GONE),
    };
    caller.check(&api.store.db, id).await?;
    response
}

impl Api {
    async fn require_room(&self, id: &str) -> ApiResult<Room> {
        self.store.room(id).await?.ok_or(GONE)
    }

    async fn activate_room(&self, id: &str, input: Activate) -> ApiResult<Response> {
        let revision = input.file_revision;
        if !(1..=MAX_REVISION).contains(&revision) {
            return Err(INVALID);
        }
        if let Some(room) = self.store.room(id).await? {
            return same_activation(&room, revision);
        }

        let source = self.store.document(id).await?.ok_or(GONE)?;
        if source.entry.revision != revision {
            return Err(STALE);
        }
        let Some(object) = self.bucket.get(&source.object_key).execute().await? else {
            return self.activation_race(id, revision, STORAGE_ERROR).await;
        };
        if object.size() != source.entry.byte_size || object.size() > MAX_BYTES {
            return self.activation_race(id, revision, STORAGE_ERROR).await;
        }
        let bytes = object.body().ok_or(STORAGE_ERROR)?.bytes().await?;
        let bytes = checkpoint(&bytes).map_err(activation_error)?;
        let Some(key) = self.store.reserve_checkpoint(id, revision).await? else {
            return self.activation_race(id, revision, Failure(409, "activation_pending")).await;
        };

        self.put_checkpoint(id, revision, &key, bytes).await
    }

    async fn put_checkpoint(&self, id: &str, revision: u64, key: &str, bytes: Vec<u8>) -> ApiResult<Response> {
        let byte_size = bytes.len() as u64;
        if !(1..=MAX_BYTES).contains(&byte_size) {
            self.store.settle_checkpoint(key).await?;
            return Err(TOO_LARGE);
        }
        let checksum = Sha256::digest(&bytes);
        let put = self.bucket.put(key, bytes).only_if(create_only()).sha256(checksum.to_vec()).execute().await;
        let result = match put {
            Ok(Some(object)) if valid_candidate(&object, byte_size, &checksum) => {
                self.store.publish_checkpoint(id, key, byte_size, &hex_checksum(&checksum)).await?
            }
            _ => {
                self.store.settle_checkpoint(key).await?;
                None
            }
        };

        self.try_room_cleanup(Some(id)).await;
        if let Some(room) = result {
            if room.checkpoint_key == key {
                return metadata(&room, RoomRole::Owner, 201);
            }
            return same_activation(&room, revision);
        }
        self.activation_race(id, revision, STORAGE_ERROR).await
    }

    async fn activation_race(&self, id: &str, revision: u64, fallback: Failure) -> ApiResult<Response> {
        if let Some(room) = self.store.room(id).await? {
            return same_activation(&room, revision);
        }
        let current = self.store.document(id).await?.ok_or(GONE)?;
        Err(if current.entry.revision == revision { fallback } else { STALE })
    }

    pub(super) async fn try_room_cleanup(&self, id: Option<&str>) {
        let Ok(items) = self.store.room_cleanup(id).await else {
            console_log!("cloud_rooms cleanup_pending");
            return;
        };
        for item in items {
            let result = if item.settled == 1 { self.purge(&item.object_key).await } else { self.fence(&item.object_key).await };
            if result.is_err() || self.store.confirm_room_cleanup(&item).await.is_err() {
                console_log!("cloud_rooms cleanup_pending");
            }
        }
    }

    async fn get_checkpoint(&self, id: &str) -> ApiResult<Response> {
        let room = self.require_room(id).await?;
        let bytes = self.checkpoint_bytes(&room).await?;
        self.require_room(id).await?;
        let mut response = Response::from_bytes(bytes)?;
        response.headers_mut().set("Content-Type", DOCX)?;
        response.headers_mut().set("ETag", &etag(room.checkpoint_revision))?;
        Ok(response)
    }

    async fn checkpoint_bytes(&self, room: &Room) -> ApiResult<Vec<u8>> {
        let checksum = digest(Some(&room.checkpoint_sha256)).map_err(|_| STORAGE_ERROR)?;
        let object = self.bucket.get(&room.checkpoint_key).execute().await?;
        let Some(object) = object.filter(|object| room.checkpoint_size <= MAX_BYTES && valid_candidate(object, room.checkpoint_size, &checksum))
        else {
            self.require_room(&room.document_id).await?;
            return Err(STORAGE_ERROR);
        };
        let bytes = object.body().ok_or(STORAGE_ERROR)?.bytes().await?;
        if Sha256::digest(&bytes).as_slice() != checksum {
            return Err(STORAGE_ERROR);
        }
        Ok(bytes)
    }

    async fn get_changes(&self, id: &str, query: Option<&str>, role: RoomRole) -> ApiResult<Response> {
        let room = self.require_room(id).await?;
        let (after, through) = page_window(query, &room)?;
        let entries = self.room_entries(id, after, through).await?;
        self.require_room(id).await?;
        let next_after = entries.last().map(|entry| entry.revision).filter(|last| *last < through);
        let changes = entries.into_iter().map(parse_change).collect::<ApiResult<_>>()?;
        Ok(Response::from_json(&Page { role, head: through, changes, next_after })?)
    }

    async fn room_entries(&self, id: &str, after: u64, through: u64) -> ApiResult<Vec<RoomChange>> {
        let entries = self.store.room_page(id, after, through).await?;
        if (entries.is_empty() && after < through) || entries.iter().zip(after + 1..).any(|(entry, expected)| entry.revision != expected) {
            self.require_room(id).await?;
            return Err(STORAGE_ERROR);
        }
        Ok(entries)
    }

    async fn submit_change(&self, id: &str, submission: Submission, caller: &Caller<'_>) -> ApiResult<Response> {
        let operation = operation_id(Some(&submission.operation_id))?;
        let bytes = serde_json::to_vec(&submission.change).map_err(|_| INVALID_BODY)?;
        if bytes.len() > MAX_CHANGE_BYTES {
            return Err(TOO_LARGE);
        }
        let mut hash = Sha256::new();
        hash.update(submission.base_revision.to_be_bytes());
        hash.update(bytes);
        let fingerprint = hex_checksum(&hash.finalize());
        let actor = caller.actor;
        if let Some(receipt) = self.store.room_receipt(id, &operation).await? {
            return receipt_response(&receipt, actor, &fingerprint);
        }
        self.require_room(id).await?;
        let change = serde_json::from_value(submission.change).map_err(|_| INVALID_BODY)?;
        for _ in 0..2 {
            let room = self.require_room(id).await?;
            let canonical = self.propose_change(&room, submission.base_revision, &change).await;
            // Another delivery may have committed this operation after the first receipt lookup.
            if let Some(receipt) = self.store.room_receipt(id, &operation).await? {
                return receipt_response(&receipt, actor, &fingerprint);
            }
            let json = serde_json::to_string(&canonical?).map_err(|_| STORAGE_ERROR)?;
            if json.len() > MAX_CHANGE_BYTES {
                return Err(TOO_LARGE);
            }
            if room.tail_bytes + json.len() as u64 > MAX_TAIL_BYTES {
                return Err(ROOM_FULL);
            }
            let append = RoomAppend {
                actor,
                grant_version: caller.access.grant_version,
                operation_id: &operation,
                fingerprint: &fingerprint,
                change_json: &json,
            };
            if let Some(receipt) = self.store.append_room_change(&room, &append).await? {
                return receipt_response(&receipt, actor, &fingerprint);
            }
        }
        self.require_room(id).await?;
        Err(ROOM_CHANGED)
    }

    async fn publish_room(&self, id: &str, request: PublishRoom) -> ApiResult<Response> {
        let operation = operation_id(Some(&request.operation_id))?;
        let target = request.room_revision;
        if let Some(row) = self.store.save(&operation).await? {
            return self.replay_room_save(id, target, row).await;
        }

        let room = self.require_room(id).await?;
        let candidate = self.prepare_room_save(&room, target).await;
        // A duplicate may have reserved or published while we reconstructed the revision.
        if let Some(row) = self.store.save(&operation).await? {
            return self.replay_room_save(id, target, row).await;
        }
        let bytes = candidate?;
        let checksum = Sha256::digest(&bytes);
        let input = SaveRequest {
            operation_id: operation,
            document_id: Some(id.to_owned()),
            base_revision: room.file_revision,
            room_revision: Some(target),
            name: None,
            byte_size: bytes.len() as u64,
            sha256: hex_checksum(&checksum),
        };
        let reservation = self.store.reserve(&input).await;
        if let Ok(Some(row)) = reservation {
            let row = self.upload_save_bytes(&row, bytes, &checksum).await?;
            self.try_cleanup(Some(id)).await;
            return save_response(&row);
        }
        if let Some(row) = self.store.save(&input.operation_id).await? {
            return self.replay_room_save(id, target, row).await;
        }
        reservation?;
        if self.store.create_tombstone(&input.operation_id).await? {
            return Err(OPERATION_MISMATCH);
        }
        check_save_target(&self.require_room(id).await?, target)?;
        Err(STORAGE_ERROR)
    }

    async fn replay_room_save(&self, id: &str, target: u64, row: Save) -> ApiResult<Response> {
        if row.document_id != id || row.room_revision != Some(target) {
            return Err(OPERATION_MISMATCH);
        }
        save_response(&self.recover_expired_save(row).await?)
    }

    async fn prepare_room_save(&self, room: &Room, target: u64) -> ApiResult<Vec<u8>> {
        check_save_target(room, target)?;
        let (document, _) = self.replay_through(room, target).await?;
        write_checkpoint(&document).map_err(activation_error)
    }

    async fn propose_change(&self, room: &Room, base: u64, change: &ChangeSet) -> ApiResult<ChangeSet> {
        if base < room.checkpoint_revision || base > room.head {
            return Err(STALE_ROOM);
        }
        if room.head == MAX_REVISION || room.head - room.checkpoint_revision >= MAX_TAIL_CHANGES {
            return Err(ROOM_FULL);
        }
        let (mut document, changes) = self.replay_through(room, room.head).await?;
        let since_base = changes.get((base - room.checkpoint_revision) as usize..).ok_or(STORAGE_ERROR)?;
        propose(&mut document, since_base, change.clone()).map_err(change_error)
    }

    async fn replay_through(&self, room: &Room, through: u64) -> ApiResult<(Document, Vec<ChangeSet>)> {
        let bytes = self.checkpoint_bytes(room).await?;
        let mut after = room.checkpoint_revision;
        let mut changes = Vec::new();
        while after < through {
            let entries = self.room_entries(&room.document_id, after, through).await?;
            after = entries.last().ok_or(STORAGE_ERROR)?.revision;
            for entry in entries {
                changes.push(parse_change(entry)?.change);
            }
        }
        let document = replay(&bytes, &changes).map_err(|_| STORAGE_ERROR)?;
        Ok((document, changes))
    }
}

fn check_save_target(room: &Room, target: u64) -> ApiResult<()> {
    if target < room.published_room_revision || !(room.checkpoint_revision..=room.head).contains(&target) {
        return Err(STALE_ROOM);
    }
    if room.file_revision == MAX_REVISION {
        return Err(ROOM_FULL);
    }
    Ok(())
}

fn metadata(room: &Room, role: RoomRole, status: u16) -> ApiResult<Response> {
    Ok(Response::from_json(&Metadata {
        id: &room.document_id,
        role,
        file_revision: room.file_revision,
        published_room_revision: room.published_room_revision,
        checkpoint_revision: room.checkpoint_revision,
        head: room.head,
    })?
    .with_status(status))
}

fn same_activation(room: &Room, revision: u64) -> ApiResult<Response> {
    if room.file_revision != revision {
        return Err(STALE);
    }
    metadata(room, RoomRole::Owner, 200)
}

fn parse_change(change: RoomChange) -> ApiResult<Change> {
    Ok(Change {
        revision: change.revision,
        operation_id: change.operation_id,
        change: serde_json::from_str(&change.change_json).map_err(|_| STORAGE_ERROR)?,
    })
}

fn receipt_response(receipt: &RoomReceipt, actor: &str, fingerprint: &str) -> ApiResult<Response> {
    if receipt.actor != actor || receipt.fingerprint != fingerprint {
        return Err(OPERATION_MISMATCH);
    }
    Ok(Response::from_json(&Accepted { revision: receipt.revision, head: receipt.revision })?)
}

fn page_window(query: Option<&str>, room: &Room) -> ApiResult<(u64, u64)> {
    let Some(query) = query else { return Ok((room.checkpoint_revision, room.head)) };
    let query = query.strip_prefix("after=").ok_or(INVALID)?;
    let (after, through) = match query.split_once("&through=") {
        Some((after, through)) => (page_revision(after)?, page_revision(through)?),
        None => (page_revision(query)?, room.head),
    };
    if after < room.checkpoint_revision || after > through || through > room.head {
        return Err(STALE_ROOM);
    }
    Ok((after, through))
}

fn page_revision(input: &str) -> ApiResult<u64> {
    if input.len() > 16 || !input.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(INVALID);
    }
    input.parse().ok().filter(|revision| *revision <= MAX_REVISION).ok_or(INVALID)
}

async fn read_json<T: DeserializeOwned>(request: &mut Request, max_bytes: usize) -> ApiResult<T> {
    if !request
        .headers()
        .get("Content-Type")?
        .is_some_and(|media| media.split(';').next().is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json")))
    {
        return Err(Failure(415, "unsupported_media_type"));
    }
    let mut stream = request.stream().map_err(|_| INVALID_BODY)?;
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| INVALID_BODY)?;
        if chunk.len() > max_bytes - bytes.len() {
            return Err(TOO_LARGE);
        }
        bytes.extend(chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| INVALID_BODY)
}

fn hex_checksum(bytes: &[u8]) -> String {
    let mut checksum = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(checksum, "{byte:02x}");
    }
    checksum
}

fn activation_error(error: RoomModelError) -> Failure {
    match error {
        RoomModelError::TooLarge => TOO_LARGE,
        RoomModelError::Unsupported => UNSUPPORTED,
        RoomModelError::Invalid | RoomModelError::Conflict => INVALID_CHECKPOINT,
    }
}

fn change_error(error: RoomModelError) -> Failure {
    match error {
        RoomModelError::TooLarge => TOO_LARGE,
        RoomModelError::Unsupported => UNSUPPORTED,
        RoomModelError::Invalid => INVALID_BODY,
        RoomModelError::Conflict => CHANGE_CONFLICT,
    }
}

use super::{
    protocol::{Failure, INVALID, MAX_BYTES, MAX_REVISION, TOO_LARGE, attachment, digest, filename, opaque_id, operation_id, revision, size},
    store::{Document, Entry, Outcome, PAGE_SIZE, Save, SaveRequest, Store},
};
use futures_util::{StreamExt, stream};
use serde::Serialize;
use std::{cell::Cell, rc::Rc};
use worker::{Bucket, Conditional, Date, Env, Error, FixedLengthStream, Headers, Object, Request, Response, Result, console_log};

const ROOT: &str = "/api/cloud/documents";
const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
const GONE: Failure = Failure(404, "document_not_found");
const STALE: Failure = Failure(412, "stale_revision");
const INVALID_BODY: Failure = Failure(400, "invalid_body");
const OPERATION_MISMATCH: Failure = Failure(409, "operation_mismatch");
const STORAGE_ERROR: Failure = Failure(503, "storage_unavailable");

type ApiResult<T> = std::result::Result<T, Failure>;

impl From<Error> for Failure {
    fn from(_: Error) -> Self {
        STORAGE_ERROR
    }
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
}

#[derive(Serialize)]
struct Page {
    documents: Vec<Entry>,
    next_cursor: Option<String>,
}

pub(crate) fn handles(path: &str) -> bool {
    path == ROOT || path.strip_prefix(ROOT).is_some_and(|rest| rest.starts_with('/'))
}

pub(crate) async fn respond(mut request: Request, env: &Env) -> Result<Response> {
    let start = Date::now().as_millis();
    let (mut response, outcome) = match dispatch(&mut request, env).await {
        Ok(response) => (response, "ok"),
        Err(Failure(status, code)) => (Response::from_json(&ErrorBody { code })?.with_status(status), code),
    };
    // Edge compression can weaken a revision ETag that the next write needs for If-Match.
    response.headers_mut().set("Cache-Control", "private, no-store, no-transform")?;
    response.headers_mut().set("X-Content-Type-Options", "nosniff")?;
    console_log!("cloud_documents {} {} {}ms", response.status_code(), outcome, Date::now().as_millis().saturating_sub(start));
    Ok(response)
}

async fn dispatch(request: &mut Request, env: &Env) -> ApiResult<Response> {
    // Only the Sites dispatcher supplies the verified owner to this Worker.
    let owner = request
        .headers()
        .get("oai-authenticated-user-id")?
        .filter(|owner| !owner.trim().is_empty())
        .ok_or(Failure(401, "authentication_required"))?;
    let path = request.path();
    let id = if path == ROOT {
        None
    } else {
        Some(path.strip_prefix(ROOT).and_then(|tail| tail.strip_prefix('/')).filter(|id| opaque_id(id)).ok_or(GONE)?)
    };
    let method = request.inner().method();
    let allowed = match id {
        None => "GET, POST",
        Some(_) => "GET, PUT, DELETE",
    };
    if !allowed.split(", ").any(|candidate| method == candidate) {
        let mut response = Response::from_json(&ErrorBody { code: "method_not_allowed" })?.with_status(405);
        response.headers_mut().set("Allow", allowed)?;
        return Ok(response);
    }
    let url = request.url()?;
    if method != "GET" && request.headers().get("Origin")?.as_deref() != Some(url.origin().ascii_serialization().as_str()) {
        return Err(Failure(403, "invalid_origin"));
    }
    let api = Api { store: Store { db: env.d1("DB")?, owner }, bucket: env.bucket("BUCKET")? };
    match (method.as_str(), id) {
        ("GET", None) => api.list(url.query()).await,
        ("GET", Some(id)) => api.read(id).await,
        ("POST", None) | ("PUT", Some(_)) => api.save(request, id).await,
        ("DELETE", Some(id)) => api.delete(id, revision(request.headers().get("If-Match")?.as_deref())?).await,
        _ => Err(Failure(405, "method_not_allowed")),
    }
}

enum Reconcile {
    Expired,
    Completed,
    ConditionalRejection,
}

struct Api {
    store: Store,
    bucket: Bucket,
}

impl Api {
    async fn list(&self, query: Option<&str>) -> ApiResult<Response> {
        let before = query.map(parse_cursor).transpose()?;
        self.maintain().await;
        let mut documents = self.store.list(before).await?;
        let next_cursor = if documents.len() > PAGE_SIZE {
            documents.truncate(PAGE_SIZE);
            documents.last().map(|entry| format!("{}.{}", entry.saved_at, entry.id))
        } else {
            None
        };
        Ok(Response::from_json(&Page { documents, next_cursor })?)
    }

    async fn read(&self, id: &str) -> ApiResult<Response> {
        let document = self.store.document(id).await?.ok_or(GONE)?;
        if let Some(body) = self.open(&document).await? {
            return Ok(body);
        }

        // A concurrent commit may have retired the old object between D1 and R2.
        let current = self.store.document(id).await?.ok_or(GONE)?;
        if current.object_key == document.object_key {
            return Err(STORAGE_ERROR);
        }
        self.open(&current).await?.ok_or(STORAGE_ERROR)
    }

    async fn open(&self, document: &Document) -> ApiResult<Option<Response>> {
        let Some(object) = self.bucket.get(&document.object_key).execute().await? else { return Ok(None) };
        if object.size() != document.entry.byte_size {
            return Err(STORAGE_ERROR);
        }
        let mut response = Response::from_body(object.body().ok_or(STORAGE_ERROR)?.response_body()?)?;
        let headers = response.headers_mut();
        headers.set("Content-Type", DOCX)?;
        headers.set("Content-Disposition", &attachment(&document.entry.name))?;
        headers.set("ETag", &etag(document.entry.revision))?;
        Ok(Some(response))
    }

    async fn save(&self, request: &mut Request, id: Option<&str>) -> ApiResult<Response> {
        let input = save_request(request.headers(), id)?;
        if let Some(row) = self.store.save(&input.operation_id).await? {
            return self.replay(row, &input).await;
        }

        let reservation = self.store.reserve(&input).await;
        if let Ok(Some(row)) = reservation {
            let row = self.upload(request, row).await?;
            self.try_cleanup(Some(&row.document_id)).await;
            return save_response(&row);
        }
        if let Some(row) = self.store.save(&input.operation_id).await? {
            return self.replay(row, &input).await;
        }
        reservation?;
        if self.store.create_tombstone(&input.operation_id).await? {
            return Err(if id.is_none() { GONE } else { OPERATION_MISMATCH });
        }
        match id {
            Some(id) if self.store.document(id).await?.is_some() => Err(STALE),
            Some(_) => Err(GONE),
            None => Err(STORAGE_ERROR),
        }
    }

    async fn replay(&self, row: Save, input: &SaveRequest) -> ApiResult<Response> {
        if !row.matches(input) {
            return Err(OPERATION_MISMATCH);
        }
        if row.deleted == 0 && matches!(row.outcome, Outcome::Pending) && row.expired == 1 {
            let row = self.reconcile(&row, Reconcile::Expired).await?;
            self.try_cleanup(Some(&row.document_id)).await;
            return save_response(&row);
        }
        save_response(&row)
    }

    async fn upload(&self, request: &mut Request, row: Save) -> ApiResult<Save> {
        let Ok(body) = request.stream() else {
            return Ok(self.store.seal(&row, "invalid", true).await?);
        };
        let failure = Rc::new(Cell::new(None));
        let body = bounded_body(body, row.byte_size, failure.clone());
        let sha256 = digest(Some(&row.sha256))?;
        let put = self
            .bucket
            .put(&row.object_key, FixedLengthStream::wrap(body, row.byte_size))
            .only_if(create_only())
            .sha256(sha256.clone())
            .execute()
            .await;

        match put {
            Ok(Some(object)) if valid_candidate(&object, row.byte_size, &sha256) => Ok(self.store.publish(&row).await?),
            Ok(Some(_)) => self.reconcile(&row, Reconcile::Completed).await,
            Ok(None) => self.reconcile(&row, Reconcile::ConditionalRejection).await,
            Err(error) => {
                if let Some(failure) = failure.get() {
                    let outcome = if failure == TOO_LARGE { "too_large" } else { "invalid" };
                    return Ok(self.store.seal(&row, outcome, false).await?);
                }
                if invalid_r2_body(&error) {
                    return Ok(self.store.seal(&row, "invalid", true).await?);
                }
                // A deletion may have won while the upload was failing.
                if self.store.deleted(&row.document_id).await? == Some(1) {
                    return Err(GONE);
                }
                Err(STORAGE_ERROR)
            }
        }
    }

    async fn reconcile(&self, row: &Save, reason: Reconcile) -> ApiResult<Save> {
        let object = self.bucket.head(&row.object_key).await?;
        let sha256 = digest(Some(&row.sha256))?;
        if let Some(object) = object {
            if matches!(reason, Reconcile::ConditionalRejection) && object.size() == 0 {
                console_log!("cloud_documents upload_fenced");
            }
            if valid_candidate(&object, row.byte_size, &sha256) {
                return Ok(self.store.publish(row).await?);
            }
        }
        Ok(self.store.seal(row, "abandoned", !matches!(reason, Reconcile::Expired)).await?)
    }

    async fn delete(&self, id: &str, base: u64) -> ApiResult<Response> {
        match self.store.tombstone(id, base).await? {
            None => return Err(GONE),
            Some(0) => return Err(STALE),
            Some(_) => {}
        }
        self.try_cleanup(Some(id)).await;
        let status = match self.store.has_cleanup(id).await {
            Ok(false) => 204,
            _ => 202,
        };
        Ok(Response::empty()?.with_status(status))
    }

    async fn maintain(&self) {
        if let Ok(rows) = self.store.expired().await {
            for row in rows {
                if self.reconcile(&row, Reconcile::Expired).await.is_err() {
                    console_log!("cloud_documents reconcile_pending");
                }
            }
        }
        self.try_cleanup(None).await;
    }

    async fn try_cleanup(&self, document_id: Option<&str>) {
        let Ok(items) = self.store.cleanup(document_id).await else {
            console_log!("cloud_documents cleanup_pending");
            return;
        };
        for item in items {
            let result = if item.settled == 1 { self.purge(&item.object_key).await } else { self.fence(&item.object_key).await };
            if result.is_err() || self.store.confirm_cleanup(&item).await.is_err() {
                console_log!("cloud_documents cleanup_pending");
            }
        }
    }

    async fn purge(&self, key: &str) -> ApiResult<()> {
        self.bucket.delete(key).await?;
        match self.bucket.head(key).await? {
            None => Ok(()),
            Some(_) => Err(STORAGE_ERROR),
        }
    }

    async fn fence(&self, key: &str) -> ApiResult<()> {
        // The upload may still finish. Keep the marker for a sealed, unsettled candidate.
        // If the upload wins the conditional put race, retry on a later cleanup pass.
        let condition = match self.bucket.head(key).await? {
            Some(object) if object.size() == 0 => return Ok(()),
            Some(object) => Conditional { etag_matches: Some(object.etag()), ..Conditional::default() },
            None => create_only(),
        };
        self.bucket.put(key, Vec::new()).only_if(condition).execute().await?;
        match self.bucket.head(key).await? {
            Some(object) if object.size() == 0 => {
                console_log!("cloud_documents cleanup_marker_confirmed");
                Ok(())
            }
            _ => Err(STORAGE_ERROR),
        }
    }
}

fn parse_cursor(query: &str) -> ApiResult<(u64, &str)> {
    let (time, id) = query.strip_prefix("cursor=").and_then(|value| value.split_once('.')).ok_or(INVALID)?;
    if time.len() > 16 || !time.bytes().all(|byte| byte.is_ascii_digit()) || !opaque_id(id) {
        return Err(INVALID);
    }
    let time = time.parse::<u64>().ok().filter(|time| *time <= MAX_REVISION).ok_or(INVALID)?;
    Ok((time, id))
}

fn save_request(headers: &Headers, id: Option<&str>) -> ApiResult<SaveRequest> {
    let base_revision = match id {
        Some(_) => revision(headers.get("If-Match")?.as_deref())?,
        None => 0,
    };
    if base_revision == MAX_REVISION {
        return Err(INVALID);
    }
    let content_type = headers.get("Content-Type")?;
    if !content_type.as_deref().is_some_and(|value| value.eq_ignore_ascii_case(DOCX)) {
        return Err(Failure(415, "unsupported_media_type"));
    }
    let byte_size = size(headers.get("X-WordCraft-Size")?.as_deref(), headers.get("Content-Length")?.as_deref())?;
    let operation_id = operation_id(headers.get("Idempotency-Key")?.as_deref())?;
    let sha256 = headers.get("X-WordCraft-SHA256")?.ok_or(INVALID)?;
    digest(Some(&sha256))?;
    let name = match (id, headers.get("X-WordCraft-Name")?) {
        (None, name) => Some(filename(name.as_deref())?),
        (Some(_), None) => None,
        (Some(_), Some(_)) => return Err(INVALID),
    };
    Ok(SaveRequest { operation_id, document_id: id.map(str::to_owned), base_revision, name, byte_size, sha256: sha256.to_ascii_lowercase() })
}

fn bounded_body(body: worker::ByteStream, expected: u64, failure: Rc<Cell<Option<Failure>>>) -> impl futures_util::Stream<Item = Result<Vec<u8>>> {
    stream::try_unfold((body.fuse(), expected), move |(mut body, remaining)| {
        let failure = failure.clone();
        async move {
            let Some(chunk) = body.next().await.transpose()? else {
                return if remaining == 0 {
                    Ok(None)
                } else {
                    failure.set(Some(INVALID_BODY));
                    Err("Upload ended before declared length".into())
                };
            };
            let length = chunk.len() as u64;
            if length > remaining {
                let actual = expected - remaining + length;
                failure.set(Some(if actual > MAX_BYTES { TOO_LARGE } else { INVALID_BODY }));
                return Err("Upload exceeded declared length".into());
            }
            // Hold back the last chunk until EOF so R2 cannot commit a prefix of an oversized body.
            if length == remaining {
                while let Some(trailing) = body.next().await.transpose()? {
                    if !trailing.is_empty() {
                        let actual = expected + trailing.len() as u64;
                        failure.set(Some(if actual > MAX_BYTES { TOO_LARGE } else { INVALID_BODY }));
                        return Err("Upload exceeded declared length".into());
                    }
                }
            }
            Ok(Some((chunk, (body, remaining - length))))
        }
    })
}

fn create_only() -> Conditional {
    Conditional { etag_does_not_match: Some("*".into()), ..Conditional::default() }
}

fn valid_candidate(object: &Object, byte_size: u64, sha256: &[u8]) -> bool {
    object.size() == byte_size && object.checksum().sha256.as_deref() == Some(sha256)
}

fn invalid_r2_body(error: &Error) -> bool {
    // R2 reports the documented IncompleteBody and BadDigest codes at the end of its error message.
    matches!(error, Error::UnknownJsError { message, .. } | Error::JsError(message)
        if message.ends_with("(10013)") || message.ends_with("(10037)"))
}

fn etag(revision: u64) -> String {
    format!("\"{revision}\"")
}

fn save_response(row: &Save) -> ApiResult<Response> {
    if row.deleted == 1 {
        return Err(GONE);
    }
    match row.outcome {
        Outcome::Pending => {
            let mut response = Response::from_json(&ErrorBody { code: "operation_pending" })?.with_status(202);
            response.headers_mut().set("Retry-After", "1")?;
            Ok(response)
        }
        Outcome::Committed => {
            let receipt = row.receipt()?;
            let mut response = Response::from_json(&receipt)?.with_status(if row.base_revision == 0 { 201 } else { 200 });
            response.headers_mut().set("ETag", &etag(receipt.revision))?;
            Ok(response)
        }
        Outcome::Stale => Err(STALE),
        Outcome::Abandoned => Err(Failure(409, "operation_abandoned")),
        Outcome::Invalid => Err(INVALID_BODY),
        Outcome::TooLarge => Err(TOO_LARGE),
        Outcome::Deleted => Err(GONE),
    }
}

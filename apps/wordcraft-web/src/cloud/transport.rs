//! Fetch adapter for the private Sites editor. The browser supplies the Site session cookie.

use super::{Entry, MAX_BYTES, Page, etag, failure, revision, status_failure, valid_cursor, valid_id, valid_name};
use js_sys::{Promise, Reflect, Uint8Array};
use serde::Deserialize;
use std::{fmt::Write as _, sync::mpsc};
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    AbortSignal, Headers, ReadableStreamDefaultReader, Request, RequestCache, RequestCredentials, RequestInit, RequestMode, RequestRedirect,
    Response, ResponseType, Window,
};
use wordcraft_ui_egui::cloud::{CloudFailure, CloudFailureKind, CloudRequest, CloudResponse, CloudService};

const ROOT: &str = "/api/cloud/documents";
const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
// Fifty names with every byte escaped, plus their other fields, fit within this cap.
const METADATA_LIMIT: usize = 64 * 1024;
const RETRY_DELAYS_MS: [i32; 4] = [1_000, 2_000, 4_000, 8_000];
type CloudResult<T> = Result<T, CloudFailure>;

pub(crate) fn service(ctx: egui::Context) -> CloudService {
    CloudService {
        new_operation_id: Box::new(|| Ok(window()?.crypto().map_err(browser_error)?.random_uuid())),
        request: Box::new(move |request| {
            let (send, receive) = mpsc::channel();
            let ctx = ctx.clone();
            spawn_local(async move {
                let result = execute(request).await;
                let _ = send.send(result);
                ctx.request_repaint();
            });
            receive
        }),
    }
}

async fn execute(request: CloudRequest) -> CloudResult<CloudResponse> {
    let window = window()?;
    let headers = Headers::new().map_err(browser_error)?;
    match request {
        CloudRequest::List { cursor } => {
            let mut url = ROOT.to_owned();
            if let Some(cursor) = cursor {
                if !valid_cursor(&cursor) {
                    return Err(invalid_response());
                }
                url = format!("{ROOT}?cursor={cursor}");
            }
            let response = fetch(&window, "GET", &url, &headers, None).await?;
            if response.status() != 200 {
                return Err(response_failure(response).await);
            }
            let (documents, next_cursor) = json::<Page>(&response).await?.validated().ok_or_else(invalid_response)?;
            Ok(CloudResponse::List { documents, next_cursor })
        }
        CloudRequest::Open { id } => {
            let response = fetch(&window, "GET", &document_url(&id)?, &headers, None).await?;
            if response.status() != 200 {
                return Err(response_failure(response).await);
            }
            let revision = response_revision(&response)?;
            if response.headers().get("Content-Type").map_err(browser_error)?.as_deref() != Some(DOCX) {
                return Err(invalid_response());
            }
            let bytes = bounded_body(&response, MAX_BYTES).await?;
            if bytes.is_empty() {
                return Err(invalid_response());
            }
            Ok(CloudResponse::Open { revision, bytes })
        }
        CloudRequest::Delete { id, revision } => {
            set_header(&headers, "If-Match", &etag(revision).ok_or_else(invalid_response)?)?;
            let response = fetch(&window, "DELETE", &document_url(&id)?, &headers, None).await?;
            match response.status() {
                204 | 202 => Ok(CloudResponse::Delete { cleanup_pending: response.status() == 202 }),
                _ => Err(response_failure(response).await),
            }
        }
        CloudRequest::Save { operation_id, id, base_revision, name, bytes } => {
            if !(1..=MAX_BYTES).contains(&bytes.len()) {
                return Err(failure(CloudFailureKind::Invalid, "Cloud documents must contain between 1 byte and 16 MiB."));
            }
            let (method, url, expected_status) = match (&id, base_revision) {
                (Some(id), Some(base)) => {
                    set_header(&headers, "If-Match", &etag(base).ok_or_else(invalid_response)?)?;
                    ("PUT", document_url(id)?, 200)
                }
                (None, None) if valid_name(&name) => {
                    let encoded = js_sys::encode_uri_component(&name);
                    set_header(&headers, "X-WordCraft-Name", &String::from(encoded))?;
                    ("POST", ROOT.to_owned(), 201)
                }
                _ => return Err(failure(CloudFailureKind::Invalid, "The cloud save has an invalid name or revision.")),
            };
            let body = Uint8Array::from(bytes.as_ref());
            set_header(&headers, "Content-Type", DOCX)?;
            set_header(&headers, "Idempotency-Key", &operation_id)?;
            set_header(&headers, "X-WordCraft-Size", &bytes.len().to_string())?;
            set_header(&headers, "X-WordCraft-SHA256", &sha256(&window, &body).await?)?;

            // Every automatic retry sends the same immutable body, operation ID and precondition.
            let mut delays = RETRY_DELAYS_MS.into_iter();
            loop {
                let result = match fetch(&window, method, &url, &headers, Some(&body)).await {
                    Ok(response) if response.status() == expected_status => {
                        let receipt = save_receipt(&response, id.as_deref(), base_revision, &name, bytes.len()).await;
                        return receipt.map_err(|_| invalid_receipt());
                    }
                    Ok(response) => response_failure(response).await,
                    Err(error) => error,
                };
                if result.kind != CloudFailureKind::Retryable {
                    return Err(result);
                }
                let Some(delay) = delays.next() else { return Err(result) };
                wait(&window, delay).await?;
            }
        }
    }
}

async fn save_receipt(response: &Response, id: Option<&str>, base: Option<u64>, name: &str, bytes: usize) -> CloudResult<CloudResponse> {
    let revision = response_revision(response)?;
    let entry = json::<Entry>(response).await?.validated().ok_or_else(invalid_response)?;
    if revision != entry.revision
        || revision != base.unwrap_or(0) + 1
        || entry.byte_size != bytes as u64
        || match id {
            Some(id) => entry.id != id,
            None => entry.name != name,
        }
    {
        return Err(invalid_receipt());
    }
    Ok(CloudResponse::Save(entry))
}

fn window() -> CloudResult<Window> {
    web_sys::window().ok_or_else(|| browser_error(JsValue::UNDEFINED))
}

fn document_url(id: &str) -> CloudResult<String> {
    valid_id(id).then(|| format!("{ROOT}/{id}")).ok_or_else(invalid_response)
}

fn set_header(headers: &Headers, key: &str, value: &str) -> CloudResult<()> {
    headers.set(key, value).map_err(browser_error)
}

async fn fetch(window: &Window, method: &str, url: &str, headers: &Headers, body: Option<&Uint8Array>) -> CloudResult<Response> {
    let init = RequestInit::new();
    init.set_method(method);
    init.set_headers_headers(headers);
    init.set_body_opt_u8_array(body);
    init.set_mode(RequestMode::SameOrigin);
    init.set_credentials(RequestCredentials::SameOrigin);
    init.set_cache(RequestCache::NoStore);
    // An expired Sites session can redirect to sign-in. Never follow a redirect with a document body.
    init.set_redirect(RequestRedirect::Manual);
    init.set_signal(Some(&AbortSignal::timeout_with_u32(30_000)));
    let request = Request::new_with_str_and_init(url, &init).map_err(browser_error)?;
    let response = JsFuture::from(window.fetch_with_request(&request)).await.map_err(browser_error)?.dyn_into::<Response>().map_err(browser_error)?;
    if response.type_() == ResponseType::Opaqueredirect || (300..400).contains(&response.status()) {
        return Err(status_failure(401, None));
    }
    Ok(response)
}

async fn wait(window: &Window, milliseconds: i32) -> CloudResult<()> {
    let promise = Promise::new(&mut |resolve, reject| {
        if let Err(error) = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, milliseconds) {
            let _ = reject.call1(&JsValue::UNDEFINED, &error);
        }
    });
    JsFuture::from(promise).await.map(|_| ()).map_err(browser_error)
}

async fn sha256(window: &Window, bytes: &Uint8Array) -> CloudResult<String> {
    let crypto = window.crypto().map_err(browser_error)?.subtle();
    let hash = JsFuture::from(crypto.digest_with_str_and_buffer_source("SHA-256", bytes).map_err(browser_error)?).await.map_err(browser_error)?;
    let mut hex = String::with_capacity(64);
    for byte in Uint8Array::new(&hash).to_vec() {
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

fn response_revision(response: &Response) -> CloudResult<u64> {
    response.headers().get("ETag").map_err(browser_error)?.as_deref().and_then(revision).ok_or_else(invalid_response)
}

async fn json<T: serde::de::DeserializeOwned>(response: &Response) -> CloudResult<T> {
    let bytes = bounded_body(response, METADATA_LIMIT).await?;
    serde_json::from_slice(&bytes).map_err(|_| invalid_response())
}

async fn bounded_body(response: &Response, limit: usize) -> CloudResult<Vec<u8>> {
    let body = response.body().ok_or_else(invalid_response)?;
    if response
        .headers()
        .get("Content-Length")
        .map_err(browser_error)?
        .is_some_and(|value| !value.parse::<usize>().is_ok_and(|length| length <= limit))
    {
        let _ = JsFuture::from(body.cancel()).await;
        return Err(invalid_response());
    }
    let reader = ReadableStreamDefaultReader::new(&body).map_err(browser_error)?;
    let mut bytes = Vec::new();
    loop {
        let chunk = JsFuture::from(reader.read()).await.map_err(browser_error)?;
        let done = Reflect::get(&chunk, &"done".into()).map_err(browser_error)?;
        if done.as_bool() == Some(true) {
            return Ok(bytes);
        }
        let data = Reflect::get(&chunk, &"value".into()).map_err(browser_error)?.dyn_into::<Uint8Array>().map_err(browser_error)?;
        let end = bytes.len().saturating_add(data.length() as usize);
        if end > limit {
            let _ = JsFuture::from(reader.cancel()).await;
            return Err(invalid_response());
        }
        let start = bytes.len();
        bytes.resize(end, 0);
        if let Some(target) = bytes.get_mut(start..) {
            data.copy_to(target);
        }
    }
}

#[derive(Deserialize)]
struct ErrorBody {
    code: String,
}

async fn response_failure(response: Response) -> CloudFailure {
    let status = response.status();
    let code = if matches!(status, 403 | 409) {
        json::<ErrorBody>(&response).await.ok().map(|body| body.code)
    } else {
        if let Some(body) = response.body() {
            let _ = JsFuture::from(body.cancel()).await;
        }
        None
    };
    if (200..300).contains(&status) {
        return invalid_receipt();
    }
    status_failure(status, code.as_deref())
}

fn browser_error(_: JsValue) -> CloudFailure {
    failure(CloudFailureKind::Retryable, "The browser could not confirm the cloud operation. Try again.")
}

fn invalid_response() -> CloudFailure {
    failure(CloudFailureKind::Invalid, "The Site returned invalid cloud data.")
}

fn invalid_receipt() -> CloudFailure {
    failure(CloudFailureKind::Retryable, "The Site could not confirm the cloud operation. Try again.")
}

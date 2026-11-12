//! Same-origin transport for the native loopback Assistant. The static build does not enable it.

use std::sync::mpsc;
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{AbortSignal, RequestCache, RequestCredentials, RequestInit, RequestMode, RequestRedirect, Response};
use wordcraft_ui_egui::local_assistant::{Reply, Request, Service};

const UNAVAILABLE: &str = "The local Assistant could not be reached. Check that the local server is running.";
const INVALID: &str = "The local Assistant returned an unexpected response.";
const AUTHORIZE_PREFIX: &str = "https://auth.openai.com/api/accounts/authorize?";

const KEY_NAME: &str = "wordcraft-local-assistant";
const KEY_FRAGMENT: &str = "#wordcraft-local-assistant=";

pub(super) fn launch_key(document: &web_sys::Document) -> Option<String> {
    let location = document.location()?;
    if !location.protocol().is_ok_and(|scheme| matches!(scheme.as_str(), "http:" | "https:"))
        || !location.hostname().is_ok_and(|host| matches!(host.as_str(), "127.0.0.1" | "[::1]" | "localhost"))
        || document.query_selector("meta[name=wordcraft-local-assistant][content=enabled]").ok().flatten().is_none()
    {
        return None;
    }
    let window = web_sys::window()?;
    let storage = window.session_storage().ok().flatten();
    let fragment = location.hash().ok()?;
    if let Some(key) = fragment.strip_prefix(KEY_FRAGMENT).filter(|key| valid_key(key)) {
        let href = location.href().ok()?;
        let (without_key, _) = href.split_once('#')?;
        // Replace the current history entry before enabling any Assistant requests.
        window.history().ok()?.replace_state_with_url(&JsValue::NULL, "", Some(without_key)).ok()?;
        if let Some(storage) = storage {
            let _ = storage.set_item(KEY_NAME, key);
        }
        return Some(key.to_owned());
    }
    storage?.get_item(KEY_NAME).ok().flatten().filter(|key| valid_key(key))
}

fn valid_key(key: &str) -> bool {
    key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn service(ctx: egui::Context, key: String) -> Service {
    Box::new(move |request| {
        let ctx = ctx.clone();
        let key = key.clone();
        let (send, recv) = mpsc::channel();
        wasm_bindgen_futures::spawn_local(async move {
            let _ = send.send(fetch(request, &key).await);
            ctx.request_repaint();
        });
        recv
    })
}

async fn fetch(action: Request, key: &str) -> Result<Reply, String> {
    let (method, path) = match &action {
        Request::Status => ("GET", "/api/assistant/local/status"),
        Request::Connect => ("POST", "/api/assistant/local/connect"),
        Request::Disconnect => ("POST", "/api/assistant/local/disconnect"),
        Request::Rewrite(_) => ("POST", "/api/assistant/local/rewrite"),
    };
    let init = RequestInit::new();
    init.set_method(method);
    init.set_mode(RequestMode::SameOrigin);
    init.set_credentials(RequestCredentials::SameOrigin);
    init.set_cache(RequestCache::NoStore);
    init.set_redirect(RequestRedirect::Error);
    // A rewrite can wait three minutes for the provider; sign-out may wait for sign-in to stop.
    let timeout = match &action {
        Request::Status => 5_000,
        Request::Connect => 60_000,
        Request::Disconnect => 140_000,
        Request::Rewrite(_) => 200_000,
    };
    init.set_signal(Some(&AbortSignal::timeout_with_u32(timeout)));
    if let Request::Rewrite(text) = &action {
        init.set_body(&JsValue::from_str(&serde_json::json!({"text": text}).to_string()));
    }
    let request = web_sys::Request::new_with_str_and_init(path, &init).map_err(|_| UNAVAILABLE)?;
    request.headers().set("Authorization", &format!("Bearer {key}")).map_err(|_| UNAVAILABLE)?;
    if method == "POST" {
        request.headers().set("Content-Type", "application/json").map_err(|_| UNAVAILABLE)?;
    }
    let window = web_sys::window().ok_or(UNAVAILABLE)?;
    let response = JsFuture::from(window.fetch_with_request(&request)).await.map_err(|_| UNAVAILABLE)?.dyn_into::<Response>().map_err(|_| INVALID)?;
    let body = JsFuture::from(response.text().map_err(|_| INVALID)?).await.map_err(|_| INVALID)?.as_string().ok_or(INVALID)?;
    let body: serde_json::Value = serde_json::from_str(&body).map_err(|_| INVALID)?;
    if !response.ok() {
        return Err(body["error"].as_str().filter(|message| !message.is_empty() && message.len() <= 300).unwrap_or(INVALID).into());
    }
    match action {
        Request::Status => {
            Ok(Reply::Status { connected: body["connected"].as_bool().ok_or(INVALID)?, connecting: body["connecting"].as_bool().ok_or(INVALID)? })
        }
        Request::Connect => {
            Ok(Reply::AuthorizationUrl(body["authorization_url"].as_str().filter(|url| url.starts_with(AUTHORIZE_PREFIX)).ok_or(INVALID)?.into()))
        }
        Request::Disconnect if body["connected"].as_bool() == Some(false) => Ok(Reply::Disconnected),
        Request::Rewrite(_) => Ok(Reply::Replacement(body["replacement"].as_str().ok_or(INVALID)?.into())),
        Request::Disconnect => Err(INVALID.into()),
    }
}

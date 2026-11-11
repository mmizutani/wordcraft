#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(all(feature = "probe", any(test, target_arch = "wasm32")))]
fn probe_id<'a>(identity: Option<&str>, query: Option<&'a str>) -> Result<&'a str, u16> {
    if identity.is_none_or(|owner| owner.trim().is_empty()) {
        return Err(401);
    }
    let id = query.and_then(|query| query.strip_prefix("id=")).ok_or(400_u16)?;
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')) {
        return Err(400);
    }
    Ok(id)
}

#[cfg(all(test, feature = "probe"))]
mod tests {
    use super::probe_id;

    #[test]
    fn requires_identity_before_validating_id() {
        assert_eq!(probe_id(None, Some("id=trial")), Err(401));
        assert_eq!(probe_id(Some(" "), None), Err(401));
    }

    #[test]
    fn accepts_only_one_bounded_opaque_id() {
        let owner = Some("authenticated");
        assert_eq!(probe_id(owner, Some("id=Run_1-a")), Ok("Run_1-a"));
        assert_eq!(probe_id(owner, Some(&format!("id={}", "a".repeat(64)))), Ok("a".repeat(64).as_str()));
        assert_eq!(probe_id(owner, Some(&format!("id={}", "a".repeat(65)))), Err(400));
        for query in [None, Some("id="), Some("id=x&id=y"), Some("id=.."), Some("id=%78"), Some("id=あ")] {
            assert_eq!(probe_id(owner, query), Err(400));
        }
    }
}

#[cfg(all(not(feature = "probe"), any(test, target_arch = "wasm32")))]
mod cloud;

#[cfg(all(feature = "probe", target_arch = "wasm32"))]
mod probe;

#[cfg(target_arch = "wasm32")]
use worker::{Context, EncodeBody, Env, Fetcher, Request, Response, Result, event};

#[cfg(target_arch = "wasm32")]
#[event(fetch)]
async fn fetch(request: Request, env: Env, _context: Context) -> Result<Response> {
    #[cfg(feature = "probe")]
    if request.path() == "/__wordcraft_probe" {
        return probe::respond(&request, &env).await;
    }

    #[cfg(not(feature = "probe"))]
    if cloud::handles(&request.path()) {
        return cloud::respond(request, &env).await;
    }

    match env.service("ASSETS") {
        Ok(assets) => match fetch_asset(&assets, request).await {
            Ok(response) => Ok(response),
            Err(_) => Response::error("Static asset fetch failed", 502),
        },
        Err(_) => Response::error("Static assets unavailable", 404),
    }
}

#[cfg(target_arch = "wasm32")]
async fn fetch_asset(assets: &Fetcher, request: Request) -> Result<Response> {
    let path = request.path();
    if !is_hashed_wasm(&request.inner().method(), &path) {
        return assets.fetch_request(request).await;
    }

    let method = request.method();
    let mut url = request.url()?;
    let response = assets.fetch_request(request).await?;
    if response.status_code() != 404 {
        return Ok(response);
    }

    url.set_path(&format!("{path}.gz"));
    let compressed = assets.fetch_request(Request::new(url.as_str(), method)?).await?;
    if compressed.status_code() != 200 {
        return Ok(response);
    }

    let headers = compressed.headers().clone();
    headers.set("Content-Type", "application/wasm")?;
    headers.set("Content-Encoding", "gzip")?;
    Ok(compressed.with_headers(headers).with_encode_body(EncodeBody::Manual))
}

#[cfg(any(test, target_arch = "wasm32"))]
fn is_hashed_wasm(method: &str, path: &str) -> bool {
    matches!(method, "GET" | "HEAD")
        && path
            .strip_prefix("/wordcraft-web-")
            .and_then(|name| name.strip_suffix("_bg.wasm"))
            .is_some_and(|hash| !hash.is_empty() && hash.len() <= 16 && hash.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')))
}

#[cfg(test)]
#[test]
fn compressed_fallback_only_matches_trunk_wasm_get_and_head() {
    const PATH: &str = "/wordcraft-web-1508560fa0bdccdc_bg.wasm";
    assert!(is_hashed_wasm("GET", PATH));
    assert!(is_hashed_wasm("HEAD", PATH));
    assert!(is_hashed_wasm("GET", "/wordcraft-web-1508560fa0bdccd_bg.wasm"));
    assert!(!is_hashed_wasm("POST", PATH));
    for path in [
        "/nested/wordcraft-web-1508560fa0bdccdc_bg.wasm",
        "/wordcraft-web-1508560fa0bdccdg_bg.wasm",
        "/wordcraft-web-_bg.wasm",
        "/wordcraft-web-1508560fa0bdccdca_bg.wasm",
        "/wordcraft-web-1508560fa0bdccdc_bg.wasm.gz",
        "/wordcraft-web-1508560fa0bdccdc.js",
    ] {
        assert!(!is_hashed_wasm("GET", path));
    }
}

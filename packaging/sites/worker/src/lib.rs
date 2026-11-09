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

#[cfg(all(feature = "probe", target_arch = "wasm32"))]
mod probe;

#[cfg(target_arch = "wasm32")]
use worker::{Context, Env, Request, Response, Result, event};

#[cfg(target_arch = "wasm32")]
#[event(fetch)]
async fn fetch(request: Request, env: Env, _context: Context) -> Result<Response> {
    #[cfg(feature = "probe")]
    if request.path() == "/__wordcraft_probe" {
        return probe::respond(&request, &env).await;
    }

    match env.service("ASSETS") {
        Ok(assets) => match assets.fetch_request(request).await {
            Ok(response) => Ok(response),
            Err(_) => Response::error("Static asset fetch failed", 502),
        },
        Err(_) => Response::error("Static assets unavailable", 404),
    }
}

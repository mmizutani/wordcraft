//! What the relay and the desktop send each other about one agent request.

use serde_json::{Value, json};
use wordcraft_http::{Request, Response};

/// The request headers the relay passes on to the desktop: what MCP over HTTP uses, nothing
/// about the connection (`Host`, `Cookie`, proxies' headers…).
pub const REQUEST_HEADERS: &[&str] =
    &["Authorization", "Content-Type", "Accept", "MCP-Protocol-Version", "Mcp-Method", "Mcp-Name", "Mcp-Session-Id", "Origin", "Last-Event-ID"];

/// The answer headers the relay passes back to the agent.
pub const RESPONSE_HEADERS: &[&str] = &["Content-Type", "WWW-Authenticate", "Allow", "Retry-After", "Mcp-Session-Id"];

fn passed(name: &str, list: &[&str]) -> bool {
    list.iter().any(|h| h.eq_ignore_ascii_case(name))
        || (list == REQUEST_HEADERS && name.len() > 10 && name.get(..10).is_some_and(|p| p.eq_ignore_ascii_case("Mcp-Param-")))
}

fn headers_json(headers: &[(String, String)], list: &[&str]) -> Value {
    Value::Array(headers.iter().filter(|(n, _)| passed(n, list)).map(|(n, v)| json!([n, v])).collect())
}

fn headers_from(v: &Value, list: &[&str]) -> Vec<(String, String)> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|h| Some((h.get(0)?.as_str()?.to_string(), h.get(1)?.as_str()?.to_string())))
                .filter(|(n, _)| passed(n, list))
                .take(32)
                .collect()
        })
        .unwrap_or_default()
}

/// An agent's request as the relay hands it to the desktop: `target` is what follows the room's
/// part of the path (`/mcp`).
pub fn forward(id: u64, req: &Request, target: &str, body: &str) -> Value {
    json!({"id": id, "method": req.method, "target": target, "headers": headers_json(&req.headers, REQUEST_HEADERS), "body": body})
}

/// A forwarded request back as the request the desktop's endpoint answers, and its id.
pub fn request_from(v: &Value) -> Option<(u64, Request)> {
    let id = v.get("id")?.as_u64()?;
    let method = v.get("method")?.as_str()?.to_string();
    let target = v.get("target")?.as_str()?.to_string();
    let body = v.get("body").and_then(Value::as_str).unwrap_or_default().as_bytes().to_vec();
    Some((id, Request { method, target, headers: headers_from(v.get("headers").unwrap_or(&Value::Null), REQUEST_HEADERS), body, peer: None }))
}

/// How long the agent still waits for its answer, from when the relay handed the request on
/// (`ttlMs`). None from a relay that doesn't say.
pub fn ttl(v: &Value) -> Option<std::time::Duration> {
    v.get("ttlMs").and_then(Value::as_u64).map(std::time::Duration::from_millis)
}

/// How long a poll waited at the relay for requests (`waitedMs`). None from a relay that
/// doesn't say.
pub fn waited(v: &Value) -> Option<std::time::Duration> {
    v.get("waitedMs").and_then(Value::as_u64).map(std::time::Duration::from_millis)
}

/// The desktop's answer to request `id`. A body that isn't UTF-8 (MCP's are JSON) is refused
/// rather than mangled.
pub fn reply(id: u64, resp: &Response) -> Value {
    match std::str::from_utf8(&resp.body) {
        Ok(body) => json!({"id": id, "status": resp.status, "headers": headers_json(&resp.headers, RESPONSE_HEADERS), "body": body}),
        Err(_) => json!({"id": id, "status": 502, "headers": [["Content-Type", "text/plain; charset=utf-8"]], "body": "the answer wasn't text\n"}),
    }
}

/// The desktop's answer back as a response for the agent, and the request it answers.
pub fn response_from(v: &Value) -> Option<(u64, Response)> {
    let id = v.get("id")?.as_u64()?;
    let status = v.get("status")?.as_u64().and_then(|s| u16::try_from(s).ok()).filter(|s| (200..600).contains(s))?;
    let body = v.get("body").and_then(Value::as_str).unwrap_or_default().as_bytes().to_vec();
    Some((id, Response { status, headers: headers_from(v.get("headers").unwrap_or(&Value::Null), RESPONSE_HEADERS), body, ..Response::new(status) }))
}

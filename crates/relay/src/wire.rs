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

/// The longest header value passed on: longer ones are dropped (the headers passed are short).
pub const MAX_HEADER_VALUE: usize = 8 << 10;

/// The headers kept from `pairs`: those `list` passes, with values of a sensible length.
fn headers_kept(pairs: Vec<(String, String)>, list: &[&str]) -> Vec<(String, String)> {
    pairs.into_iter().filter(|(n, v)| v.len() <= MAX_HEADER_VALUE && passed(n, list)).take(32).collect()
}

/// An agent's request as the relay hands it to the desktop: `target` is what follows the room's
/// part of the path (`/mcp`).
pub fn forward(id: u64, req: &Request, target: &str, body: &str) -> Value {
    json!({"id": id, "method": req.method, "target": target, "headers": headers_json(&req.headers, REQUEST_HEADERS), "body": body})
}

/// One forwarded request as the desktop takes it from a poll's answer: its id, the request, and
/// how long its agent still waits from when the relay handed it on (`ttlMs`; None from a relay
/// that doesn't say).
pub type Forwarded = (u64, Request, Option<std::time::Duration>);

/// A poll's answer: its requests, and how long the poll waited at the relay (`waitedMs`; None
/// from a relay that doesn't say). Read as exactly those fields, never as a whole JSON tree, so a
/// relay can't make the desktop hold a tree many times the size of its answer.
pub fn polled_from(bytes: &[u8]) -> Option<(Vec<Forwarded>, Option<std::time::Duration>)> {
    let p: Polled = serde_json::from_slice(bytes).ok()?;
    let requests = p
        .requests
        .0
        .into_iter()
        .map(|f| {
            let req = Request {
                method: f.method,
                target: f.target,
                headers: headers_kept(f.headers.0, REQUEST_HEADERS),
                body: f.body.into_bytes(),
                peer: None,
            };
            (f.id, req, f.ttl_ms.map(std::time::Duration::from_millis))
        })
        .collect();
    Some((requests, p.waited_ms.map(std::time::Duration::from_millis)))
}

#[derive(serde::Deserialize)]
struct Polled {
    #[serde(default)]
    requests: Few<ForwardedRequest, MAX_POLLED>,
    #[serde(rename = "waitedMs")]
    waited_ms: Option<u64>,
}

#[derive(serde::Deserialize)]
struct ForwardedRequest {
    id: u64,
    method: String,
    target: String,
    #[serde(default)]
    headers: Few<(String, String), MAX_HEADERS>,
    #[serde(default)]
    body: String,
    #[serde(rename = "ttlMs")]
    ttl_ms: Option<u64>,
}

/// The most requests a poll's answer may carry (a relay sends at most 16).
const MAX_POLLED: usize = 64;

/// The desktop's answer to request `id`. A body that isn't UTF-8 (MCP's are JSON) is refused
/// rather than mangled.
pub fn reply(id: u64, resp: &Response) -> Value {
    match std::str::from_utf8(&resp.body) {
        Ok(body) => json!({"id": id, "status": resp.status, "headers": headers_json(&resp.headers, RESPONSE_HEADERS), "body": body}),
        Err(_) => json!({"id": id, "status": 502, "headers": [["Content-Type", "text/plain; charset=utf-8"]], "body": "the answer wasn't text\n"}),
    }
}

/// [`reply`] as the bytes sent to the relay, which takes up to `max` of them. The body is written
/// from where it is, not copied first, into a buffer that never grows past `max`. An answer
/// larger once written (JSON escapes each `"` and `\` in its body, so it can grow to twice its
/// size) becomes a short error its agent can act on, not an upload the relay refuses while the
/// agent waits until it times out.
pub fn reply_within(id: u64, resp: &Response, max: usize) -> Vec<u8> {
    let Ok(body) = std::str::from_utf8(&resp.body) else {
        return reply(id, resp).to_string().into_bytes();
    };
    let mut out = Capped { buf: Vec::new(), max };
    let written = (|| -> Result<(), std::io::Error> {
        use std::io::Write;
        write!(out, "{{\"id\":{id},\"status\":{},\"headers\":", resp.status)?;
        serde_json::to_writer(&mut out, &headers_json(&resp.headers, RESPONSE_HEADERS))?;
        out.write_all(b",\"body\":")?;
        serde_json::to_writer(&mut out, body)?;
        out.write_all(b"}")
    })();
    if written.is_ok() {
        return out.buf;
    }
    drop(out);
    // How large it would have been, counted without keeping it.
    let mut counted = Counted(0);
    let _ = serde_json::to_writer(&mut counted, body);
    let mib = |n: usize| n.div_ceil(1 << 20);
    let why = format!("the answer is too large to pass through the relay: {} MiB once written, over {} MiB", mib(counted.0), mib(max));
    // The shape of the relay's own refusals.
    let body = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32000, "message": why}}).to_string();
    json!({"id": id, "status": 502, "headers": [["Content-Type", "application/json"]], "body": body}).to_string().into_bytes()
}

/// A buffer that refuses to grow past `max` bytes, and whose capacity never passes it either.
#[doc(hidden)]
pub struct Capped {
    pub buf: Vec<u8>,
    pub max: usize,
}

impl std::io::Write for Capped {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let len = self.buf.len().checked_add(bytes.len()).filter(|n| *n <= self.max).ok_or_else(|| std::io::Error::other("past the limit"))?;
        if len > self.buf.capacity() {
            let want = self.buf.capacity().saturating_mul(2).max(len).min(self.max);
            self.buf.reserve_exact(want.saturating_sub(self.buf.len()));
        }
        self.buf.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Counted(usize);

impl std::io::Write for Counted {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The bytes a response takes as written: its headers as well as its body.
pub fn size(resp: &Response) -> usize {
    resp.headers.iter().fold(resp.body.len(), |n, (k, v)| n.saturating_add(k.len()).saturating_add(v.len()).saturating_add(4))
}

/// The desktop's answer back as a response for the agent, and the request it answers. Read as
/// exactly the fields [`reply`] writes, never as a whole JSON tree: an answer of up to 96 MiB
/// with a field of millions of tiny values would take many times that as a tree. A field the
/// desktop doesn't send, or more headers than any MCP answer has, is refused unread.
pub fn response_from(bytes: &[u8]) -> Option<(u64, Response)> {
    let a: Answer = serde_json::from_slice(bytes).ok()?;
    let status = u16::try_from(a.status).ok().filter(|s| (200..600).contains(s))?;
    Some((a.id, Response { status, headers: headers_kept(a.headers.0, RESPONSE_HEADERS), body: a.body.into_bytes(), ..Response::new(status) }))
}

/// The most headers a request or an answer may carry; MCP's have a handful.
const MAX_HEADERS: usize = 64;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Answer {
    id: u64,
    status: u64,
    #[serde(default)]
    headers: Few<(String, String), MAX_HEADERS>,
    #[serde(default)]
    body: String,
}

/// A JSON array of at most `MAX` items, refused past that as it is read.
struct Few<T, const MAX: usize>(Vec<T>);

impl<T, const MAX: usize> Default for Few<T, MAX> {
    fn default() -> Self {
        Few(Vec::new())
    }
}

impl<'de, T: serde::Deserialize<'de>, const MAX: usize> serde::Deserialize<'de> for Few<T, MAX> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Seq<T, const MAX: usize>(std::marker::PhantomData<T>);
        impl<'de, T: serde::Deserialize<'de>, const MAX: usize> serde::de::Visitor<'de> for Seq<T, MAX> {
            type Value = Few<T, MAX>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "an array of at most {MAX} items")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Few<T, MAX>, A::Error> {
                let mut out = Vec::new();
                while let Some(item) = seq.next_element::<T>()? {
                    if out.len() == MAX {
                        return Err(serde::de::Error::custom("too many items"));
                    }
                    out.push(item);
                }
                Ok(Few(out))
            }
        }
        d.deserialize_seq(Seq::<T, MAX>(std::marker::PhantomData))
    }
}

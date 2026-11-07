//! MCP over Streamable HTTP (#75): `wordcraft-cli mcp --http 127.0.0.1:PORT --connect <room port>`
//! and, behind the relay (#76), the same endpoint at the relay's URL.
//!
//! Every request carries an invite's key in `Authorization: Bearer`. The key is the only identity:
//! there are no MCP sessions (2026-07-28 has none, and they are optional before it), and each key
//! keeps one connection to the app, so one hosted copy of the shared document, on a thread of its
//! own ([`Endpoint`]). The window's key is refused: it acts as the person, and nothing that leaves
//! this computer may hold it.
//!
//! Both eras are served on one endpoint. A request whose `_meta` names its protocol version is a
//! 2026-07-28 one: its `MCP-Protocol-Version`, `Mcp-Method` and `Mcp-Name` headers must match
//! its body, and errors take their HTTP statuses (400 for a version or a header that doesn't
//! match, 404 for an unknown method). Anything else is a 2025-06-18 (or older) request, starting
//! with `initialize`. Replies are `application/json`; nothing streams, so `GET` and `DELETE` are
//! 405.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wordcraft_http::{Request, Response, bearer};

use crate::backend::Backend;
use crate::server::{ALL_VERSIONS, MAX_REQUEST_LINE, META_VERSION, Server, UNSUPPORTED_VERSION, stateless_version};

/// A 2026-07-28 request whose headers don't say what its body does.
pub const HEADER_MISMATCH: i64 = -32020;
const METHOD_NOT_FOUND: i64 = -32601;

/// Opens the backend for an invite's key: a connection to the app with that key.
pub type Opener = Box<dyn Fn(&str) -> Result<Box<dyn Backend + Send>, String> + Send + Sync>;

/// How the endpoint checks requests.
pub struct Options {
    /// The `Host` values accepted, compared without case. `None` accepts any: behind the relay,
    /// which is reached by its own name, a page in a browser can't rebind to this computer.
    pub hosts: Option<Vec<String>>,
    /// The `Origin` values accepted. A request with any other `Origin` (a web page) is refused;
    /// MCP clients send none.
    pub origins: Vec<String>,
    /// Keys served at once.
    pub max_keys: usize,
    /// A key's connection to the app closes after this long without a request.
    pub idle: Duration,
    /// How long one request may take.
    pub timeout: Duration,
    /// Answers' bytes waiting to be sent, in all: more are refused (503) until clients have read
    /// some, so slow readers can't make the endpoint hold copy after copy of a large document.
    pub max_sending: usize,
    /// How often a key's connection asks the app again whether it still takes the key. Some
    /// requests (tools/list, server/discover) are answered without the app, so a key taken back
    /// would otherwise keep its connection, and its place, for as long as it kept asking.
    pub recheck: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            hosts: Some(Vec::new()),
            origins: Vec::new(),
            max_keys: 64,
            idle: Duration::from_secs(30 * 60),
            timeout: Duration::from_secs(150),
            recheck: Duration::from_secs(15),
            max_sending: 256 << 20,
        }
    }
}

/// What a key's thread answers one request with.
struct Answer {
    reply: Option<String>,
    /// The app refused the key: the thread has ended.
    ended: bool,
}

/// Requests one key may have waiting for its connection; more are answered 503 at once, so a
/// client sending faster than the app answers can't pile up request bodies.
const QUEUE: usize = 8;

struct Job {
    body: String,
    answer: Sender<Answer>,
    /// When the request's caller stops waiting: a job not begun by then is dropped, never run
    /// after its caller was told it timed out.
    until: Instant,
}

struct Seat {
    jobs: SyncSender<Job>,
    last: Instant,
}

/// The MCP endpoint: answers one HTTP request at a time from any thread ([`Endpoint::handle`]).
pub struct Endpoint {
    options: Options,
    open: Arc<Opener>,
    seats: Mutex<HashMap<String, Seat>>,
    /// Keys being checked with the app right now (each may wait for the app's slowdown of wrong
    /// keys); more than a few at once are refused rather than queued.
    opening: AtomicUsize,
    /// Answers' bytes waiting to be sent.
    sending: Arc<AtomicUsize>,
}

/// An answer's share of [`Options::max_sending`], given back once it has been sent.
struct Sending {
    counter: Arc<AtomicUsize>,
    bytes: usize,
}

impl Drop for Sending {
    fn drop(&mut self) {
        self.counter.fetch_sub(self.bytes, Ordering::SeqCst);
    }
}

/// A JSON-RPC error with no id, as the body of a refused request.
fn refusal(status: u16, code: i64, message: &str) -> Response {
    Response::json(status, json!({"jsonrpc": "2.0", "id": null, "error": {"code": code, "message": message}}).to_string())
}

/// A header value, with the `=?base64?…?=` form 2026-07-28 uses for values that aren't plain
/// ASCII decoded.
fn header_text(v: &str) -> Option<String> {
    match v.strip_prefix("=?base64?").and_then(|r| r.strip_suffix("?=")) {
        Some(b64) => wordcraft_engine::cmd::insert::base64_decode(b64).and_then(|b| String::from_utf8(b).ok()),
        None => Some(v.to_string()),
    }
}

/// For a 2026-07-28 request: its headers say what its body does, or why not.
fn check_headers(req: &Request, msg: &Value) -> Result<(), String> {
    let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
    let params = msg.get("params").unwrap_or(&Value::Null);
    let want = |header: &str, body: &str| -> Result<(), String> {
        match req.header(header).and_then(header_text) {
            None => Err(format!("Header mismatch: {header} header is missing")),
            Some(v) if v == body => Ok(()),
            Some(v) => Err(format!("Header mismatch: {header} header value '{v}' does not match body value '{body}'")),
        }
    };
    want("MCP-Protocol-Version", stateless_version(params).unwrap_or_default())?;
    want("Mcp-Method", method)?;
    let name = match method {
        "tools/call" | "prompts/get" => params.get("name"),
        "resources/read" => params.get("uri"),
        _ => None,
    };
    if let Some(name) = name {
        want("Mcp-Name", name.as_str().unwrap_or_default())?;
    }
    Ok(())
}

impl Endpoint {
    pub fn new(options: Options, open: Opener) -> Self {
        Endpoint {
            options,
            open: Arc::new(open),
            seats: Mutex::new(HashMap::new()),
            opening: AtomicUsize::new(0),
            sending: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// An endpoint whose keys reach the app's room port at `connect` (File › Share's line).
    pub fn connecting(options: Options, connect: &str) -> Self {
        let addr = connect.to_string();
        Self::new(
            options,
            Box::new(move |key| {
                crate::backend::Remote::connect_with_key(&addr, key)
                    .map(|r| Box::new(r) as Box<dyn Backend + Send>)
                    .map_err(|e| format!("the WordCraft app at {addr} is not reachable: {e}"))
            }),
        )
    }

    /// How many keys hold a connection to the app now.
    pub fn keys(&self) -> usize {
        self.seats.lock().map(|s| s.len()).unwrap_or(0)
    }

    /// Answer one request to `/mcp`.
    pub fn handle(&self, req: &Request) -> Response {
        if req.path() != "/mcp" {
            return Response::text(404, "not found: the MCP endpoint is /mcp\n");
        }
        if let Some(hosts) = &self.options.hosts {
            let ok = req.header("Host").is_some_and(|h| hosts.iter().any(|a| a.eq_ignore_ascii_case(h)));
            if !ok {
                return refusal(403, -32600, "this Host is not served here (start the endpoint with --allow-host for a name a tunnel uses)");
            }
        }
        if let Some(origin) = req.header("Origin")
            && !self.options.origins.iter().any(|o| o.eq_ignore_ascii_case(origin))
        {
            return refusal(403, -32600, "requests from web pages are not served (Origin)");
        }
        if req.method != "POST" {
            return refusal(405, -32600, "only POST: this endpoint keeps no streams").with_header("Allow", "POST");
        }
        let Some(key) = bearer(req) else {
            return refusal(401, -32600, "an invite's key goes in Authorization: Bearer <key> (room.invite gives one)")
                .with_header("WWW-Authenticate", "Bearer realm=\"wordcraft\"");
        };
        if !req.header("Content-Type").is_some_and(|c| c.split(';').next().is_some_and(|t| t.trim().eq_ignore_ascii_case("application/json"))) {
            return refusal(415, -32600, "the body is JSON (Content-Type: application/json)");
        }
        if req.body.len() > MAX_REQUEST_LINE {
            return refusal(413, -32600, "request too large");
        }
        let Ok(body) = std::str::from_utf8(&req.body) else { return refusal(400, -32700, "the body is not UTF-8") };
        let Ok(msg) = serde_json::from_str::<Value>(body) else { return refusal(400, -32700, "parse error") };
        // One message a POST. MCP over HTTP has had no batches since 2025-06-18, and a batch
        // would let one request run any number of tools and hold every answer in memory.
        if !msg.is_object() {
            return refusal(400, -32600, "one JSON-RPC message per POST: batches aren't accepted over HTTP");
        }
        let stateless = msg.get("params").and_then(stateless_version).is_some();
        if stateless {
            if let Err(e) = check_headers(req, &msg) {
                return refusal(400, HEADER_MISMATCH, &e);
            }
        } else if let Some(v) = req.header("MCP-Protocol-Version")
            && (!ALL_VERSIONS.contains(&v) || v == crate::server::STATELESS_VERSION)
        {
            // An older request names its version only in the header; 2026-07-28 ones name it
            // in `_meta` too.
            let mut e = json!({"jsonrpc": "2.0", "id": msg.get("id").cloned().unwrap_or(Value::Null), "error": {"code": UNSUPPORTED_VERSION, "message": format!("protocol version {v} is not supported without {META_VERSION} in _meta")}});
            e["error"]["data"] = json!({"supported": ALL_VERSIONS, "requested": v});
            return Response::json(400, e.to_string());
        }
        // While clients are slow to read what was already answered, nothing new runs: refused
        // before it runs, a request is safe to send again.
        if self.sending.load(Ordering::SeqCst) >= self.options.max_sending {
            return refusal(503, -32603, "the endpoint is still sending earlier answers: try again").with_header("Retry-After", "1");
        }
        let answer = match self.ask(key, body.to_string()) {
            Ok(a) => a,
            Err(resp) => return resp,
        };
        match answer {
            None => Response::new(202),
            Some(reply) => {
                // Counted until it has been sent (the request ran: its answer is never dropped).
                let bytes = reply.len();
                self.sending.fetch_add(bytes, Ordering::SeqCst);
                let share = Sending { counter: self.sending.clone(), bytes };
                let status = if stateless { status_of(&reply) } else { 200 };
                Response::json(status, reply).holding(share)
            }
        }
    }

    /// Run `body` on `key`'s thread, opening it first when it has none.
    fn ask(&self, key: &str, body: String) -> Result<Option<String>, Response> {
        for _ in 0..2 {
            let jobs = self.seat(key)?;
            let (tx, rx) = mpsc::channel();
            let until = Instant::now().checked_add(self.options.timeout).unwrap_or_else(Instant::now);
            match jobs.try_send(Job { body: body.clone(), answer: tx, until }) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    return Err(refusal(503, -32603, "too many requests are waiting for this key: try again").with_header("Retry-After", "1"));
                }
                Err(TrySendError::Disconnected(_)) => {
                    // The thread idled out: open it again.
                    self.forget(key);
                    continue;
                }
            }
            return match rx.recv_timeout(self.options.timeout) {
                Ok(a) if a.ended => {
                    self.forget(key);
                    Err(refusal(401, -32600, "the WordCraft app no longer takes this key: it was taken back, or its time was up")
                        .with_header("WWW-Authenticate", "Bearer realm=\"wordcraft\", error=\"invalid_token\""))
                }
                Ok(a) => Ok(a.reply),
                Err(mpsc::RecvTimeoutError::Timeout) => Err(refusal(504, -32603, "the WordCraft app took too long to answer")),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.forget(key);
                    Err(refusal(502, -32603, "the connection to the WordCraft app ended"))
                }
            };
        }
        Err(refusal(502, -32603, "the connection to the WordCraft app ended"))
    }

    fn forget(&self, key: &str) {
        if let Ok(mut seats) = self.seats.lock() {
            seats.remove(key);
        }
    }

    /// `key`'s thread, opened (and the key checked with the app) when it has none.
    fn seat(&self, key: &str) -> Result<SyncSender<Job>, Response> {
        {
            let Ok(mut seats) = self.seats.lock() else { return Err(refusal(500, -32603, "internal error")) };
            let idle = self.options.idle;
            seats.retain(|_, s| s.last.elapsed() < idle);
            if let Some(s) = seats.get_mut(key) {
                s.last = Instant::now();
                return Ok(s.jobs.clone());
            }
            if seats.len() >= self.options.max_keys {
                return Err(refusal(503, -32603, "too many participants are connected here").with_header("Retry-After", "5"));
            }
        }
        if self.opening.fetch_add(1, Ordering::SeqCst) >= 8 {
            self.opening.fetch_sub(1, Ordering::SeqCst);
            return Err(refusal(503, -32603, "busy: try again").with_header("Retry-After", "1"));
        }
        let opened = self.open_seat(key);
        self.opening.fetch_sub(1, Ordering::SeqCst);
        let jobs = opened?;
        let Ok(mut seats) = self.seats.lock() else { return Err(refusal(500, -32603, "internal error")) };
        // Another request with the same key may have opened one meanwhile: keep the first.
        let seat = seats.entry(key.to_string()).or_insert(Seat { jobs, last: Instant::now() });
        Ok(seat.jobs.clone())
    }

    /// Start a thread holding a connection to the app with `key`, once the app says the key is
    /// an invite's.
    fn open_seat(&self, key: &str) -> Result<SyncSender<Job>, Response> {
        let (jobs, inbox) = mpsc::sync_channel::<Job>(QUEUE);
        let (ready_tx, ready) = mpsc::channel::<Result<(), Response>>();
        let (open, key, idle, recheck) = (self.open.clone(), key.to_string(), self.options.idle, self.options.recheck);
        let spawned = std::thread::Builder::new().name("mcp http key".into()).spawn(move || {
            let mut backend = match open(&key) {
                Ok(b) => b,
                Err(e) => {
                    let _ = ready_tx.send(Err(refusal(502, -32603, &e)));
                    return;
                }
            };
            match backend.call("auth", json!({})) {
                Ok(v) if v.get("as").and_then(Value::as_str) == Some("participant") && v.get("remote") == Some(&Value::Bool(true)) => {}
                Ok(v) if v.get("as").and_then(Value::as_str) == Some("participant") => {
                    let _ = ready_tx.send(Err(refusal(
                        403,
                        -32600,
                        "this invite is for an agent on this computer: invite the agent as one on another machine (room.invite with remote: true) and use that key",
                    )));
                    return;
                }
                Ok(_) => {
                    let _ = ready_tx.send(Err(refusal(
                        403,
                        -32600,
                        "this is the window's key, which acts as you and never leaves this computer: invite the agent (room.invite) and use its key",
                    )));
                    return;
                }
                Err(e) if e.starts_with("unauthorized") => {
                    let _ = ready_tx.send(Err(refusal(
                        401,
                        -32600,
                        "the WordCraft app doesn't know this key: it was taken back, its time was up, or it was never handed out",
                    )
                    .with_header("WWW-Authenticate", "Bearer realm=\"wordcraft\", error=\"invalid_token\"")));
                    return;
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(refusal(502, -32603, &e)));
                    return;
                }
            }
            let _ = ready_tx.send(Ok(()));
            serve_key(Server::new(backend), &inbox, idle, recheck);
        });
        if spawned.is_err() {
            return Err(refusal(503, -32603, "no thread for this participant"));
        }
        match ready.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(())) => Ok(jobs),
            Ok(Err(resp)) => Err(resp),
            Err(_) => Err(refusal(504, -32603, "the WordCraft app took too long to check the key")),
        }
    }
}

/// A key's thread: answer its requests in turn until it idles out or the app refuses the key.
fn serve_key(mut server: Server, inbox: &Receiver<Job>, idle: Duration, recheck: Duration) {
    let mut checked = Instant::now();
    while let Ok(job) = inbox.recv_timeout(idle) {
        if Instant::now() >= job.until {
            continue;
        }
        // Every so often, whatever the request: is the key still the app's?
        if checked.elapsed() >= recheck {
            checked = Instant::now();
            let refused = server.backend().call("auth", json!({})).is_err_and(|e| e.starts_with("unauthorized"));
            if refused || server.backend().ended().is_some() {
                let _ = job.answer.send(Answer { reply: None, ended: true });
                return;
            }
        }
        let reply = server.handle_line(&job.body);
        // A failed request may be the first sign that the key was taken back (the copy says so
        // and its connection closes): ask the app whether it still takes the key.
        if reply.as_deref().is_some_and(failed) && server.backend().ended().is_none() {
            let _ = server.backend().call("auth", json!({}));
        }
        let ended = server.backend().ended().is_some();
        let _ = job.answer.send(Answer { reply, ended });
        if ended {
            return;
        }
    }
}

/// Whether a reply is an error, or a tool's result that says it failed.
fn failed(reply: &str) -> bool {
    serde_json::from_str::<Value>(reply)
        .is_ok_and(|v| v.get("error").is_some() || v.get("result").and_then(|r| r.get("isError")) == Some(&Value::Bool(true)))
}

/// The HTTP status of a 2026-07-28 reply: errors that are about the request, not the tool, have
/// their own.
fn status_of(reply: &str) -> u16 {
    let code = serde_json::from_str::<Value>(reply).ok().and_then(|v| v.get("error").and_then(|e| e.get("code")).and_then(Value::as_i64));
    match code {
        Some(UNSUPPORTED_VERSION | HEADER_MISMATCH | -32021) => 400,
        Some(METHOD_NOT_FOUND) => 404,
        _ => 200,
    }
}

/// The `Host` values a loopback endpoint on `port` answers to, and the extra names given.
pub fn loopback_hosts(port: u16, extra: &[String]) -> Vec<String> {
    let mut hosts = vec![format!("127.0.0.1:{port}"), format!("localhost:{port}"), format!("[::1]:{port}")];
    hosts.extend(extra.iter().cloned());
    hosts
}

/// Serve `endpoint` on `listener`, which must be on a loopback address: TLS and reaching it
/// from elsewhere are a tunnel's or the relay's job, never plain HTTP off this computer.
pub fn serve(listener: TcpListener, endpoint: Arc<Endpoint>) -> std::io::Result<()> {
    let addr: SocketAddr = listener.local_addr()?;
    if !addr.ip().is_loopback() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("{addr} is not a loopback address: the MCP endpoint serves only this computer"),
        ));
    }
    let config = wordcraft_http::Config { limits: wordcraft_http::Limits { body: MAX_REQUEST_LINE, ..Default::default() }, ..Default::default() };
    wordcraft_http::serve(listener, config, Arc::new(move |req: &Request| endpoint.handle(req)))
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;

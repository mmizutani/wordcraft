//! A small HTTP/1.1 server on `std`, for WordCraft's MCP endpoint (`wordcraft-cli mcp --http`)
//! and the relay (`wordcraft-relay`). One thread per connection, a cap on connections, and every
//! read bounded: the request head, the number of headers, the body (by `Content-Length` or
//! chunked), the time a connection may sit idle, and the time one request may take to arrive
//! whole. Requests and responses are whole bodies;
//! nothing streams.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Bounds on what one request may make the server read.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// The request line and headers together.
    pub head: usize,
    /// How many header lines.
    pub headers: usize,
    /// The body, after any chunked encoding is taken off.
    pub body: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { head: 16 << 10, headers: 64, body: 1 << 20 }
    }
}

/// One request, read whole.
#[derive(Clone, Debug, Default)]
pub struct Request {
    pub method: String,
    /// The request target as sent (`/mcp`, `/r/abc/mcp?x=1`).
    pub target: String,
    /// Header names as sent; [`Request::header`] compares them without case.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Who sent it, when it came over a socket.
    pub peer: Option<SocketAddr>,
}

impl Request {
    /// The first header called `name` (any case), trimmed.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.trim())
    }

    /// The target without its query string.
    pub fn path(&self) -> &str {
        self.target.split_once('?').map_or(self.target.as_str(), |(p, _)| p)
    }
}

/// One response, written whole with its `Content-Length`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Close the connection after this response.
    pub close: bool,
}

impl Response {
    pub fn new(status: u16) -> Self {
        Response { status, headers: Vec::new(), body: Vec::new(), close: false }
    }

    /// A JSON body.
    pub fn json(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Response::new(status).with_header("Content-Type", "application/json").with_body(body)
    }

    /// A plain-text body.
    pub fn text(status: u16, body: impl Into<String>) -> Self {
        Response::new(status).with_header("Content-Type", "text/plain; charset=utf-8").with_body(body.into())
    }

    pub fn with_header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_string(), value.into()));
        self
    }

    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    pub fn closing(mut self) -> Self {
        self.close = true;
        self
    }

    /// The first header called `name` (any case).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// Why a request couldn't be read: the status to answer with, then the connection closes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    pub status: u16,
    pub message: String,
}

fn refused(status: u16, message: impl Into<String>) -> Refused {
    Refused { status, message: message.into() }
}

impl Refused {
    pub fn response(&self) -> Response {
        Response::text(self.status, format!("{}\n", self.message)).closing()
    }
}

/// What reading a request's head gave.
#[derive(Debug)]
pub enum Head {
    /// The connection closed (or sat idle past its timeout) before a request began.
    Closed,
    /// A request, and whether it came as HTTP/1.0 (which closes after the response).
    Request(Request, bool),
}

/// The request line and headers, at most `limits.head` bytes and `limits.headers` lines.
pub fn read_head(reader: &mut impl BufRead, limits: &Limits) -> Result<Head, Refused> {
    let mut used = 0usize;
    let line = |reader: &mut dyn BufRead, used: &mut usize| -> Result<Option<String>, Refused> {
        let left = limits.head.saturating_sub(*used);
        let mut buf = Vec::new();
        let read = reader.take(left as u64 + 1).read_until(b'\n', &mut buf);
        match read {
            Ok(0) => return Ok(None),
            Ok(_) => {}
            Err(e) if timed_out(&e) && buf.is_empty() && *used == 0 => return Ok(None),
            Err(e) if timed_out(&e) => return Err(refused(408, "the request took too long to arrive")),
            Err(e) => return Err(refused(400, format!("could not read the request: {e}"))),
        }
        *used = used.saturating_add(buf.len());
        if buf.last() != Some(&b'\n') {
            return Err(if *used > limits.head { refused(431, "request head too large") } else { refused(400, "request ended early") });
        }
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        String::from_utf8(buf).map(Some).map_err(|_| refused(400, "request head is not UTF-8"))
    };
    // Blank lines before the request line are allowed (RFC 9112 §2.2).
    let start = loop {
        match line(reader, &mut used)? {
            None => return Ok(Head::Closed),
            Some(l) if l.is_empty() => {
                if used > 64 {
                    return Err(refused(400, "no request line"));
                }
            }
            Some(l) => break l,
        }
    };
    let mut parts = start.split(' ');
    let (Some(method), Some(target), Some(version), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err(refused(400, "bad request line"));
    };
    if !matches!(version, "HTTP/1.1" | "HTTP/1.0") {
        return Err(refused(505, "only HTTP/1.1 and HTTP/1.0"));
    }
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) || !target.starts_with('/') {
        return Err(refused(400, "bad request line"));
    }
    let mut headers = Vec::new();
    loop {
        let Some(l) = line(reader, &mut used)? else { return Err(refused(400, "request ended in its headers")) };
        if l.is_empty() {
            break;
        }
        if l.starts_with([' ', '\t']) {
            return Err(refused(400, "folded header lines are not accepted"));
        }
        let Some((name, value)) = l.split_once(':') else { return Err(refused(400, "bad header line")) };
        if name.is_empty() || name.bytes().any(|b| b.is_ascii_whitespace() || b.is_ascii_control()) {
            return Err(refused(400, "bad header name"));
        }
        if headers.len() >= limits.headers {
            return Err(refused(431, "too many headers"));
        }
        headers.push((name.to_string(), value.trim().to_string()));
    }
    let req = Request { method: method.to_string(), target: target.to_string(), headers, body: Vec::new(), peer: None };
    Ok(Head::Request(req, version == "HTTP/1.0"))
}

/// Read the body `req`'s headers announce into `req.body`: by `Content-Length`, chunked, or none.
pub fn read_body(reader: &mut impl BufRead, req: &mut Request, limits: &Limits) -> Result<(), Refused> {
    let te = req.header("Transfer-Encoding").map(str::to_ascii_lowercase);
    let cl = req.header("Content-Length").map(str::to_string);
    match (te, cl) {
        (Some(_), Some(_)) => Err(refused(400, "both Transfer-Encoding and Content-Length")),
        (Some(te), None) if te == "chunked" => read_chunked(reader, &mut req.body, limits.body),
        (Some(_), None) => Err(refused(501, "only chunked transfer encoding")),
        (None, Some(cl)) => {
            let n: usize = cl.parse().map_err(|_| refused(400, "bad Content-Length"))?;
            if n > limits.body {
                return Err(refused(413, format!("body over {} bytes", limits.body)));
            }
            let mut body = vec![0u8; n];
            reader.read_exact(&mut body).map_err(body_error)?;
            req.body = body;
            Ok(())
        }
        (None, None) => Ok(()),
    }
}

fn timed_out(e: &std::io::Error) -> bool {
    matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)
}

fn body_error(e: std::io::Error) -> Refused {
    if timed_out(&e) { refused(408, "the request took too long to arrive") } else { refused(400, "body ended early") }
}

fn read_chunked(reader: &mut impl BufRead, body: &mut Vec<u8>, max: usize) -> Result<(), Refused> {
    let size_line = |reader: &mut dyn BufRead| -> Result<String, Refused> {
        let mut buf = Vec::new();
        reader.take(256).read_until(b'\n', &mut buf).map_err(body_error)?;
        if buf.last() != Some(&b'\n') {
            return Err(refused(400, "bad chunk"));
        }
        String::from_utf8(buf).map(|s| s.trim_end().to_string()).map_err(|_| refused(400, "bad chunk"))
    };
    loop {
        let line = size_line(reader)?;
        let hex = line.split(';').next().unwrap_or_default().trim();
        let n = usize::from_str_radix(hex, 16).map_err(|_| refused(400, "bad chunk size"))?;
        if n == 0 {
            // Trailers, up to the blank line; a few at most.
            for _ in 0..16 {
                if size_line(reader)?.is_empty() {
                    return Ok(());
                }
            }
            return Err(refused(400, "too many trailers"));
        }
        if body.len().saturating_add(n) > max {
            return Err(refused(413, format!("body over {max} bytes")));
        }
        let start = body.len();
        body.resize(start + n, 0);
        let Some(chunk) = body.get_mut(start..) else { return Err(refused(400, "bad chunk")) };
        reader.read_exact(chunk).map_err(body_error)?;
        if !size_line(reader)?.is_empty() {
            return Err(refused(400, "bad chunk end"));
        }
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        100 => "Continue",
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        408 => "Request Timeout",
        409 => "Conflict",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        505 => "HTTP Version Not Supported",
        _ => "",
    }
}

/// Write `resp` whole. Header values with line breaks are dropped (they would split the head).
pub fn write_response(w: &mut impl Write, resp: &Response, close: bool) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {} {}\r\n", resp.status, reason(resp.status));
    for (name, value) in &resp.headers {
        if name.eq_ignore_ascii_case("Content-Length") || name.eq_ignore_ascii_case("Connection") {
            continue;
        }
        if name.contains(['\r', '\n', ':']) || value.contains(['\r', '\n']) {
            continue;
        }
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if resp.status >= 200 && resp.status != 204 {
        head.push_str(&format!("Content-Length: {}\r\n", resp.body.len()));
    }
    if close || resp.close {
        head.push_str("Connection: close\r\n");
    }
    head.push_str("\r\n");
    w.write_all(head.as_bytes())?;
    if resp.status != 204 {
        w.write_all(&resp.body)?;
    }
    w.flush()
}

/// How [`serve`] runs.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub limits: Limits,
    /// Connections served at once; more are answered 503 and closed.
    pub max_connections: usize,
    /// How long a connection may wait between requests.
    pub idle: Duration,
    /// How long one request, its head and body, may take to arrive once it began: a client
    /// sending a byte now and then can't hold a connection longer.
    pub request: Duration,
    /// How long writing one response may take.
    pub write: Duration,
    /// Requests on one connection before it closes.
    pub max_requests: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            limits: Limits::default(),
            max_connections: 64,
            idle: Duration::from_secs(30),
            request: Duration::from_secs(60),
            write: Duration::from_secs(30),
            max_requests: 1000,
        }
    }
}

/// What answers each request.
pub type Handler = Arc<dyn Fn(&Request) -> Response + Send + Sync>;

/// Connections turned away (503) that are closed gently at once; more are dropped.
const MAX_CLOSING: usize = 16;

/// Accept connections on `listener` until it fails, each on a thread of its own.
pub fn serve(listener: TcpListener, config: Config, handler: Handler) -> std::io::Result<()> {
    let open = Arc::new(AtomicUsize::new(0));
    // Connections turned away and still being closed, each on a thread of its own, so that a
    // client trickling bytes into one never holds up the next accept.
    let closing = Arc::new(AtomicUsize::new(0));
    loop {
        let (stream, peer) = match listener.accept() {
            Ok(s) => s,
            // A connection that went away before it was accepted, or a short-lived lack of
            // descriptors: keep serving.
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(e) => {
                log::warn!("http: accept failed: {e}");
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        let _ = stream.set_nodelay(true);
        let _ = stream.set_write_timeout(Some(config.write));
        if open.fetch_add(1, Ordering::SeqCst) >= config.max_connections {
            open.fetch_sub(1, Ordering::SeqCst);
            let mut s = stream;
            if closing.fetch_add(1, Ordering::SeqCst) >= MAX_CLOSING {
                // Too many being closed already: drop this one at once.
                closing.fetch_sub(1, Ordering::SeqCst);
                let _ = s.shutdown(std::net::Shutdown::Both);
                continue;
            }
            let done = closing.clone();
            let spawned = std::thread::Builder::new().name("http turned away".into()).spawn(move || {
                let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
                let _ = write_response(&mut s, &Response::text(503, "too many connections\n").with_header("Retry-After", "1"), true);
                close_gently(&s, &mut BufReader::new(&s), Duration::from_millis(200));
                done.fetch_sub(1, Ordering::SeqCst);
            });
            if spawned.is_err() {
                // The closure (and the connection) went with the failed spawn.
                closing.fetch_sub(1, Ordering::SeqCst);
            }
            continue;
        }
        let (open, handler) = (open.clone(), handler.clone());
        let spawned = std::thread::Builder::new().name("http connection".into()).spawn(move || {
            connection(stream, peer, &config, &handler);
            open.fetch_sub(1, Ordering::SeqCst);
        });
        if spawned.is_err() {
            log::warn!("http: no thread for a connection");
        }
    }
}

/// The read side of a connection: each read waits only until `until`, so a slow drip of bytes
/// can't make a request take longer than its deadline.
struct Timed {
    stream: TcpStream,
    until: Instant,
}

impl Read for Timed {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.until.checked_duration_since(Instant::now()).filter(|l| !l.is_zero()).ok_or(std::io::ErrorKind::TimedOut)?;
        self.stream.set_read_timeout(Some(left))?;
        self.stream.read(buf)
    }
}

/// `d` from now (far ahead when that doesn't fit an [`Instant`]).
fn after(d: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(d).or_else(|| now.checked_add(Duration::from_secs(365 * 24 * 60 * 60))).unwrap_or(now)
}

/// Answer a request that couldn't be read with why, and close.
fn refuse(writer: &mut TcpStream, reader: &mut BufReader<Timed>, e: &Refused) {
    let _ = write_response(writer, &e.response(), true);
    reader.get_mut().until = after(Duration::from_secs(1));
    close_gently(writer, reader.get_mut(), Duration::from_secs(1));
}

/// Serve one connection's requests until it closes, idles out or asks to close.
pub fn connection(stream: TcpStream, peer: SocketAddr, config: &Config, handler: &Handler) {
    let Ok(read) = stream.try_clone() else { return };
    let mut reader = BufReader::new(Timed { stream: read, until: Instant::now() });
    let mut writer = stream;
    for _ in 0..config.max_requests.max(1) {
        // Wait up to `idle` for the next request to begin, then up to `request` for all of it.
        reader.get_mut().until = after(config.idle);
        match reader.fill_buf() {
            Ok([]) | Err(_) => return,
            Ok(_) => {}
        }
        reader.get_mut().until = after(config.request);
        let (mut req, http10) = match read_head(&mut reader, &config.limits) {
            Ok(Head::Closed) => return,
            Ok(Head::Request(r, http10)) => (r, http10),
            Err(e) => {
                refuse(&mut writer, &mut reader, &e);
                return;
            }
        };
        req.peer = Some(peer);
        if req.header("Expect").is_some_and(|e| e.eq_ignore_ascii_case("100-continue")) && writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").is_err()
        {
            return;
        }
        if let Err(e) = read_body(&mut reader, &mut req, &config.limits) {
            refuse(&mut writer, &mut reader, &e);
            return;
        }
        let close = http10 || req.header("Connection").is_some_and(|c| c.eq_ignore_ascii_case("close"));
        let resp = handler(&req);
        let close = close || resp.close;
        if write_response(&mut writer, &resp, close).is_err() || close {
            return;
        }
    }
    let _ = writer.shutdown(std::net::Shutdown::Both);
}

/// Close after an answer sent before the request was read whole: stop writing, then read and
/// drop a little of what the client is still sending, so its unread bytes don't make the
/// system reset the connection and lose the answer.
fn close_gently(stream: &TcpStream, reader: &mut impl Read, wait: Duration) {
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let _ = stream.set_read_timeout(Some(wait));
    let mut sink = [0u8; 4096];
    for _ in 0..16 {
        match reader.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
}

/// Whether two secrets are equal, in time that depends only on their lengths.
pub fn same_secret(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The token in an `Authorization: Bearer <token>` header.
pub fn bearer(req: &Request) -> Option<&str> {
    let v = req.header("Authorization")?;
    let (scheme, token) = v.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

#[cfg(test)]
mod tests;

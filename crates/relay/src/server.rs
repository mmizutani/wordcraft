//! The relay's side: rooms that desktops open, the queue of agents' requests each one waits on,
//! and the limits that keep one room, one agent or one address from taking the relay over.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wordcraft_http::{Request, Response, bearer, same_secret};

use crate::{is_hex, random_hex, room_of, wire};

/// A room's id, in its URL: 128 bits of a hash of its secret, as hex ([`room_of`]).
const ROOM_HEX: usize = 32;
/// A room's secret, which only its desktop holds: 256 random bits as hex.
const SECRET_HEX: usize = 64;
/// An agent's request body, as the MCP endpoint takes it.
pub const MAX_REQUEST: usize = 1 << 20;
/// A desktop's answer: a PDF of up to 32 MB, as base64 in JSON.
pub const MAX_REPLY: usize = 96 << 20;
/// Answers larger than an agent's request that one token's desktops may be sending at once,
/// and that all desktops may: each holds a connection, and up to [`MAX_REPLY`] of memory.
const LARGE_REPLIES_PER_TOKEN: usize = 2;
const LARGE_REPLIES: usize = 16;
/// Request ids stay below 2^53, a whole number in any JSON reader.
const ID_MASK: u64 = (1 << 53) - 1;
/// Polls one room's desktop may have waiting at once: the app has two. More would only hold
/// connections that the relay's other rooms need.
const MAX_POLLS: usize = 4;
/// Requests a desktop takes in one poll.
const PER_POLL: usize = 16;

/// How the relay runs.
#[derive(Clone, Debug)]
pub struct Config {
    /// The relay's address as agents reach it, without a trailing `/` (`https://relay.example.org`).
    pub public_url: String,
    /// The tokens desktops open rooms with. Empty: nobody can.
    pub tokens: Vec<String>,
    /// Take the agent's address from `X-Forwarded-For` (behind a proxy that sets it) rather
    /// than from the connection.
    pub trust_forwarded: bool,
    pub max_rooms: usize,
    pub rooms_per_token: usize,
    /// How long a desktop's poll waits for a request.
    pub poll_wait: Duration,
    /// How long an agent waits for the desktop's answer.
    pub answer_wait: Duration,
    /// A room whose desktop hasn't polled for this long is offline: agents are told so at once.
    pub offline_after: Duration,
    /// A room nobody polled for this long is forgotten.
    pub forget_after: Duration,
    /// Requests waiting in one room.
    pub max_waiting: usize,
    /// Agents' requests waiting for an answer, in all the rooms of one token: queued or being
    /// answered. One token can't hold every connection the relay has.
    pub waiting_per_token: usize,
    /// Answers' bytes waiting for agents to read them, from one token's rooms and in all: while
    /// either is spent, new requests are refused (503) before they reach the desktop.
    pub sending_per_token: usize,
    pub max_sending: usize,
    /// Requests per second (and burst) from one address, and to one room.
    pub per_source: (f64, f64),
    pub per_room: (f64, f64),
}

impl Default for Config {
    fn default() -> Self {
        Config {
            public_url: String::new(),
            tokens: Vec::new(),
            trust_forwarded: false,
            max_rooms: 1024,
            rooms_per_token: 16,
            poll_wait: Duration::from_secs(25),
            answer_wait: Duration::from_secs(180),
            offline_after: Duration::from_secs(40),
            forget_after: Duration::from_secs(24 * 60 * 60),
            max_waiting: 64,
            waiting_per_token: 128,
            sending_per_token: 192 << 20,
            max_sending: 1 << 30,
            per_source: (20.0, 40.0),
            per_room: (50.0, 100.0),
        }
    }
}

/// So many requests a second, with bursts up to a size.
#[derive(Clone, Copy, Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

impl Bucket {
    fn new(burst: f64) -> Self {
        Bucket { tokens: burst, last: Instant::now() }
    }

    fn take(&mut self, (rate, burst): (f64, f64)) -> bool {
        let now = Instant::now();
        self.tokens = (self.tokens + now.duration_since(self.last).as_secs_f64() * rate).min(burst);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// An agent's request waiting for the desktop, and when its agent stops waiting.
struct Queued {
    request: Value,
    until: Instant,
}

struct Waiting {
    queue: VecDeque<Queued>,
    answers: HashMap<u64, Sender<Response>>,
    next: u64,
    /// When the desktop last asked for requests, or finished asking.
    seen: Instant,
    /// The desktop's polls waiting now.
    polling: usize,
    closed: bool,
    bucket: Bucket,
}

struct Room {
    secret: String,
    token: String,
    waiting: Mutex<Waiting>,
    wake: Condvar,
}

impl Room {
    fn lock(&self) -> Option<MutexGuard<'_, Waiting>> {
        self.waiting.lock().ok()
    }

    fn online(w: &Waiting, offline_after: Duration) -> bool {
        w.polling > 0 || w.seen.elapsed() < offline_after
    }
}

/// The relay: answers every request from any thread ([`Relay::handle`]).
pub struct Relay {
    config: Config,
    rooms: Mutex<HashMap<String, Arc<Room>>>,
    sources: Mutex<HashMap<IpAddr, Bucket>>,
    /// Agents' requests waiting for an answer, by the token of their room.
    waiting: Arc<Mutex<HashMap<String, usize>>>,
    /// Answers' bytes waiting for agents to read them.
    sending: Arc<Mutex<Sending>>,
    /// Large answers being received: the token of their room, and when the HTTP server gives up
    /// on each (the time it gives the body), when its place goes back if it hasn't before.
    large: Mutex<Vec<(String, Instant)>>,
}

/// Answers' bytes waiting for agents to read them, in all and by token.
#[derive(Default)]
struct Sending {
    total: usize,
    by_token: HashMap<String, usize>,
}

/// One answer's bytes in [`Sending`], given back once it has been sent (or given up on).
struct Unsent {
    sending: Arc<Mutex<Sending>>,
    token: String,
    bytes: usize,
}

impl Drop for Unsent {
    fn drop(&mut self) {
        if let Ok(mut s) = self.sending.lock() {
            s.total = s.total.saturating_sub(self.bytes);
            if let Some(n) = s.by_token.get_mut(&self.token) {
                *n = n.saturating_sub(self.bytes);
                if *n == 0 {
                    s.by_token.remove(&self.token);
                }
            }
        }
    }
}

/// One request's place in its token's count, given back however its wait ends.
struct Waits {
    counts: Arc<Mutex<HashMap<String, usize>>>,
    token: String,
}

impl Drop for Waits {
    fn drop(&mut self) {
        if let Ok(mut c) = self.counts.lock()
            && let Some(n) = c.get_mut(&self.token)
        {
            *n = n.saturating_sub(1);
            if *n == 0 {
                c.remove(&self.token);
            }
        }
    }
}

/// An error an MCP client reads: a JSON-RPC error with no id.
fn refusal(status: u16, message: &str) -> Response {
    Response::json(status, json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32000, "message": message}}).to_string())
}

impl Relay {
    pub fn new(config: Config) -> Self {
        Relay {
            config,
            rooms: Mutex::new(HashMap::new()),
            sources: Mutex::new(HashMap::new()),
            waiting: Arc::new(Mutex::new(HashMap::new())),
            large: Mutex::new(Vec::new()),
            sending: Arc::new(Mutex::new(Sending::default())),
        }
    }

    fn room(&self, id: &str) -> Option<Arc<Room>> {
        self.rooms.lock().ok()?.get(id).cloned()
    }

    /// The room `id` when `req` carries its secret.
    fn hosts(&self, req: &Request, id: &str) -> Option<Arc<Room>> {
        let room = self.room(id)?;
        bearer(req).is_some_and(|s| same_secret(s, &room.secret)).then_some(room)
    }

    /// How much body `req` may have: a desktop's answer may be large, when it comes with its
    /// room's secret, an agent waits for one, and few large answers are arriving (from this
    /// token's desktops, and in all); everything else is an MCP request's size.
    pub fn body_limit(&self, req: &Request) -> usize {
        let path = req.path();
        let Some(room) = path.strip_prefix("/host/r/").and_then(|r| r.strip_suffix("/reply")).and_then(|id| self.hosts(req, id)) else {
            return MAX_REQUEST;
        };
        let size = req.header("Content-Length").and_then(|n| n.trim().parse::<usize>().ok());
        if size.is_some_and(|n| n <= MAX_REQUEST) || room.lock().is_none_or(|w| w.answers.is_empty()) {
            return MAX_REQUEST;
        }
        let Ok(mut large) = self.large.lock() else { return MAX_REQUEST };
        let now = Instant::now();
        large.retain(|(_, until)| *until > now);
        if large.len() >= LARGE_REPLIES || large.iter().filter(|(t, _)| *t == room.token).count() >= LARGE_REPLIES_PER_TOKEN {
            return MAX_REQUEST;
        }
        let secs = 60 + size.unwrap_or(MAX_REPLY).min(MAX_REPLY) / (64 << 10);
        large.push((room.token.clone(), now + Duration::from_secs(u64::try_from(secs).unwrap_or(u64::MAX))));
        MAX_REPLY
    }

    /// A large answer has arrived (or failed to): its place goes to the next.
    fn large_done(&self, token: &str) {
        if let Ok(mut large) = self.large.lock()
            && let Some(i) = large.iter().position(|(t, _)| t == token)
        {
            large.remove(i);
        }
    }

    pub fn handle(&self, req: &Request) -> Response {
        let path = req.path().to_string();
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        match (req.method.as_str(), parts.as_slice()) {
            ("GET", ["healthz"]) => Response::text(200, "ok\n"),
            ("POST", ["host", "rooms"]) => self.open_room(req),
            ("POST", ["host", "r", id, "poll"]) => self.poll(req, id),
            ("POST", ["host", "r", id, "reply"]) => self.reply(req, id),
            ("DELETE", ["host", "r", id]) => self.close(req, id),
            (_, ["r", id, rest @ ..]) if !rest.is_empty() => {
                let target = format!("/{}{}", rest.join("/"), req.target.find('?').and_then(|i| req.target.get(i..)).unwrap_or_default());
                self.forward(req, id, &target)
            }
            _ => Response::text(404, "not found\n"),
        }
    }

    /// `POST /host/rooms`: open a room for a desktop with a token, or open its room again.
    fn open_room(&self, req: &Request) -> Response {
        let Some(token) = bearer(req).and_then(|t| self.config.tokens.iter().find(|k| same_secret(t, k))).cloned() else {
            return Response::text(401, "a relay token goes in Authorization: Bearer\n")
                .with_header("WWW-Authenticate", "Bearer realm=\"wordcraft-relay\"");
        };
        let asked: Value = if req.body.is_empty() { json!({}) } else { serde_json::from_slice(&req.body).unwrap_or(Value::Null) };
        let claim = match (asked.get("room").and_then(Value::as_str), asked.get("secret").and_then(Value::as_str)) {
            (Some(r), Some(s)) if is_hex(r, ROOM_HEX) && is_hex(s, SECRET_HEX) => Some((r.to_string(), s.to_string())),
            (None, None) if asked.is_object() => None,
            _ => return Response::text(400, "`room` and `secret`, both, or neither\n"),
        };
        let Ok(mut rooms) = self.rooms.lock() else { return Response::text(500, "internal error\n") };
        let forget = self.config.forget_after;
        rooms.retain(|_, r| r.lock().is_some_and(|w| !w.closed && (w.polling > 0 || w.seen.elapsed() < forget)));
        let (id, secret) = match claim {
            // Only the secret a room's id was made from opens it, whether the relay still holds
            // the room or not.
            Some((id, secret)) if room_of(&secret) != id => return Response::text(409, "that room is someone else's\n"),
            Some((id, secret)) => {
                if let Some(room) = rooms.get(&id) {
                    if !same_secret(&secret, &room.secret) {
                        return Response::text(409, "that room is someone else's\n");
                    }
                    // Another desktop with the same saved room (a second copy of the app on the
                    // same settings) is asking for it now: two desktops never share a room's
                    // requests. This one gets a room of its own.
                    if room.lock().is_none_or(|w| Room::online(&w, self.config.offline_after)) {
                        return Response::text(409, "that room is in use by another desktop\n");
                    }
                    return self.opened(&id, &secret);
                }
                (id, secret)
            }
            None => match random_hex(SECRET_HEX / 2) {
                Ok(secret) => (room_of(&secret), secret),
                Err(_) => return Response::text(503, "no room could be made\n"),
            },
        };
        if rooms.len() >= self.config.max_rooms {
            return Response::text(503, "the relay holds as many rooms as it can\n").with_header("Retry-After", "60");
        }
        if rooms.values().filter(|r| r.token == token).count() >= self.config.rooms_per_token {
            return Response::text(429, "this token holds as many rooms as it may\n");
        }
        // Request ids start somewhere new each time a room opens, again after a restart or a
        // close, so a late answer to a request of the room before can't answer one of this.
        let Ok(start) = random_hex(8).map(|h| u64::from_str_radix(&h, 16).unwrap_or(0) & ID_MASK) else {
            return Response::text(503, "no room could be made\n");
        };
        let waiting = Waiting {
            queue: VecDeque::new(),
            answers: HashMap::new(),
            next: start,
            seen: Instant::now(),
            polling: 0,
            closed: false,
            bucket: Bucket::new(self.config.per_room.1),
        };
        rooms.insert(id.clone(), Arc::new(Room { secret: secret.clone(), token, waiting: Mutex::new(waiting), wake: Condvar::new() }));
        log::info!("relay: room {} opened", short(&id));
        self.opened(&id, &secret)
    }

    fn opened(&self, id: &str, secret: &str) -> Response {
        Response::json(200, json!({"room": id, "secret": secret, "url": format!("{}/r/{id}/mcp", self.config.public_url)}).to_string())
    }

    /// `POST /host/r/<room>/poll`: the requests waiting for the desktop, after waiting a while
    /// for one.
    fn poll(&self, req: &Request, id: &str) -> Response {
        let Some(room) = self.hosts(req, id) else { return Response::text(404, "no such room (or not its secret)\n") };
        let Some(mut w) = room.lock() else { return Response::text(500, "internal error\n") };
        if w.polling >= MAX_POLLS {
            return Response::text(429, "this room's desktop is already waiting for requests as often as it may\n").with_header("Retry-After", "1");
        }
        w.polling += 1;
        w.seen = Instant::now();
        let until = Instant::now() + self.config.poll_wait;
        while w.queue.is_empty() && !w.closed {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            w = match room.wake.wait_timeout(w, left) {
                Ok((w, _)) => w,
                Err(_) => return Response::text(500, "internal error\n"),
            };
        }
        w.polling = w.polling.saturating_sub(1);
        w.seen = Instant::now();
        if w.closed {
            return Response::text(404, "the room was closed\n");
        }
        // Each says how long its agent still waits, so the desktop never starts one whose agent
        // has given up (and may already be asking again).
        let now = Instant::now();
        let n = w.queue.len().min(PER_POLL);
        let requests: Vec<Value> = w
            .queue
            .drain(..n)
            .filter_map(|q| {
                let left = q.until.saturating_duration_since(now);
                let mut r = q.request;
                r["ttlMs"] = json!(u64::try_from(left.as_millis()).unwrap_or(u64::MAX));
                (!left.is_zero()).then_some(r)
            })
            .collect();
        Response::json(200, json!({"requests": requests}).to_string())
    }

    /// `POST /host/r/<room>/reply`: the desktop's answer to one request.
    fn reply(&self, req: &Request, id: &str) -> Response {
        let Some(room) = self.hosts(req, id) else { return Response::text(404, "no such room (or not its secret)\n") };
        if req.body.len() > MAX_REQUEST {
            self.large_done(&room.token);
        }
        let Some((n, resp)) = serde_json::from_slice::<Value>(&req.body).ok().as_ref().and_then(wire::response_from) else {
            return Response::text(400, "an answer is {\"id\", \"status\", \"headers\", \"body\"}\n");
        };
        let Some(mut w) = room.lock() else { return Response::text(500, "internal error\n") };
        w.seen = Instant::now();
        match w.answers.remove(&n) {
            Some(tx) => {
                let _ = tx.send(resp);
                Response::new(204)
            }
            None => Response::text(404, "nobody waits for that answer any more\n"),
        }
    }

    /// `DELETE /host/r/<room>`: the desktop stopped sharing.
    fn close(&self, req: &Request, id: &str) -> Response {
        let Some(room) = self.hosts(req, id) else { return Response::text(404, "no such room (or not its secret)\n") };
        if let Some(mut w) = room.lock() {
            w.closed = true;
            w.queue.clear();
            // Everyone waiting hears at once that nobody will answer.
            w.answers.clear();
        }
        room.wake.notify_all();
        if let Ok(mut rooms) = self.rooms.lock() {
            rooms.remove(id);
        }
        log::info!("relay: room {} closed", short(id));
        Response::new(204)
    }

    /// Where an agent's request comes from: the connection, or the first `X-Forwarded-For`
    /// address when the proxy in front is trusted to set it.
    fn source(&self, req: &Request) -> Option<IpAddr> {
        if self.config.trust_forwarded
            && let Some(ip) = req.header("X-Forwarded-For").and_then(|v| v.split(',').next()).and_then(|v| v.trim().parse().ok())
        {
            return Some(ip);
        }
        req.peer.map(|p| p.ip())
    }

    fn allowed_from(&self, ip: Option<IpAddr>) -> bool {
        let Some(ip) = ip else { return true };
        let Ok(mut sources) = self.sources.lock() else { return false };
        if sources.len() > 100_000 {
            let burst = self.config.per_source.1;
            // Full buckets tell nothing; drop them first.
            sources.retain(|_, b| b.last.elapsed() < Duration::from_secs(60) && b.tokens < burst);
            if sources.len() > 100_000 {
                sources.clear();
            }
        }
        sources.entry(ip).or_insert_with(|| Bucket::new(self.config.per_source.1)).take(self.config.per_source)
    }

    /// An agent's request to room `id`: wait for the desktop's answer.
    fn forward(&self, req: &Request, id: &str, target: &str) -> Response {
        if !self.allowed_from(self.source(req)) {
            return refusal(429, "too many requests from this address").with_header("Retry-After", "1");
        }
        let Some(room) = self.room(id) else { return refusal(404, "no such room: the link is wrong, or the document isn't shared any more") };
        let Ok(body) = std::str::from_utf8(&req.body) else { return refusal(400, "the body is not UTF-8") };
        // While agents are slow to read answers already given, nothing new goes to the desktop:
        // refused before it runs, a request is safe to send again.
        if let Ok(s) = self.sending.lock()
            && (s.total >= self.config.max_sending || s.by_token.get(&room.token).is_some_and(|n| *n >= self.config.sending_per_token))
        {
            return refusal(503, "the relay is still sending earlier answers for this relay user's documents: try again")
                .with_header("Retry-After", "1");
        }
        let waits = {
            let Ok(mut c) = self.waiting.lock() else { return refusal(500, "internal error") };
            let n = c.entry(room.token.clone()).or_insert(0);
            if *n >= self.config.waiting_per_token {
                return refusal(503, "too many requests are waiting for this relay user's documents").with_header("Retry-After", "1");
            }
            *n += 1;
            Waits { counts: self.waiting.clone(), token: room.token.clone() }
        };
        let (tx, rx) = mpsc::channel();
        let n = {
            let Some(mut w) = room.lock() else { return refusal(500, "internal error") };
            if w.closed {
                return refusal(404, "the document isn't shared any more");
            }
            if !Room::online(&w, self.config.offline_after) {
                return refusal(
                    503,
                    "the room's host is offline: the WordCraft app that shares this document isn't connected (the computer may be asleep)",
                )
                .with_header("Retry-After", "30");
            }
            if !w.bucket.take(self.config.per_room) {
                return refusal(429, "too many requests to this room").with_header("Retry-After", "1");
            }
            if w.queue.len() >= self.config.max_waiting {
                return refusal(503, "too many requests are waiting for this room").with_header("Retry-After", "1");
            }
            let n = w.next;
            w.next = w.next.wrapping_add(1) & ID_MASK;
            w.queue.push_back(Queued { request: wire::forward(n, req, target, body), until: Instant::now() + self.config.answer_wait });
            w.answers.insert(n, tx);
            n
        };
        room.wake.notify_all();
        match rx.recv_timeout(self.config.answer_wait) {
            Ok(resp) => {
                // Counted until the agent has read it: the request's place, and its bytes.
                let bytes = resp.body.len();
                if let Ok(mut s) = self.sending.lock() {
                    s.total = s.total.saturating_add(bytes);
                    *s.by_token.entry(room.token.clone()).or_insert(0) += bytes;
                }
                let unsent = Unsent { sending: self.sending.clone(), token: room.token.clone(), bytes };
                resp.holding((waits, unsent))
            }
            Err(e) => {
                if let Some(mut w) = room.lock() {
                    w.answers.remove(&n);
                    w.queue.retain(|q| q.request.get("id").and_then(Value::as_u64) != Some(n));
                }
                match e {
                    mpsc::RecvTimeoutError::Timeout => refusal(504, "the room's host took too long to answer"),
                    mpsc::RecvTimeoutError::Disconnected => refusal(503, "the room's host stopped sharing the document"),
                }
            }
        }
    }
}

/// The start of a room id, for the log: enough to tell rooms apart, not enough to reach one.
fn short(id: &str) -> &str {
    id.get(..6).unwrap_or(id)
}

/// Run the relay on `listener` until it fails.
pub fn serve(listener: std::net::TcpListener, relay: Arc<Relay>, max_connections: usize) -> std::io::Result<()> {
    let config = wordcraft_http::Config { max_connections, idle: Duration::from_secs(60), write: Duration::from_secs(60), ..Default::default() };
    let limit = relay.clone();
    wordcraft_http::serve_with(
        listener,
        config,
        Arc::new(move |req: &Request| limit.body_limit(req)),
        Arc::new(move |req: &Request| relay.handle(req)),
    )
}

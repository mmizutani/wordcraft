//! The desktop's side: open a room on a relay and answer the requests agents send it, by asking
//! for them over HTTPS. Only outgoing connections, so it works behind any router and on any
//! network that lets the computer browse the web.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wordcraft_http::{Handler, Request};

use crate::{relay_url, wire};

/// Polls waiting on the relay at once: one answers while another is on its way.
const POLLERS: usize = 2;
/// Requests answered at once; more wait for their turn.
const AT_ONCE: usize = 16;
/// The longest answer the relay sends: its own refusals, and a poll's requests.
const MAX_FROM_RELAY: u64 = 64 << 20;

/// Why the relay refuses to open a room again: it holds a room with that id for someone else.
const TAKEN: &str = "the relay gave this room to someone else";

/// Whether the room can be reached now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Agents' requests reach this computer.
    Connected,
    /// The relay can't be reached; trying again. Agents are told the host is offline.
    Retrying(String),
    /// The relay refused the token, or the room was taken: nothing more is tried.
    Refused(String),
    Stopped,
}

struct Shared {
    relay: String,
    token: String,
    room: String,
    secret: String,
    url: String,
    agent: ureq::Agent,
    stop: AtomicBool,
    status: Mutex<Status>,
    busy: AtomicUsize,
    handler: Handler,
}

/// A room open on a relay, answered from this computer until [`Hosting::stop`] (or drop).
pub struct Hosting {
    shared: Arc<Shared>,
}

fn agent(relay: &str, wait: Duration) -> ureq::Agent {
    let mut config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(wait))
        .user_agent(format!("WordCraft/{}", env!("CARGO_PKG_VERSION")));
    if relay.starts_with("http://") {
        // Only ever this computer (`relay_url`): no proxy in between.
        config = config.proxy(None);
    }
    config.build().into()
}

/// One call's status and body, at most [`MAX_FROM_RELAY`] bytes of it.
fn read(result: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<(u16, String), String> {
    let resp = result.map_err(|e| match e {
        ureq::Error::Timeout(_) => "the relay took too long to answer".to_string(),
        ureq::Error::HostNotFound => "the relay's name wasn't found (is the computer online?)".to_string(),
        ureq::Error::ConnectionFailed => "the relay can't be reached (is the computer online?)".to_string(),
        other => format!("the relay can't be reached: {other}"),
    })?;
    let status = resp.status().as_u16();
    let body = resp
        .into_body()
        .into_with_config()
        .limit(MAX_FROM_RELAY)
        .read_to_string()
        .map_err(|e| format!("the relay's answer couldn't be read: {e}"))?;
    Ok((status, body))
}

/// Open (or, with `claim`, open again) a room on the relay with `token`.
fn open_room(agent: &ureq::Agent, relay: &str, token: &str, claim: Option<(&str, &str)>) -> Result<(String, String, String), Status> {
    let body = match claim {
        Some((room, secret)) => json!({"room": room, "secret": secret}),
        None => json!({}),
    };
    let sent = agent
        .post(&format!("{relay}/host/rooms"))
        .header("Authorization", &format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .send(body.to_string().as_bytes());
    let (status, text) = read(sent).map_err(Status::Retrying)?;
    match status {
        200 => {}
        401 => return Err(Status::Refused("the relay doesn't take this token".into())),
        409 => return Err(Status::Refused(TAKEN.into())),
        s => return Err(Status::Retrying(format!("the relay answered {s}: {}", text.trim()))),
    }
    let v: Value = serde_json::from_str(&text).map_err(|_| Status::Retrying("the relay's answer isn't JSON".into()))?;
    let field = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    match (field("room"), field("secret"), field("url")) {
        (Some(r), Some(s), Some(u)) => Ok((r, s, u)),
        _ => Err(Status::Retrying("the relay's answer has no room".into())),
    }
}

impl Hosting {
    /// Open a room on the relay at `relay` with `token`, and answer its agents' requests with
    /// `handler` from now on. `claim` is a room this computer opened before (its id and secret),
    /// to keep its URL. Fails when the relay can't be reached or refuses the token.
    pub fn start(relay: &str, token: &str, claim: Option<(String, String)>, handler: Handler) -> Result<Hosting, String> {
        Hosting::open(relay, token, claim, handler).map_err(|s| match s {
            Status::Retrying(why) | Status::Refused(why) => why,
            other => format!("{other:?}"),
        })
    }

    /// [`Hosting::start`], saying whether trying again may help: [`Status::Retrying`] when the
    /// relay couldn't be reached, [`Status::Refused`] when it refused (the token, the address).
    /// A room claimed that the relay gave to someone else is given up for a new one.
    pub fn open(relay: &str, token: &str, claim: Option<(String, String)>, handler: Handler) -> Result<Hosting, Status> {
        let relay = relay_url(relay).map_err(Status::Refused)?;
        let token = token.trim();
        if token.is_empty() {
            return Err(Status::Refused("the relay needs a token (from whoever runs it)".into()));
        }
        let opener = agent(&relay, Duration::from_secs(30));
        let opened = match open_room(&opener, &relay, token, claim.as_ref().map(|(r, s)| (r.as_str(), s.as_str()))) {
            // Agents' old URL can't come back: a new room, with a new URL.
            Err(Status::Refused(why)) if why == TAKEN => open_room(&opener, &relay, token, None),
            other => other,
        };
        let (room, secret, url) = opened?;
        let shared = Arc::new(Shared {
            relay: relay.clone(),
            token: token.to_string(),
            room,
            secret,
            url,
            agent: agent(&relay, Duration::from_secs(150)),
            stop: AtomicBool::new(false),
            status: Mutex::new(Status::Connected),
            busy: AtomicUsize::new(0),
            handler,
        });
        let hosting = Hosting { shared };
        for _ in 0..POLLERS {
            let s = hosting.shared.clone();
            // Without its threads the room is closed again (dropping `hosting` stops it).
            std::thread::Builder::new()
                .name("relay poll".into())
                .spawn(move || poll_loop(&s))
                .map_err(|e| Status::Retrying(format!("no thread for the relay: {e}")))?;
        }
        Ok(hosting)
    }

    /// The URL agents use: the relay's, for this room (`https://<relay>/r/<room>/mcp`).
    pub fn url(&self) -> &str {
        &self.shared.url
    }

    /// The room's id and secret, to open the same room (and so keep its URL) next time.
    pub fn claim(&self) -> (String, String) {
        (self.shared.room.clone(), self.shared.secret.clone())
    }

    pub fn status(&self) -> Status {
        self.shared.status.lock().map(|s| s.clone()).unwrap_or(Status::Stopped)
    }

    /// Close the room on the relay: agents' requests are refused from now on.
    pub fn stop(&self) {
        if self.shared.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        set(&self.shared, Status::Stopped);
        let s = &self.shared;
        let closer = agent(&s.relay, Duration::from_secs(5));
        let _ = read(closer.delete(&format!("{}/host/r/{}", s.relay, s.room)).header("Authorization", &format!("Bearer {}", s.secret)).call());
    }
}

impl Drop for Hosting {
    fn drop(&mut self) {
        self.stop();
    }
}

fn set(s: &Shared, status: Status) {
    if let Ok(mut st) = s.status.lock()
        && (*st != Status::Stopped || status == Status::Stopped)
    {
        *st = status;
    }
}

/// Wait `d`, or less once the room is stopped.
fn pause(s: &Shared, d: Duration) {
    let step = Duration::from_millis(100);
    let mut left = d;
    while !left.is_zero() && !s.stop.load(Ordering::SeqCst) {
        let now = left.min(step);
        std::thread::sleep(now);
        left = left.saturating_sub(now);
    }
}

/// Ask the relay for requests until the room stops, answering each one.
fn poll_loop(s: &Arc<Shared>) {
    let mut backoff = Duration::from_secs(1);
    while !s.stop.load(Ordering::SeqCst) {
        let url = format!("{}/host/r/{}/poll", s.relay, s.room);
        let polled = read(s.agent.post(&url).header("Authorization", &format!("Bearer {}", s.secret)).send(&b""[..]));
        let failure = match polled {
            Ok((200, text)) => {
                set(s, Status::Connected);
                backoff = Duration::from_secs(1);
                let requests =
                    serde_json::from_str::<Value>(&text).ok().and_then(|v| v.get("requests").and_then(Value::as_array).cloned()).unwrap_or_default();
                let now = Instant::now();
                for r in requests {
                    if let Some((id, req)) = wire::request_from(&r) {
                        // A tenth less than the relay says, for the time its answer took to get here.
                        let until = wire::ttl(&r).map(|t| now + t.saturating_sub((t / 10).min(Duration::from_secs(2))));
                        answer(s, id, req, until);
                    }
                }
                None
            }
            // Stopped meanwhile: the room was closed on purpose.
            _ if s.stop.load(Ordering::SeqCst) => return,
            // The relay forgot the room (it restarted): open it again, with the same URL.
            Ok((404, _)) => match open_room(&s.agent, &s.relay, &s.token, Some((&s.room, &s.secret))) {
                Ok(_) => None,
                Err(Status::Refused(why)) => {
                    set(s, Status::Refused(why));
                    return;
                }
                Err(Status::Retrying(why)) => Some(why),
                Err(_) => Some("the room couldn't be opened again".into()),
            },
            Ok((status, text)) => Some(format!("the relay answered {status}: {}", text.trim())),
            Err(why) => Some(why),
        };
        if let Some(why) = failure {
            if s.stop.load(Ordering::SeqCst) {
                return;
            }
            log::warn!("relay: {why}; trying again in {} s", backoff.as_secs());
            set(s, Status::Retrying(why));
            pause(s, backoff);
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    }
}

/// Answer one request on a thread of its own (or on this one, when [`AT_ONCE`] are running),
/// and send the answer to the relay. A request whose agent has stopped waiting by `until` is
/// dropped unrun: its agent was told it timed out, and may already have sent it again.
fn answer(s: &Arc<Shared>, id: u64, req: Request, until: Option<Instant>) {
    let run = move |s: &Shared| {
        let mut req = req;
        if let Some(u) = until {
            let left = u.saturating_duration_since(Instant::now());
            if left.is_zero() {
                log::debug!("relay: a request waited too long here and wasn't run");
                return;
            }
            // The endpoint waits only what's left, in its own queue too.
            req.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(wordcraft_http::WAIT_HEADER));
            req.headers.push((wordcraft_http::WAIT_HEADER.into(), left.as_millis().to_string()));
        }
        let resp = (s.handler)(&req);
        let body = wire::reply(id, &resp).to_string();
        let url = format!("{}/host/r/{}/reply", s.relay, s.room);
        if let Err(why) = read(
            s.agent
                .post(&url)
                .header("Authorization", &format!("Bearer {}", s.secret))
                .header("Content-Type", "application/json")
                .send(body.as_bytes()),
        ) {
            log::warn!("relay: an answer didn't reach the relay: {why}");
        }
    };
    if s.busy.fetch_add(1, Ordering::SeqCst) >= AT_ONCE {
        run(s);
        s.busy.fetch_sub(1, Ordering::SeqCst);
        return;
    }
    let shared = s.clone();
    let spawned = std::thread::Builder::new().name("relay answer".into()).spawn(move || {
        run(&shared);
        shared.busy.fetch_sub(1, Ordering::SeqCst);
    });
    if spawned.is_err() {
        s.busy.fetch_sub(1, Ordering::SeqCst);
        log::warn!("relay: no thread for an answer");
    }
}

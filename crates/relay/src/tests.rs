use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wordcraft_http::{Request, Response};

use crate::host::{Hosting, Status};
use crate::relay_url;
use crate::server::{Config, MAX_REPLY, MAX_REQUEST, Relay, serve};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// A relay on a loopback port.
fn relay(config: impl FnOnce(&mut Config)) -> (String, Arc<Relay>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let mut c = Config { public_url: base.clone(), tokens: vec![TOKEN.into()], ..Config::default() };
    config(&mut c);
    let relay = Arc::new(Relay::new(c));
    let r = relay.clone();
    std::thread::spawn(move || serve(listener, r, 64));
    (base, relay)
}

/// One request over a fresh connection: status, headers, body.
fn http(url: &str, method: &str, headers: &[(&str, &str)], body: &str) -> (u16, Vec<(String, String)>, String) {
    let rest = url.strip_prefix("http://").unwrap();
    let (host, path) = rest.split_once('/').map_or((rest, "/".to_string()), |(h, p)| (h, format!("/{p}")));
    let mut s = TcpStream::connect(host).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Length: {}\r\n", body.len());
    for (n, v) in headers {
        req.push_str(&format!("{n}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    // A refused body may be cut off before it is all sent; the answer still comes.
    let _ = s.write_all(req.as_bytes());
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
    let mut lines = head.lines();
    let status = lines.next().and_then(|l| l.split(' ').nth(1)).and_then(|s| s.parse().ok()).unwrap_or(0);
    let headers = lines.filter_map(|l| l.split_once(": ")).map(|(n, v)| (n.to_string(), v.to_string())).collect();
    (status, headers, body.to_string())
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

/// The desktop's endpoint, as a stand-in: says what it was sent.
fn echo() -> wordcraft_http::Handler {
    Arc::new(|req: &Request| {
        let names: Vec<&str> = req.headers.iter().map(|(n, _)| n.as_str()).collect();
        let said = json!({"method": req.method, "target": req.target, "auth": req.header("Authorization"), "headers": names, "body": String::from_utf8_lossy(&req.body)});
        Response::json(200, said.to_string()).with_header("X-Internal", "secret").with_header("WWW-Authenticate", "Bearer")
    })
}

fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !ok() {
        assert!(Instant::now() < until, "{what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn relay_addresses_are_https_or_this_computer() {
    assert_eq!(relay_url("https://relay.example.org/").unwrap(), "https://relay.example.org");
    assert!(relay_url("https://relay.example.org/base").is_ok());
    assert!(relay_url("http://127.0.0.1:8790").is_ok());
    assert!(relay_url("http://localhost:8790").is_ok());
    for bad in [
        "http://relay.example.org",
        "ftp://x",
        "https://",
        "relay.example.org",
        "http://127.0.0.1:80@evil.example",
        "https://u@relay.example.org",
        "http://127.0.0.1.evil.example",
        "https://relay.example.org/?x",
    ] {
        assert!(relay_url(bad).is_err(), "{bad}");
    }
}

/// An agent's request goes to the relay's URL and comes back answered from the desktop, which
/// only dialed out; only MCP's headers cross, both ways.
#[test]
fn an_agents_request_reaches_the_desktop_through_the_relay() {
    let (base, _relay) = relay(|_| {});
    let hosting = Hosting::start(&base, TOKEN, None, echo()).unwrap();
    let url = hosting.url().to_string();
    assert!(url.starts_with(&format!("{base}/r/")) && url.ends_with("/mcp"), "{url}");
    let (status, headers, body) = http(
        &format!("{url}?x=1"),
        "POST",
        &[
            ("Authorization", "Bearer invite-key"),
            ("Content-Type", "application/json"),
            ("Mcp-Method", "tools/list"),
            ("Mcp-Param-Region", "eu"),
            ("Cookie", "a=b"),
            ("X-Forwarded-For", "10.0.0.1"),
        ],
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
    );
    assert_eq!(status, 200, "{body}");
    let said: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(said["method"], "POST");
    assert_eq!(said["target"], "/mcp?x=1");
    assert_eq!(said["auth"], "Bearer invite-key", "the invite's key goes to the desktop, which checks it");
    assert!(said["body"].as_str().unwrap().contains("tools/list"));
    let names: Vec<String> = said["headers"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_ascii_lowercase()).collect();
    assert!(names.contains(&"mcp-method".to_string()) && names.contains(&"mcp-param-region".to_string()), "{names:?}");
    for not in ["cookie", "host", "x-forwarded-for", "connection", "content-length"] {
        assert!(!names.contains(&not.to_string()), "{not} crossed: {names:?}");
    }
    assert_eq!(header(&headers, "WWW-Authenticate"), Some("Bearer"));
    assert_eq!(header(&headers, "X-Internal"), None, "only MCP's answer headers come back");
    assert_eq!(hosting.status(), Status::Connected);
    // Several at once.
    let threads: Vec<_> = (0..6)
        .map(|i| {
            let url = url.clone();
            std::thread::spawn(move || http(&url, "POST", &[("Content-Type", "application/json")], &format!("{{\"n\":{i}}}")))
        })
        .collect();
    for (i, t) in threads.into_iter().enumerate() {
        let (status, _, body) = t.join().unwrap();
        assert_eq!(status, 200);
        assert!(body.contains(&format!("{{\\\"n\\\":{i}}}")), "{body}");
    }
    // The person stops sharing: the room is gone at once.
    hosting.stop();
    assert_eq!(hosting.status(), Status::Stopped);
    let (status, _, body) = http(&url, "POST", &[], "{}");
    assert_eq!(status, 404, "{body}");
    assert!(body.contains("isn't shared"), "{body}");
}

#[test]
fn a_desktop_needs_a_token_the_relay_takes() {
    let (base, _relay) = relay(|_| {});
    let e = Hosting::start(&base, "wrong-token-wrong-token-wrong-token", None, echo()).err().unwrap();
    assert!(e.contains("token"), "{e}");
    assert!(Hosting::start(&base, " ", None, echo()).is_err());
    assert!(Hosting::start("http://relay.example.org", TOKEN, None, echo()).err().unwrap().contains("https://"));
    let (status, _, _) = http(&format!("{base}/host/rooms"), "POST", &[], "{}");
    assert_eq!(status, 401);
    let (status, _, _) = http(&format!("{base}/r/{}/mcp", "0".repeat(32)), "POST", &[], "{}");
    assert_eq!(status, 404, "an unknown room");
    let (status, _, _) = http(&format!("{base}/healthz"), "GET", &[], "");
    assert_eq!(status, 200);
}

/// A room whose desktop stopped polling (asleep, offline) answers at once that its host is
/// offline, rather than making the agent wait.
#[test]
fn an_offline_host_is_said_at_once() {
    let (base, _relay) = relay(|c| c.offline_after = Duration::from_millis(200));
    let (status, _, body) = http(&format!("{base}/host/rooms"), "POST", &[("Authorization", &format!("Bearer {TOKEN}"))], "{}");
    assert_eq!(status, 200);
    let room: Value = serde_json::from_str(&body).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let started = Instant::now();
    let (status, headers, body) = http(room["url"].as_str().unwrap(), "POST", &[], "{}");
    assert_eq!(status, 503, "{body}");
    assert!(body.contains("offline") && started.elapsed() < Duration::from_secs(2), "{body}");
    assert_eq!(header(&headers, "Retry-After"), Some("30"));
}

/// The relay forgets the room (here, closed behind the desktop's back; a restart does the
/// same): the desktop opens it again with its id and secret, so its URL keeps working.
#[test]
fn a_forgotten_room_is_opened_again_with_the_same_url() {
    let (base, _relay) = relay(|_| {});
    let hosting = Hosting::start(&base, TOKEN, None, echo()).unwrap();
    let url = hosting.url().to_string();
    let (room, secret) = hosting.claim();
    let (status, _, _) = http(&format!("{base}/host/r/{room}"), "DELETE", &[("Authorization", &format!("Bearer {secret}"))], "");
    assert_eq!(status, 204);
    wait_for("the room comes back", || http(&url, "POST", &[], "{}").0 == 200);
    // Someone else can't take a room by its id without its secret.
    let claim = json!({"room": room, "secret": "f".repeat(64)}).to_string();
    let (status, _, _) = http(&format!("{base}/host/rooms"), "POST", &[("Authorization", &format!("Bearer {TOKEN}"))], &claim);
    assert_eq!(status, 409);
    for junk in [r#"{"room": "short", "secret": "x"}"#, r#"{"room": "00000000000000000000000000000000"}"#, "[]", "not json"] {
        let (status, _, _) = http(&format!("{base}/host/rooms"), "POST", &[("Authorization", &format!("Bearer {TOKEN}"))], junk);
        assert_eq!(status, 400, "{junk}");
    }
    // And a new desktop can open the same room again later with its id and secret.
    hosting.stop();
    let again = Hosting::start(&base, TOKEN, Some((room.clone(), secret.clone())), echo()).unwrap();
    assert_eq!(again.url(), url);
    // A desktop whose room is someone else's now (a copied settings file) gets a new one.
    let other = Hosting::start(&base, TOKEN, Some((room.clone(), "e".repeat(64))), echo()).unwrap();
    assert_ne!(other.url(), url);
    assert_eq!(http(other.url(), "POST", &[], "{}").0, 200);
    // Refusals that trying again won't help say so.
    assert!(matches!(Hosting::open(&base, "wrong-token-wrong-token-wrong-token", None, echo()), Err(Status::Refused(_))));
    assert!(matches!(Hosting::open("http://127.0.0.1:9", TOKEN, None, echo()), Err(Status::Retrying(_))));
}

/// A room opened again (the relay restarted, or it was closed behind the desktop's back) numbers
/// its requests anew: an answer the desktop sends late, for a request of the room before, never
/// answers an agent's request in the room now.
#[test]
fn a_late_answer_never_answers_another_agent_after_the_room_opens_again() {
    let (base, _relay) = relay(|_| {});
    // Answers what it was sent, the first after 1.5 s and the second after 3 s.
    let slow: wordcraft_http::Handler = Arc::new(|req: &Request| {
        let body = String::from_utf8_lossy(&req.body).to_string();
        std::thread::sleep(Duration::from_millis(if body.contains("first") { 1500 } else { 3000 }));
        Response::json(200, body)
    });
    let hosting = Hosting::start(&base, TOKEN, None, slow).unwrap();
    let url = hosting.url().to_string();
    let (room, secret) = hosting.claim();
    let u = url.clone();
    let first = std::thread::spawn(move || http(&u, "POST", &[], r#"{"agent": "first"}"#));
    std::thread::sleep(Duration::from_millis(300));
    // The relay forgets the room while the desktop is still answering the first agent.
    let (status, _, _) = http(&format!("{base}/host/r/{room}"), "DELETE", &[("Authorization", &format!("Bearer {secret}"))], "");
    assert_eq!(status, 204);
    assert_eq!(first.join().unwrap().0, 503, "the first agent hears the room went");
    // The second agent asks as soon as the room is back; its answer comes after the first's late
    // one reached the relay, and it is its own.
    let started = Instant::now();
    loop {
        let (status, _, body) = http(&url, "POST", &[], r#"{"agent": "second"}"#);
        if status == 200 {
            assert_eq!(body, r#"{"agent": "second"}"#);
            break;
        }
        assert!([404, 503].contains(&status) && started.elapsed() < Duration::from_secs(10), "{status} {body}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Two copies of the app with the same saved room (the same settings folder) never share it: the
/// second gets a room of its own, and stopping it leaves the first's working.
#[test]
fn two_desktops_with_the_same_saved_room_get_a_room_each() {
    let (base, _relay) = relay(|_| {});
    let first = Hosting::start(&base, TOKEN, None, echo()).unwrap();
    let second = Hosting::start(&base, TOKEN, Some(first.claim()), echo()).unwrap();
    assert_ne!(second.url(), first.url());
    assert_ne!(second.claim(), first.claim());
    second.stop();
    assert_eq!(http(first.url(), "POST", &[], "{}").0, 200, "the first room still works");
}

/// Agents waiting when the person stops sharing hear so at once.
#[test]
fn waiting_agents_hear_when_sharing_stops() {
    let (base, _relay) = relay(|_| {});
    let slow: wordcraft_http::Handler = Arc::new(|_: &Request| {
        std::thread::sleep(Duration::from_secs(3));
        Response::json(200, "{}")
    });
    let hosting = Hosting::start(&base, TOKEN, None, slow).unwrap();
    let url = hosting.url().to_string();
    let agent = std::thread::spawn(move || {
        let started = Instant::now();
        let r = http(&url, "POST", &[], "{}");
        (r, started.elapsed())
    });
    std::thread::sleep(Duration::from_millis(500));
    hosting.stop();
    let ((status, _, body), took) = agent.join().unwrap();
    assert_eq!(status, 503, "{body}");
    assert!(body.contains("stopped sharing") && took < Duration::from_secs(3), "{body} after {took:?}");
}

#[test]
fn the_relay_holds_back_floods_and_large_bodies() {
    let (base, relay) = relay(|c| {
        c.per_source = (0.5, 2.0);
        c.rooms_per_token = 1;
    });
    let hosting = Hosting::start(&base, TOKEN, None, echo()).unwrap();
    let url = hosting.url().to_string();
    assert_eq!(http(&url, "POST", &[], "{}").0, 200);
    assert_eq!(http(&url, "POST", &[], "{}").0, 200);
    let (status, headers, _) = http(&url, "POST", &[], "{}");
    assert_eq!((status, header(&headers, "Retry-After")), (429, Some("1")), "a third at once from one address");
    // One room per token here.
    let (status, _, _) = http(&format!("{base}/host/rooms"), "POST", &[("Authorization", &format!("Bearer {TOKEN}"))], "{}");
    assert_eq!(status, 429);
    // Large bodies only where an answer goes, and only with the room's secret.
    let (room, secret) = hosting.claim();
    let req = |path: &str, auth: Option<&str>| Request {
        method: "POST".into(),
        target: path.into(),
        headers: auth.map(|a| vec![("Authorization".to_string(), format!("Bearer {a}"))]).unwrap_or_default(),
        body: Vec::new(),
        peer: None,
    };
    assert_eq!(relay.body_limit(&req(&format!("/host/r/{room}/reply"), Some(&secret))), MAX_REPLY);
    assert_eq!(relay.body_limit(&req(&format!("/host/r/{room}/reply"), Some("nope"))), MAX_REQUEST);
    assert_eq!(relay.body_limit(&req(&format!("/host/r/{room}/reply"), None)), MAX_REQUEST);
    assert_eq!(relay.body_limit(&req(&format!("/r/{room}/mcp"), Some(&secret))), MAX_REQUEST);
    let big = "x".repeat(MAX_REQUEST + 1);
    let (status, _, _) = http(&format!("{base}/r/{room}/mcp"), "POST", &[], &big);
    assert_eq!(status, 413);
    // A poll or answer without the room's secret.
    for path in [format!("/host/r/{room}/poll"), format!("/host/r/{room}/reply")] {
        assert_eq!(http(&format!("{base}{path}"), "POST", &[("Authorization", "Bearer nope")], "{}").0, 404, "{path}");
    }
    assert_eq!(http(&format!("{base}/host/r/{room}"), "DELETE", &[], "").0, 404);
}

#[test]
fn hostile_frames_never_panic() {
    let relay = Relay::new(Config { public_url: "http://127.0.0.1:1".into(), tokens: vec![TOKEN.into()], ..Config::default() });
    for target in ["/", "/r", "/r/", "/r//", "/r/x", "/host/r//poll", "/host/r/x/reply", "/host", "/host/rooms/x", "/r/x/mcp/../..", "/healthz?x"] {
        for method in ["GET", "POST", "DELETE", "PUT"] {
            let r = relay.handle(&Request { method: method.into(), target: target.into(), ..Request::default() });
            assert!(r.status >= 200, "{method} {target}");
        }
    }
    for v in [
        json!(null),
        json!({"id": "x"}),
        json!({"id": 1, "status": 99}),
        json!({"id": u64::MAX, "status": 200, "headers": "x"}),
        json!({"id": 1, "status": 70000}),
    ] {
        assert!(crate::wire::response_from(&v).is_none_or(|(_, r)| (200..600).contains(&r.status)), "{v}");
        let _ = crate::wire::request_from(&v);
    }
}

/// The desktop is answering as many requests as it runs at once, so a batch it took waits its
/// turn here. A request whose agent stopped waiting meanwhile (and may be sending it again) is
/// dropped, not run late: a timed-out edit never lands twice.
#[test]
fn a_request_whose_agent_gave_up_is_never_run_late() {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    // A relay as a stand-in, which hands the desktop one batch: 16 requests that keep every
    // answering thread busy, one more that the poller answers itself, and five whose agents wait
    // only 400 ms.
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let room = "0123456789abcdef0123456789abcdef";
    let replied = Arc::new(Mutex::new(Vec::<u64>::new()));
    let sent = Arc::new(AtomicBool::new(false));
    let (b, r, s) = (base.clone(), replied.clone(), sent.clone());
    let fake: wordcraft_http::Handler = Arc::new(move |req: &Request| {
        if req.target == "/host/rooms" {
            return Response::json(200, json!({"room": room, "secret": "s3cret", "url": format!("{b}/r/{room}/mcp")}).to_string());
        }
        if req.target.ends_with("/poll") {
            if s.swap(true, Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(100));
                return Response::json(200, json!({"requests": []}).to_string());
            }
            let one =
                |id: u64, body: &str, ttl: u64| json!({"id": id, "method": "POST", "target": "/mcp", "headers": [], "body": body, "ttlMs": ttl});
            let mut batch: Vec<Value> = (1..=16).map(|id| one(id, "hold", 10_000)).collect();
            batch.push(one(17, "slow", 10_000));
            batch.extend((18..=22).map(|id| one(id, "quick", 400)));
            return Response::json(200, json!({"requests": batch}).to_string());
        }
        if req.target.ends_with("/reply") {
            if let Some(id) = serde_json::from_slice::<Value>(&req.body).ok().and_then(|v| v["id"].as_u64()) {
                r.lock().unwrap().push(id);
            }
            return Response::new(204);
        }
        Response::new(204)
    });
    std::thread::spawn(move || wordcraft_http::serve(listener, wordcraft_http::Config::default(), fake));
    let ran = Arc::new(Mutex::new(Vec::<String>::new()));
    let r = ran.clone();
    let desktop: wordcraft_http::Handler = Arc::new(move |req: &Request| {
        let body = String::from_utf8_lossy(&req.body).to_string();
        r.lock().unwrap().push(body.clone());
        std::thread::sleep(Duration::from_millis(match body.as_str() {
            "hold" => 1500,
            "slow" => 800,
            _ => 0,
        }));
        Response::json(200, "{}")
    });
    let hosting = Hosting::start(&base, TOKEN, None, desktop).unwrap();
    wait_for("the busy requests are answered", || (1..=17).all(|id| replied.lock().unwrap().contains(&id)));
    std::thread::sleep(Duration::from_millis(300));
    let ran = ran.lock().unwrap().clone();
    assert!(!ran.iter().any(|b| b == "quick"), "ran after their agents gave up: {ran:?}");
    assert_eq!(ran.len(), 17, "{ran:?}");
    assert!(!replied.lock().unwrap().iter().any(|id| *id > 17));
    drop(hosting);
}

/// A room's id comes from its secret: once the relay has forgotten a room (it restarted, or the
/// room was closed), a token holder who knows its URL still can't open it with a secret of their
/// own and take its agents' requests. Its own desktop can.
#[test]
fn only_a_rooms_own_secret_opens_it_again_after_the_relay_forgot_it() {
    let (base, _relay) = relay(|_| {});
    let auth = format!("Bearer {TOKEN}");
    let (status, _, body) = http(&format!("{base}/host/rooms"), "POST", &[("Authorization", &auth)], "{}");
    assert_eq!(status, 200, "{body}");
    let opened: Value = serde_json::from_str(&body).unwrap();
    let (room, secret) = (opened["room"].as_str().unwrap().to_string(), opened["secret"].as_str().unwrap().to_string());
    assert_eq!(room, crate::room_of(&secret));
    let (status, _, _) = http(&format!("{base}/host/r/{room}"), "DELETE", &[("Authorization", &format!("Bearer {secret}"))], "");
    assert_eq!(status, 204);
    let theirs = json!({"room": room, "secret": "a".repeat(64)}).to_string();
    let (status, _, body) = http(&format!("{base}/host/rooms"), "POST", &[("Authorization", &auth)], &theirs);
    assert_eq!(status, 409, "{body}");
    let own = json!({"room": room, "secret": secret}).to_string();
    let (status, _, body) = http(&format!("{base}/host/rooms"), "POST", &[("Authorization", &auth)], &own);
    assert_eq!(status, 200, "{body}");
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["url"], opened["url"]);
}

/// One room's desktop can't hold the relay's connections by asking for requests again and again
/// at once: past a few waiting polls, more are refused.
#[test]
fn a_rooms_waiting_polls_are_capped() {
    let (base, _relay) = relay(|c| c.poll_wait = Duration::from_millis(1500));
    let (status, _, body) = http(&format!("{base}/host/rooms"), "POST", &[("Authorization", &format!("Bearer {TOKEN}"))], "{}");
    assert_eq!(status, 200, "{body}");
    let opened: Value = serde_json::from_str(&body).unwrap();
    let (room, secret) = (opened["room"].as_str().unwrap().to_string(), opened["secret"].as_str().unwrap().to_string());
    let polls: Vec<_> = (0..8)
        .map(|_| {
            let (url, auth) = (format!("{base}/host/r/{room}/poll"), format!("Bearer {secret}"));
            std::thread::spawn(move || http(&url, "POST", &[("Authorization", &auth)], "").0)
        })
        .collect();
    let mut statuses: Vec<u16> = polls.into_iter().map(|p| p.join().unwrap()).collect();
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 200, 200, 200, 429, 429, 429, 429]);
}

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use wordcraft_http::{Request, Response};

use super::{Endpoint, HEADER_MISMATCH, Options};
use crate::backend::Backend;
use crate::server::{STATELESS_VERSION, UNSUPPORTED_VERSION};

/// What the stand-in app knows: which keys it takes back, and every call it got.
#[derive(Default)]
struct App {
    revoked: Mutex<Vec<String>>,
    calls: Mutex<Vec<(String, String)>>,
    opened: AtomicUsize,
    /// How long each call other than `auth` takes, in milliseconds.
    slow_ms: AtomicU64,
}

/// A connection to the stand-in app with one key: `window` is the window's key, `stranger` is a
/// key it never handed out, anything else an invite's.
struct Fake {
    key: String,
    app: Arc<App>,
    refused: AtomicBool,
}

impl Backend for Fake {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.app.calls.lock().unwrap().push((self.key.clone(), method.to_string()));
        if method != "auth" {
            std::thread::sleep(Duration::from_millis(self.app.slow_ms.load(Ordering::SeqCst)));
        }
        if self.key == "stranger" || self.app.revoked.lock().unwrap().contains(&self.key) {
            self.refused.store(true, Ordering::SeqCst);
            return Err("unauthorized: refused".into());
        }
        match method {
            "auth" if self.key == "window" => Ok(json!({"as": "window"})),
            "auth" if self.key == "local" => Ok(json!({"as": "participant", "name": self.key, "role": "owner", "remote": false})),
            "auth" => Ok(json!({"as": "participant", "name": self.key, "role": "suggester", "remote": true})),
            "engine.execute" => Ok(json!({"ran": params["command"], "by": self.key})),
            "document.inspect" => Ok(json!({"text": "one two three"})),
            _ => Ok(json!({})),
        }
    }
    fn has_ui(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        "a stand-in app".into()
    }
    fn ended(&self) -> Option<String> {
        self.refused.load(Ordering::SeqCst).then(|| "refused".to_string())
    }
}

fn endpoint(options: Options) -> (Endpoint, Arc<App>) {
    let app = Arc::new(App::default());
    let a = app.clone();
    let options = Options { hosts: Some(vec!["127.0.0.1:7990".into()]), ..options };
    let e = Endpoint::new(
        options,
        Box::new(move |key| {
            a.opened.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Fake { key: key.to_string(), app: a.clone(), refused: AtomicBool::new(false) }) as Box<dyn Backend + Send>)
        }),
    );
    (e, app)
}

/// A POST to /mcp from an MCP client holding `key`, with `body` and extra `headers`.
fn post(key: Option<&str>, body: &Value, headers: &[(&str, &str)]) -> Request {
    let mut h = vec![("Host".to_string(), "127.0.0.1:7990".to_string()), ("Content-Type".to_string(), "application/json".to_string())];
    if let Some(k) = key {
        h.push(("Authorization".into(), format!("Bearer {k}")));
    }
    h.extend(headers.iter().map(|(n, v)| (n.to_string(), v.to_string())));
    Request { method: "POST".into(), target: "/mcp".into(), headers: h, body: body.to_string().into_bytes(), peer: None }
}

/// A 2026-07-28 request: `_meta` names the version, and the headers say what the body does.
fn stateless(key: &str, id: u64, method: &str, mut params: Value) -> Request {
    params["_meta"] = json!({"io.modelcontextprotocol/protocolVersion": STATELESS_VERSION, "io.modelcontextprotocol/clientCapabilities": {}});
    let name = params.get("name").or_else(|| params.get("uri")).and_then(Value::as_str).map(str::to_string);
    let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let mut headers = vec![("MCP-Protocol-Version", STATELESS_VERSION), ("Mcp-Method", method)];
    if let Some(n) = name.as_deref() {
        headers.push(("Mcp-Name", n));
    }
    post(Some(key), &body, &headers)
}

fn body(r: &Response) -> Value {
    serde_json::from_slice(&r.body).unwrap_or(Value::Null)
}

#[test]
fn a_2026_07_28_client_discovers_lists_and_calls_with_no_handshake() {
    let (e, _app) = endpoint(Options::default());
    let r = e.handle(&stateless("k1", 1, "server/discover", json!({})));
    assert_eq!(r.status, 200, "{:?}", body(&r));
    let d = body(&r)["result"].clone();
    assert_eq!(d["resultType"], "complete");
    assert!(d["supportedVersions"].as_array().unwrap().iter().any(|v| v == STATELESS_VERSION));
    assert!(d["supportedVersions"].as_array().unwrap().iter().any(|v| v == "2025-06-18"));
    assert!(d["capabilities"]["tools"].is_object() && d["ttlMs"].is_u64() && d["cacheScope"].is_string());
    assert_eq!(d["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "wordcraft");
    assert_eq!(r.header("Mcp-Session-Id"), None, "no sessions");

    let list = body(&e.handle(&stateless("k1", 2, "tools/list", json!({}))))["result"].clone();
    assert!(list["tools"].as_array().is_some_and(|t| !t.is_empty()));
    assert_eq!((list["resultType"].clone(), list["cacheScope"].clone()), (json!("complete"), json!("public")));

    let call =
        e.handle(&stateless("k1", 3, "tools/call", json!({"name": "execute", "arguments": {"command": "text.insert", "params": {"text": "Hi"}}})));
    assert_eq!(call.status, 200);
    let res = body(&call)["result"].clone();
    assert_eq!(res["resultType"], "complete");
    assert_eq!(res["isError"], false, "{res}");
    assert!(res["content"][0]["text"].as_str().unwrap().contains("k1"), "ran with k1's connection: {res}");

    let read = body(&e.handle(&stateless("k1", 4, "resources/read", json!({"uri": "wordcraft://document"}))))["result"].clone();
    assert_eq!((read["ttlMs"].clone(), read["cacheScope"].clone()), (json!(0), json!("private")), "the document is never cached");
    // An unknown method is a 404 with a JSON-RPC body, an unknown resource -32602.
    let r = e.handle(&stateless("k1", 5, "ping", json!({})));
    assert_eq!((r.status, body(&r)["error"]["code"].clone()), (404, json!(-32601)));
    let r = e.handle(&stateless("k1", 6, "resources/read", json!({"uri": "wordcraft://nothing"})));
    assert_eq!((r.status, body(&r)["error"]["code"].clone()), (200, json!(-32602)));
}

#[test]
fn a_2026_07_28_request_whose_headers_dont_match_is_refused() {
    let (e, app) = endpoint(Options::default());
    let good = stateless("k1", 1, "tools/call", json!({"name": "execute", "arguments": {}}));
    let without = |name: &str| {
        let mut r = good.clone();
        r.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        r
    };
    let with = |name: &str, value: &str| {
        let mut r = without(name);
        r.headers.push((name.to_string(), value.to_string()));
        r
    };
    for (r, what) in [
        (without("MCP-Protocol-Version"), "MCP-Protocol-Version"),
        (with("MCP-Protocol-Version", "2025-06-18"), "MCP-Protocol-Version"),
        (without("Mcp-Method"), "Mcp-Method"),
        (with("mcp-method", "tools/list"), "Mcp-Method"),
        (without("Mcp-Name"), "Mcp-Name"),
        (with("Mcp-Name", "type_text"), "Mcp-Name"),
    ] {
        let resp = e.handle(&r);
        assert_eq!(resp.status, 400, "{what}");
        let b = body(&resp);
        assert_eq!(b["error"]["code"], HEADER_MISMATCH, "{what}");
        assert!(b["error"]["message"].as_str().unwrap().contains(what), "{b}");
    }
    assert!(app.calls.lock().unwrap().is_empty(), "nothing reached the app");
    // The base64 form of a header value counts as its text.
    assert_eq!(e.handle(&with("Mcp-Name", "=?base64?ZXhlY3V0ZQ==?=")).status, 200);
    // A version the server doesn't speak: 400, with the versions it does.
    let mut r = stateless("k1", 2, "tools/list", json!({}));
    let mut b: Value = serde_json::from_slice(&r.body).unwrap();
    b["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!("2099-01-01");
    r.body = b.to_string().into_bytes();
    r.headers.retain(|(n, _)| n != "MCP-Protocol-Version");
    r.headers.push(("MCP-Protocol-Version".into(), "2099-01-01".into()));
    let resp = e.handle(&r);
    assert_eq!((resp.status, body(&resp)["error"]["code"].clone()), (400, json!(UNSUPPORTED_VERSION)));
    assert_eq!(body(&resp)["error"]["data"]["requested"], "2099-01-01");
    assert!(body(&resp)["error"]["data"]["supported"].as_array().unwrap().iter().any(|v| v == STATELESS_VERSION));
}

#[test]
fn a_2025_06_18_client_initializes_and_gets_no_session() {
    let (e, _app) = endpoint(Options::default());
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}});
    let r = e.handle(&post(Some("k1"), &init, &[]));
    assert_eq!(r.status, 200);
    assert_eq!(body(&r)["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(r.header("Mcp-Session-Id"), None, "sessions are optional, and WordCraft keeps none");
    let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    let r = e.handle(&post(Some("k1"), &note, &[("MCP-Protocol-Version", "2025-06-18")]));
    assert_eq!((r.status, r.body.len()), (202, 0));
    // A session id a client sends anyway is ignored.
    let call = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "inspect_document", "arguments": {}}});
    let r = e.handle(&post(Some("k1"), &call, &[("MCP-Protocol-Version", "2025-06-18"), ("Mcp-Session-Id", "whatever")]));
    assert_eq!(r.status, 200);
    assert_eq!(body(&r)["result"]["isError"], false, "{:?}", body(&r));
    assert!(body(&r)["result"].get("resultType").is_none(), "older clients get older results");
    // An older request with a version header the server doesn't know is refused.
    let r = e.handle(&post(Some("k1"), &call, &[("MCP-Protocol-Version", "1999-01-01")]));
    assert_eq!(r.status, 400);
    // A 2026-07-28 header on a body without its `_meta` doesn't pass as one.
    let r = e.handle(&post(Some("k1"), &call, &[("MCP-Protocol-Version", STATELESS_VERSION)]));
    assert_eq!(r.status, 400);
}

#[test]
fn requests_without_an_invites_key_or_from_elsewhere_are_refused() {
    let (e, app) = endpoint(Options::default());
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
    let r = e.handle(&post(None, &list, &[]));
    assert_eq!(r.status, 401);
    assert!(r.header("WWW-Authenticate").is_some_and(|h| h.starts_with("Bearer")));
    let r = e.handle(&post(Some("window"), &list, &[]));
    assert_eq!(r.status, 403, "the window's key acts as the person: {:?}", body(&r));
    // An invite for an agent on this computer may be the owner and use this computer's files:
    // it isn't one to hand to an agent elsewhere.
    let r = e.handle(&post(Some("local"), &list, &[]));
    assert_eq!(r.status, 403, "{:?}", body(&r));
    assert!(body(&r).to_string().contains("remote: true"), "{:?}", body(&r));
    let r = e.handle(&post(Some("stranger"), &list, &[]));
    assert_eq!(r.status, 401);
    assert_eq!(e.keys(), 0, "no connection kept for refused keys");
    // A web page (DNS rebinding, or a page posting to loopback).
    assert_eq!(e.handle(&post(Some("k1"), &list, &[("Origin", "https://evil.example")])).status, 403);
    let mut r = post(Some("k1"), &list, &[]);
    r.headers.retain(|(n, _)| n != "Host");
    r.headers.push(("Host".into(), "evil.example:7990".into()));
    assert_eq!(e.handle(&r).status, 403, "another Host");
    r.headers.retain(|(n, _)| n != "Host");
    assert_eq!(e.handle(&r).status, 403, "no Host");
    for method in ["GET", "DELETE", "PUT"] {
        let mut r = post(Some("k1"), &list, &[]);
        r.method = method.into();
        let resp = e.handle(&r);
        assert_eq!((resp.status, resp.header("Allow")), (405, Some("POST")), "{method}");
    }
    let mut r = post(Some("k1"), &list, &[]);
    r.target = "/other".into();
    assert_eq!(e.handle(&r).status, 404);
    let mut r = post(Some("k1"), &list, &[]);
    r.headers.retain(|(n, _)| n != "Content-Type");
    r.headers.push(("Content-Type".into(), "text/plain".into()));
    assert_eq!(e.handle(&r).status, 415);
    let mut r = post(Some("k1"), &list, &[]);
    r.body = b"{not json".to_vec();
    assert_eq!(e.handle(&r).status, 400);
    r.body = vec![0xff, 0xfe];
    assert_eq!(e.handle(&r).status, 400);
    r.body = vec![b' '; crate::server::MAX_REQUEST_LINE + 1];
    assert_eq!(e.handle(&r).status, 413);
    let calls = app.calls.lock().unwrap();
    assert!(
        calls.iter().all(|(k, m)| m == "auth" && (k == "window" || k == "stranger" || k == "local")),
        "only the key checks reached the app: {calls:?}"
    );
}

#[test]
fn each_key_keeps_one_connection_until_it_is_taken_back_or_idles_out() {
    let (e, app) = endpoint(Options { max_keys: 2, idle: Duration::from_millis(300), ..Options::default() });
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "inspect_document", "arguments": {}}});
    for _ in 0..3 {
        assert_eq!(e.handle(&post(Some("ana"), &list, &[])).status, 200);
    }
    assert_eq!(app.opened.load(Ordering::SeqCst), 1, "one connection for ana");
    assert_eq!(e.handle(&post(Some("ben"), &list, &[])).status, 200);
    assert_eq!((app.opened.load(Ordering::SeqCst), e.keys()), (2, 2));
    let r = e.handle(&post(Some("cy"), &list, &[]));
    assert_eq!(r.status, 503, "two keys at most");
    // The owner takes ana's invite back: her request gets the app's refusal, and her connection
    // goes; the next one is refused like any unknown key.
    app.revoked.lock().unwrap().push("ana".into());
    let r = e.handle(&post(Some("ana"), &list, &[]));
    assert_eq!(r.status, 401, "{:?}", body(&r));
    assert_eq!(e.keys(), 1);
    assert_eq!(e.handle(&post(Some("ana"), &list, &[])).status, 401);
    // Ben idles out: his place goes to someone else, and he gets a new connection next time.
    std::thread::sleep(Duration::from_millis(450));
    assert_eq!(e.handle(&post(Some("cy"), &list, &[])).status, 200);
    assert_eq!(e.keys(), 1, "only cy now");
    assert_eq!(e.handle(&post(Some("ben"), &list, &[])).status, 200);
    assert_eq!(app.opened.load(Ordering::SeqCst), 5, "ana twice, ben twice, cy once: {}", app.opened.load(Ordering::SeqCst));
}

#[test]
fn hostile_bodies_never_panic() {
    let (e, _app) = endpoint(Options::default());
    for b in [
        json!(null),
        json!([]),
        json!([1, 2]),
        json!({"jsonrpc": "2.0", "id": {"x": 1}, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": 1, "method": 5}),
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": 7}}}),
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"_meta": "x"}}),
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": 5, "_meta": {"io.modelcontextprotocol/protocolVersion": STATELESS_VERSION}}}),
    ] {
        let r = e.handle(&post(Some("k1"), &b, &[("Mcp-Method", "tools/call"), ("MCP-Protocol-Version", STATELESS_VERSION)]));
        assert!(r.status >= 200, "{b}");
    }
}

/// A batch would run every member and keep every answer: over HTTP, it's refused before it
/// reaches the app, and so is anything else that isn't one message.
#[test]
fn batches_are_refused_and_never_reach_the_app() {
    let (e, app) = endpoint(Options::default());
    let call = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "inspect_document", "arguments": {}}});
    assert_eq!(e.handle(&post(Some("ana"), &call, &[])).status, 200);
    let runs = |app: &App| app.calls.lock().unwrap().iter().filter(|(_, m)| m != "auth").count();
    let before = runs(&app);
    let batch = Value::Array(vec![call.clone(); 50]);
    for b in [batch, json!([]), json!(null), json!(5), json!("tools/call")] {
        let r = e.handle(&post(Some("ana"), &b, &[]));
        assert_eq!(r.status, 400, "{b}");
        assert_eq!(body(&r)["error"]["code"], -32600, "{b}");
    }
    assert_eq!(runs(&app), before, "nothing ran");
}

/// An agent sending faster than the app answers: a few requests wait their turn, the rest are
/// told to try again at once, and a request whose caller was told it timed out never runs later.
#[test]
fn a_flood_on_one_key_is_bounded_and_late_requests_never_run() {
    let (e, app) = endpoint(Options { timeout: Duration::from_millis(150), ..Options::default() });
    let call = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "inspect_document", "arguments": {}}});
    assert_eq!(e.handle(&post(Some("ana"), &call, &[])).status, 200);
    app.slow_ms.store(400, Ordering::SeqCst);
    let e = Arc::new(e);
    let runs = |app: &App| app.calls.lock().unwrap().iter().filter(|(_, m)| m != "auth").count();
    let before = runs(&app);
    let floods: Vec<_> = (0..30)
        .map(|_| {
            let (e, call) = (e.clone(), call.clone());
            std::thread::spawn(move || e.handle(&post(Some("ana"), &call, &[])).status)
        })
        .collect();
    let statuses: Vec<u16> = floods.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(statuses.iter().all(|s| [503, 504].contains(s)), "{statuses:?}");
    assert!(statuses.iter().filter(|s| **s == 503).count() >= 30 - super::QUEUE - 1, "{statuses:?}");
    // Every caller has its answer; wait for what was begun to finish, then check nothing late ran.
    std::thread::sleep(Duration::from_millis(1200));
    assert!(runs(&app) - before <= 2, "{} ran after their callers stopped waiting", runs(&app) - before);
}

/// Requests the endpoint answers without the app (tools/list) still don't keep a key taken
/// back: every so often its connection asks the app again, and once the app says no, the key
/// gets 401 and its place goes.
#[test]
fn a_key_taken_back_loses_its_place_even_asking_only_for_the_tool_list() {
    let (e, app) = endpoint(Options { recheck: Duration::from_millis(200), ..Options::default() });
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}});
    assert_eq!(e.handle(&post(Some("ana"), &list, &[])).status, 200);
    assert_eq!(e.keys(), 1);
    app.revoked.lock().unwrap().push("ana".into());
    std::thread::sleep(Duration::from_millis(300));
    let r = e.handle(&post(Some("ana"), &list, &[]));
    assert_eq!(r.status, 401, "{:?}", body(&r));
    assert_eq!(e.keys(), 0, "its place went");
}

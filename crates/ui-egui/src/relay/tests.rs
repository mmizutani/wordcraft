use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wordcraft_doc::Document;
use wordcraft_engine::Session;
use wordcraft_engine::room::Lobby;

use crate::control::{self, ControlRequest, Outcome};
use crate::{Services, WordApp};

const TOKEN: &str = "relay-token-for-tests-0123456789abcdef";

/// A window showing "one two three" by Minoru, whose lobby is `lobby`, keeping its settings in
/// `dir` when given.
fn window(lobby: &Lobby, dir: Option<&std::path::Path>) -> WordApp {
    let services = Services { settings_dir: dir.map(std::path::Path::to_path_buf), ..Default::default() };
    let mut a = WordApp::new(Session::new(Document::from_text("one two three")), services).with_lobby(lobby.clone());
    a.session.author = "Minoru".into();
    a
}

fn call(a: &mut WordApp, method: &str, params: Value) -> Value {
    let (req, _reply) = ControlRequest::new(method, params);
    match control::handle(a, &egui::Context::default(), &req) {
        Outcome::Done(v) => v,
        Outcome::Screenshot { .. } => panic!("{method} took a screenshot"),
    }
}

fn result(a: &mut WordApp, method: &str, params: Value) -> Value {
    let v = call(a, method, params);
    assert_eq!(v["ok"], true, "{method}: {v}");
    v["result"].clone()
}

fn error(a: &mut WordApp, method: &str, params: Value) -> String {
    let v = call(a, method, params);
    v["error"].as_str().unwrap_or_else(|| panic!("{method} worked: {v}")).to_string()
}

/// A relay on this computer: its address.
fn relay() -> String {
    relay_on(TcpListener::bind(("127.0.0.1", 0)).unwrap())
}

fn relay_on(listener: TcpListener) -> String {
    use wordcraft_relay::server::{Config, Relay, serve};
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let relay = Arc::new(Relay::new(Config { public_url: base.clone(), tokens: vec![TOKEN.into()], ..Config::default() }));
    std::thread::spawn(move || serve(listener, relay, 64));
    base
}

/// An MCP client's 2026-07-28 tool call to `url` with `key`: the HTTP status and the reply.
fn tool(url: &str, key: &str, name: &str, arguments: Value) -> (u16, Value) {
    let rest = url.strip_prefix("http://").unwrap();
    let (host, path) = rest.split_at(rest.find('/').unwrap());
    let meta = json!({"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}});
    let body =
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": arguments, "_meta": meta}}).to_string();
    let mut s = TcpStream::connect(host).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    write!(
        s,
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {key}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2026-07-28\r\nMcp-Method: tools/call\r\nMcp-Name: {name}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    let status = out.get(9..12).and_then(|c| c.parse().ok()).unwrap_or(0);
    let reply = out.split_once("\r\n\r\n").and_then(|(_, b)| serde_json::from_str(b).ok()).unwrap_or(Value::Null);
    (status, reply)
}

/// Wait until `room.participants` says the relay is `status`: its state then.
fn relay_becomes(a: &mut WordApp, status: &str) -> Value {
    let started = Instant::now();
    loop {
        let state = result(a, "room.participants", json!({}));
        if state["relay"]["status"] == status {
            return state["relay"].clone();
        }
        assert!(started.elapsed() < Duration::from_secs(15), "the relay is {}", state["relay"]);
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// An agent on another computer (#76): the person sets up a relay, shares the document and
/// invites the agent; the invite gives the agent the relay's URL and a configuration with its
/// key, and its edits arrive as tracked changes in its name. Stopping sharing closes the room on
/// the relay, and sharing again opens the same room: the agent's URL keeps working.
#[test]
fn an_agent_on_another_computer_edits_the_shared_document_through_the_relay() {
    let dir = std::env::temp_dir().join(format!("wordcraft-relay-window-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let lobby = Lobby::default();
    let _port = wordcraft_assist::acp::RoomPort::open(&lobby).unwrap();
    let base = relay();
    let mut a = window(&lobby, Some(&dir));
    let set = result(&mut a, "room.relay", json!({"relay": base, "token": TOKEN}));
    assert_eq!((set["relay"].as_str(), set["status"].as_str(), &set["url"]), (Some(base.as_str()), Some("waiting"), &Value::Null), "{set}");
    result(&mut a, "room.share", json!({}));
    let up = relay_becomes(&mut a, "connected");
    let url = up["url"].as_str().unwrap().to_string();
    assert!(url.starts_with(&format!("{base}/r/")) && url.ends_with("/mcp"), "{url}");
    // The settings are the person's only, and keep the room for next time.
    let path = dir.join(super::SETTINGS_FILE);
    let started = Instant::now();
    while !std::fs::read_to_string(&path).unwrap_or_default().contains("secret") {
        assert!(started.elapsed() < Duration::from_secs(5), "the room isn't kept");
        std::thread::sleep(Duration::from_millis(10));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    let invite = result(&mut a, "room.invite", json!({"name": "Sandbox", "remote": true}));
    let key = invite["key"].as_str().unwrap().to_string();
    assert_eq!(invite["url"], json!(url));
    let server = &invite["mcpConfig"]["mcpServers"]["wordcraft"];
    assert_eq!((server["url"].as_str(), server["headers"]["Authorization"].as_str()), (Some(url.as_str()), Some(format!("Bearer {key}").as_str())));
    // A local invite has no URL: it joins on this computer.
    let local = result(&mut a, "room.invite", json!({"name": "Here"}));
    assert!(local.get("url").is_none(), "{local}");

    let (status, r) = tool(&url, &key, "select_text", json!({"text": "two"}));
    assert_eq!((status, &r["result"]["isError"]), (200, &json!(false)), "{r}");
    let (status, r) = tool(&url, &key, "type_text", json!({"text": "2"}));
    assert_eq!((status, &r["result"]["isError"]), (200, &json!(false)), "{r}");
    // The window's own key never works from elsewhere, and neither does a made-up one.
    assert_eq!(tool(&url, "not-a-key", "select_text", json!({"text": "two"})).0, 401);
    let started = Instant::now();
    while !a.session.doc.revisions.iter().any(|r| r.author == "Sandbox") {
        assert!(started.elapsed() < Duration::from_secs(5), "no tracked change in the agent's name");
        a.catch_up_share();
        std::thread::sleep(Duration::from_millis(5));
    }

    // Stop sharing: the room closes on the relay too.
    result(&mut a, "room.stop", json!({}));
    assert_eq!(result(&mut a, "room.relay", json!({}))["status"], "waiting");
    let started = Instant::now();
    loop {
        let (status, _) = tool(&url, &key, "select_text", json!({"text": "one"}));
        if status != 200 {
            assert!([404, 503].contains(&status), "{status}");
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(10), "the room stays open on the relay");
        std::thread::sleep(Duration::from_millis(20));
    }
    // Sharing again opens the same room: the same URL.
    result(&mut a, "room.share", json!({}));
    assert_eq!(relay_becomes(&mut a, "connected")["url"], json!(url));
    result(&mut a, "room.stop", json!({}));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `room.relay` checks what it's given, keeps the token for the same relay, and forgets the relay
/// when turned off; without a relay a remote invite says it has no URL yet.
#[test]
fn the_relay_setting_is_checked_kept_and_turned_off() {
    let dir = std::env::temp_dir().join(format!("wordcraft-relay-setting-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let lobby = Lobby::default();
    let mut a = window(&lobby, Some(&dir));
    assert_eq!(result(&mut a, "room.relay", json!({})), json!({"relay": null, "status": "off", "url": null}));
    for (params, says) in [
        (json!({"relay": "http://relay.example.org", "token": "t"}), "isn't a relay's address"),
        (json!({"relay": "https://user@relay.example.org", "token": "t"}), "isn't a relay's address"),
        (json!({"relay": 5, "token": "t"}), "`relay`"),
        (json!({"token": "t"}), "`relay`"),
        (json!({"relay": "https://relay.example.org"}), "missing `token`"),
        (json!({"relay": "https://relay.example.org", "token": " "}), "`token`"),
        (json!({"relay": "https://relay.example.org", "token": ["t"]}), "`token`"),
    ] {
        let e = error(&mut a, "room.relay", params.clone());
        assert!(e.contains(says), "{params}: {e}");
    }
    assert!(!dir.join(super::SETTINGS_FILE).exists(), "nothing kept from a refusal");
    let set = result(&mut a, "room.relay", json!({"relay": "https://relay.example.org/", "token": "secret-token"}));
    assert_eq!((set["relay"].as_str(), set["status"].as_str()), (Some("https://relay.example.org"), Some("waiting")));
    // The same relay again keeps its token; another window reads the same setting.
    result(&mut a, "room.relay", json!({"relay": "https://relay.example.org"}));
    let mut b = window(&lobby, Some(&dir));
    assert_eq!(result(&mut b, "room.relay", json!({}))["relay"], "https://relay.example.org");
    assert!(std::fs::read_to_string(dir.join(super::SETTINGS_FILE)).unwrap().contains("secret-token"));
    // Off: forgotten.
    assert_eq!(result(&mut a, "room.relay", json!({"off": true}))["status"], "off");
    assert!(!dir.join(super::SETTINGS_FILE).exists());
    // Without a relay, a remote invite has a key but no URL yet.
    result(&mut a, "room.share", json!({}));
    let invite = result(&mut a, "room.invite", json!({"name": "Sandbox", "remote": true}));
    assert_eq!((invite["key"].is_string(), &invite["url"], &invite["mcpConfig"]), (true, &Value::Null, &Value::Null));
    result(&mut a, "room.stop", json!({}));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A relay that can't be reached when the document is shared (the computer is offline, the
/// relay is restarting) is tried again until it can: the agents' URL then works.
#[test]
fn a_relay_out_of_reach_is_tried_again() {
    let lobby = Lobby::default();
    let _port = wordcraft_assist::acp::RoomPort::open(&lobby).unwrap();
    // A port nothing listens on yet.
    let port = TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port();
    let mut a = window(&lobby, None);
    result(&mut a, "room.relay", json!({"relay": format!("http://127.0.0.1:{port}"), "token": TOKEN}));
    result(&mut a, "room.share", json!({}));
    let retrying = relay_becomes(&mut a, "retrying");
    assert!(retrying["why"].as_str().is_some_and(|w| w.contains("reached")), "{retrying}");
    relay_on(TcpListener::bind(("127.0.0.1", port)).unwrap());
    let up = relay_becomes(&mut a, "connected");
    assert!(up["url"].as_str().is_some_and(|u| u.starts_with(&format!("http://127.0.0.1:{port}/r/"))), "{up}");
    // A token the relay refuses isn't tried again.
    result(&mut a, "room.relay", json!({"relay": format!("http://127.0.0.1:{port}"), "token": "not-this-relay-s-token-not-this-one"}));
    let refused = relay_becomes(&mut a, "failed");
    assert!(refused["why"].as_str().is_some_and(|w| w.contains("token")), "{refused}");
    result(&mut a, "room.stop", json!({}));
}

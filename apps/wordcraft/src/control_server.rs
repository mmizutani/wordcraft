//! Loopback JSON-lines control server: one request per line, one reply per line.
//! This is the transport the MCP server (`wordcraft-cli mcp --connect`) wraps.
//!
//! Every request needs the window's key (`docs/control-protocol.md`, Keys): once the port is
//! bound, the app writes a random key to `<settings>/control-key.<instance>` (mode 0600) and
//! removes the file when the window closes. A connection whose first request carries the key of
//! an invite to the document the window shares (`room.invite`) gets its holder's own copy of it
//! instead, which answers the rest of the connection on its thread
//! (`wordcraft_engine::room::hosted`); the window never sees those requests.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wordcraft_engine::room::{Lobby, Replica, hosted};
use wordcraft_ui_egui::ControlRequest;

/// Longer request lines are refused (and the connection closed).
const MAX_LINE: usize = 1 << 20;

/// A connection that sends no authorised request in this time is closed.
const FIRST_REQUEST: Duration = Duration::from_secs(30);

/// The key file of a running control server; [`KeyFile::remove`] it when the window closes.
pub struct KeyFile(PathBuf);

impl KeyFile {
    pub fn path(&self) -> &Path {
        &self.0
    }
    pub fn remove(&self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Bind `127.0.0.1:port`, write the key file into `settings` and serve; participants with a
/// key join the room `lobby` opens. `None` when the port cannot be bound or there is no random
/// key (the control channel is off); the key file is `None` when it could not be written
/// (requests with the window's key are then refused).
pub fn start(port: u16, ctx: egui::Context, settings: Option<&Path>, lobby: Lobby) -> Option<(Receiver<ControlRequest>, Option<KeyFile>)> {
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            log::error!("control server failed to bind 127.0.0.1:{port}: {e}");
            return None;
        }
    };
    let key = match random_key() {
        Ok(k) => k,
        Err(e) => {
            log::error!("control server off: no random key ({e})");
            return None;
        }
    };
    let key_file = match settings {
        Some(dir) => {
            let p = wordcraft_control_key::key_path(dir, port);
            match write_key_file(&p, &key) {
                Ok(()) => Some(KeyFile(p)),
                Err(e) => {
                    log::error!("control key file not written ({e}): requests will be refused");
                    None
                }
            }
        }
        None => {
            log::error!("no settings folder for the control key file: requests will be refused");
            None
        }
    };
    match &key_file {
        Some(f) => log::info!("control server listening on 127.0.0.1:{port}, key in {}", f.path().display()),
        None => log::info!("control server listening on 127.0.0.1:{port}"),
    }
    Some((serve_on(listener, ctx, key, FIRST_REQUEST, lobby), key_file))
}

/// A new key: 32 random bytes from the operating system, as hex.
fn random_key() -> Result<String, String> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| e.to_string())?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Write the key file: a stale one (from a run that did not exit cleanly) is replaced; on Unix
/// the new file is created with mode 0600.
fn write_key_file(p: &Path, key: &str) -> Result<(), String> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("cannot create {}: {e}", d.display()))?;
    }
    let _ = std::fs::remove_file(p);
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
    let mut f = open.open(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
    f.write_all(key.as_bytes()).map_err(|e| format!("cannot write {}: {e}", p.display()))
}

/// Compare a key with the window's in time that does not depend on where they differ. An
/// empty key never matches.
fn keys_match(given: &str, key: &str) -> bool {
    let (a, b) = (given.as_bytes(), key.as_bytes());
    !b.is_empty() && a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn serve_on(listener: TcpListener, ctx: egui::Context, key: String, first: Duration, lobby: Lobby) -> Receiver<ControlRequest> {
    let (tx, rx) = channel::<ControlRequest>();
    let key: Arc<str> = key.into();
    // Wrong keys slow down the replies to the next ones, on every connection.
    let refusals = Arc::new(hosted::Refusals::default());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (tx, ctx, key, lobby, refusals) = (tx.clone(), ctx.clone(), key.clone(), lobby.clone(), refusals.clone());
            std::thread::spawn(move || serve(stream, tx, ctx, &key, first, &lobby, &refusals));
        }
    });
    rx
}

/// The read side of a connection with an optional overall deadline: each read waits only for
/// the time left, so blank lines, bad JSON or a slow drip of bytes cannot extend it.
struct Timed {
    s: TcpStream,
    deadline: Option<Instant>,
}

impl Read for Timed {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(d) = self.deadline {
            let left = d.checked_duration_since(Instant::now()).filter(|l| !l.is_zero()).ok_or(std::io::ErrorKind::TimedOut)?;
            self.s.set_read_timeout(Some(left))?;
        }
        self.s.read(buf)
    }
}

impl Timed {
    fn set_deadline(&mut self, d: Option<Instant>) {
        self.deadline = d;
        if d.is_none() {
            let _ = self.s.set_read_timeout(None);
        }
    }
}

/// The next line (trimmed), `None` at the end of the stream or after a read error.
fn read_line(r: &mut BufReader<Timed>) -> Option<Result<String, &'static str>> {
    let mut buf = Vec::new();
    let n = r.by_ref().take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf).ok()?;
    if n == 0 {
        return None;
    }
    if buf.len() > MAX_LINE {
        return Some(Err("line too long"));
    }
    Some(String::from_utf8(buf).map(|s| s.trim().to_string()).map_err(|_| "not UTF-8"))
}

/// Read and drop the rest of an oversize line, so closing the socket does not reset the
/// connection before the client has read the error. At most 64 MiB, within 2 s.
fn discard_rest_of_line(r: &mut BufReader<Timed>) {
    r.get_mut().set_deadline(Some(Instant::now() + Duration::from_secs(2)));
    let mut left: usize = 64 << 20;
    while left > 0 {
        let (used, done) = match r.fill_buf() {
            Ok([]) | Err(_) => return,
            Ok(b) => match b.iter().position(|&c| c == b'\n') {
                Some(i) => (i + 1, true),
                None => (b.len(), false),
            },
        };
        r.consume(used);
        if done {
            return;
        }
        left = left.saturating_sub(used);
    }
}

/// A request: its id, method and params, and the key it carries (the top-level `key`; `auth`
/// may carry it in its params, while for other methods `params.key` is an ordinary parameter,
/// as in `ui.key {"key": "Enter"}`).
struct Request {
    id: Value,
    method: String,
    params: Value,
    key: Option<String>,
}

/// The next request on the connection; `None` when the connection ends: at the end of the
/// stream, after a read error, or after a line that is too long or isn't JSON (answered first).
fn next_request(reader: &mut BufReader<Timed>, out: &mut TcpStream) -> Option<Request> {
    loop {
        let line = match read_line(reader)? {
            Ok(l) => l,
            Err(e) => {
                let _ = writeln!(out, "{}", json!({"ok": false, "error": e}));
                if e == "line too long" {
                    discard_rest_of_line(reader);
                }
                return None;
            }
        };
        if line.is_empty() {
            continue;
        }
        let msg = match serde_json::from_str::<Value>(&line) {
            Ok(m) => m,
            // Close on a line that isn't JSON: an HTTP request (say, a web page's `fetch` to this
            // port) starts with a request line that never parses, so it can't carry a command in
            // its body, even before the key is checked.
            Err(e) => {
                let _ = writeln!(out, "{}", json!({"ok": false, "error": format!("bad JSON: {e}; closing the connection")}));
                return None;
            }
        };
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("").to_string();
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let key = msg
            .get("key")
            .and_then(Value::as_str)
            .or_else(|| if method == "auth" { params.get("key").and_then(Value::as_str) } else { None })
            .map(str::to_string);
        return Some(Request { id, method, params, key });
    }
}

/// Write reply `v` to request `id`; false when the connection is gone.
fn send(out: &mut TcpStream, id: &Value, mut v: Value) -> bool {
    if let Some(o) = v.as_object_mut() {
        o.insert("id".into(), id.clone());
    }
    writeln!(out, "{v}").is_ok()
}

fn serve(stream: TcpStream, tx: Sender<ControlRequest>, ctx: egui::Context, key: &str, first: Duration, lobby: &Lobby, refusals: &hosted::Refusals) {
    let Ok(read) = stream.try_clone() else { return };
    let mut out = stream;
    // One overall deadline from accept to the first authorised request; after it, none.
    let mut reader = BufReader::new(Timed { s: read, deadline: Some(Instant::now() + first) });
    let mut authorised = false;
    // The key an `auth` request gave this connection.
    let mut conn_key: Option<String> = None;
    while let Some(req) = next_request(&mut reader, &mut out) {
        // Checked on every request.
        let k = req.key.clone().or_else(|| conn_key.clone()).unwrap_or_default();
        if !keys_match(&k, key) {
            // The key of an invite to the shared document, on the connection's first request:
            // its holder's own copy answers them from now on, on this thread.
            if !authorised && let Ok(copy) = lobby.join_with(&k) {
                reader.get_mut().set_deadline(None);
                serve_copy(&mut reader, &mut out, copy, &k, req, refusals);
                return;
            }
            std::thread::sleep(refusals.refused());
            let _ = send(&mut out, &req.id, json!({"ok": false, "error": "unauthorized"}));
            break;
        }
        if !authorised {
            authorised = true;
            reader.get_mut().set_deadline(None);
        }
        if req.method == "auth" {
            conn_key = Some(k);
            // The window's key: whoever holds it acts as the person, in the window.
            if !send(&mut out, &req.id, json!({"ok": true, "result": {"as": "window"}})) {
                break;
            }
            continue;
        }
        let (r, rrx) = ControlRequest::new(req.method, req.params);
        if tx.send(r).is_err() {
            break;
        }
        ctx.request_repaint();
        let reply = rrx.recv_timeout(Duration::from_secs(60)).unwrap_or_else(|_| json!({"ok": false, "error": "timeout"}));
        if !send(&mut out, &req.id, reply) {
            break;
        }
    }
}

/// The rest of a connection whose first request, `first`, carried `key`, an invite's: `copy`,
/// its holder's own copy of the shared document, answers each request ([`hosted::call`];
/// `auth` just says yes). Every request carries that key or none. The connection ends, and the
/// copy leaves the room, when its participant is out of it or sharing stops.
fn serve_copy(reader: &mut BufReader<Timed>, out: &mut TcpStream, mut copy: Replica, key: &str, first: Request, refusals: &hosted::Refusals) {
    let mut next = Some(first);
    while let Some(req) = next.take().or_else(|| next_request(reader, out)) {
        if req.key.as_deref().is_some_and(|k| !keys_match(k, key)) {
            std::thread::sleep(refusals.refused());
            let _ = send(out, &req.id, json!({"ok": false, "error": "unauthorized"}));
            return;
        }
        if let Some(why) = hosted::ended(&copy) {
            let _ = send(out, &req.id, json!({"ok": false, "error": why}));
            return;
        }
        let reply = match req.method.as_str() {
            "auth" => json!({"ok": true, "result": hosted::auth_reply(&copy)}),
            method => match hosted::call(&mut copy, method, &req.params) {
                Ok(v) => json!({"ok": true, "result": v}),
                Err(e) => json!({"ok": false, "error": e}),
            },
        };
        if !send(out, &req.id, reply) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;
    use wordcraft_doc::{Document, StoryRef};
    use wordcraft_engine::room::{Role, Room};

    const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn boot_with(first: Duration) -> (u16, Receiver<ControlRequest>) {
        boot_in(first, Lobby::default())
    }

    fn boot_in(first: Duration, lobby: Lobby) -> (u16, Receiver<ControlRequest>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        (port, serve_on(listener, egui::Context::default(), KEY.to_string(), first, lobby))
    }

    fn boot() -> (u16, Receiver<ControlRequest>) {
        boot_with(FIRST_REQUEST)
    }

    /// Answer the next request that reaches the UI with `ok`, and return it.
    fn answer(rx: &Receiver<ControlRequest>) -> ControlRequest {
        let req = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let _ = req.reply.send(json!({"ok": true, "result": {}}));
        req
    }

    /// One connection, several lines.
    struct Conn(BufReader<TcpStream>, TcpStream);

    impl Conn {
        fn open(port: u16) -> Self {
            let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            Self(BufReader::new(s.try_clone().unwrap()), s)
        }
        fn send(&mut self, line: &str) {
            writeln!(self.1, "{line}").unwrap();
        }
        fn recv(&mut self) -> Value {
            let mut r = String::new();
            let _ = self.0.read_line(&mut r);
            serde_json::from_str(&r).unwrap_or_default()
        }
        fn call(&mut self, line: &str) -> Value {
            self.send(line);
            self.recv()
        }
        /// The server closed the connection.
        fn closed(&mut self) -> bool {
            let mut r = String::new();
            matches!(self.0.read_line(&mut r), Ok(0))
        }
    }

    #[test]
    fn a_request_without_the_key_is_refused_and_closed() {
        let (port, rx) = boot();
        let mut c = Conn::open(port);
        let r = c.call(r#"{"id":1,"method":"document.inspect"}"#);
        assert_eq!((r["ok"].clone(), r["error"].clone(), r["id"].clone()), (json!(false), json!("unauthorized"), json!(1)));
        assert!(c.closed());
        assert!(rx.try_recv().is_err(), "nothing reached the UI");
    }

    #[test]
    fn a_wrong_key_is_refused_and_closed() {
        let (port, rx) = boot();
        for key in ["nope", &KEY[..63], &format!("{KEY}0"), &KEY.to_uppercase()] {
            let mut c = Conn::open(port);
            assert_eq!(c.call(&format!(r#"{{"id":1,"key":"{key}","method":"document.inspect"}}"#))["error"], "unauthorized", "{key}");
            assert!(c.closed());
        }
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn the_key_on_each_request() {
        let (port, rx) = boot();
        let mut c = Conn::open(port);
        c.send(&format!(r#"{{"id":7,"key":"{KEY}","method":"document.inspect","params":{{"text":true}}}}"#));
        let req = answer(&rx);
        assert_eq!((req.method.as_str(), &req.params), ("document.inspect", &json!({"text": true})));
        let r = c.recv();
        assert_eq!((r["ok"].clone(), r["id"].clone()), (json!(true), json!(7)));
        // Every request needs it: one without the key is refused, also after good ones.
        assert_eq!(c.call(r#"{"id":8,"method":"document.inspect"}"#)["error"], "unauthorized");
        assert!(c.closed());
    }

    #[test]
    fn auth_once_per_connection() {
        let (port, rx) = boot();
        let mut c = Conn::open(port);
        // The window's key reaches the window, and `auth` says so (#74).
        assert_eq!(c.call(&format!(r#"{{"id":1,"method":"auth","key":"{KEY}"}}"#))["result"], json!({"as": "window"}));
        for id in 2..4 {
            c.send(&format!(r#"{{"id":{id},"method":"document.inspect"}}"#));
            answer(&rx);
            assert_eq!(c.recv()["ok"], true);
        }
        // `params.key` works for `auth` too.
        let mut c = Conn::open(port);
        assert_eq!(c.call(&format!(r#"{{"id":1,"method":"auth","params":{{"key":"{KEY}"}}}}"#))["ok"], true);
        // A wrong key on an authenticated connection is still refused, and closes it.
        assert_eq!(c.call(r#"{"id":2,"key":"nope","method":"document.inspect"}"#)["error"], "unauthorized");
        assert!(c.closed());
        // A failed auth closes the connection.
        let mut c = Conn::open(port);
        assert_eq!(c.call(r#"{"id":1,"method":"auth","key":"nope"}"#)["error"], "unauthorized");
        assert!(c.closed());
    }

    #[test]
    fn params_key_is_only_a_secret_for_auth() {
        let (port, rx) = boot();
        // `params.key` does not authenticate other methods.
        let mut c = Conn::open(port);
        assert_eq!(c.call(&format!(r#"{{"id":1,"method":"document.inspect","params":{{"key":"{KEY}"}}}}"#))["error"], "unauthorized");
        // It is an ordinary parameter for them (`ui.key` presses a key).
        let mut c = Conn::open(port);
        c.send(&format!(r#"{{"id":1,"key":"{KEY}","method":"ui.key","params":{{"key":"Enter"}}}}"#));
        let req = answer(&rx);
        assert_eq!((req.method.as_str(), &req.params), ("ui.key", &json!({"key": "Enter"})));
        assert_eq!(c.recv()["ok"], true);
    }

    #[test]
    fn a_line_over_1_mib_is_refused_and_closed() {
        let (port, rx) = boot();
        let mut c = Conn::open(port);
        let big = format!(r#"{{"id":1,"key":"{KEY}","method":"document.inspect","pad":"{}"}}"#, "x".repeat(MAX_LINE));
        assert_eq!(c.call(&big)["error"], "line too long");
        assert!(c.closed());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_silent_connection_is_closed() {
        let (port, rx) = boot_with(Duration::from_millis(300));
        let mut quiet = Conn::open(port);
        let t0 = Instant::now();
        assert!(quiet.closed());
        assert!(t0.elapsed() < Duration::from_secs(3));
        // After an authorised request the connection may stay idle.
        let mut busy = Conn::open(port);
        assert_eq!(busy.call(&format!(r#"{{"id":1,"method":"auth","key":"{KEY}"}}"#))["ok"], true);
        std::thread::sleep(Duration::from_millis(600));
        busy.send(r#"{"id":2,"method":"document.inspect"}"#);
        answer(&rx);
        assert_eq!(busy.recv()["ok"], true);
    }

    #[test]
    fn the_first_request_limit_is_one_overall_deadline() {
        // Blank lines and bad JSON do not keep a connection without the key open.
        let (port, _rx) = boot_with(Duration::from_millis(300));
        let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut w = s.try_clone().unwrap();
        let feeder = std::thread::spawn(move || {
            for i in 0..40 {
                if w.write_all(if i % 2 == 0 { b"\n" } else { b"not json\n" }).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let t0 = Instant::now();
        let mut r = BufReader::new(s);
        r.get_ref().set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut line = String::new();
        loop {
            line.clear();
            if matches!(r.read_line(&mut line), Ok(0) | Err(_)) {
                break;
            }
            assert!(line.contains("bad JSON"), "{line}");
        }
        let _ = feeder.join();
        assert!(t0.elapsed() < Duration::from_millis(1500), "{:?}", t0.elapsed());
    }

    #[test]
    fn bad_json_closes_the_connection() {
        let (port, rx) = boot();
        let mut c = Conn::open(port);
        assert_eq!(c.call("not json")["ok"], false);
        assert!(c.closed());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn http_request_cannot_smuggle_a_command() {
        // What a web page's `fetch("http://127.0.0.1:<port>/", {method: "POST", body})` sends,
        // even with the key in the body.
        let (port, rx) = boot();
        let body = format!("\n{{\"key\":\"{KEY}\",\"method\":\"file.saveAs\",\"params\":{{\"path\":\"/tmp/x.docx\"}}}}\n");
        let req = format!("POST / HTTP/1.1\r\nHost: 127.0.0.1:7981\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        let mut c = Conn::open(port);
        c.1.write_all(req.as_bytes()).unwrap();
        let _ = c.recv();
        assert!(c.closed());
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }

    /// A server whose lobby lets participants into a room for "Hello world", and the key of an
    /// invite for "Agent" with `role`.
    fn boot_shared(role: Role) -> (u16, Receiver<ControlRequest>, std::sync::Arc<Room>, String) {
        let room = Room::new(Document::from_text("Hello world"));
        let key = room.invite("Agent", role).unwrap();
        let lobby = Lobby::default();
        lobby.open(room.clone());
        let (port, rx) = boot_in(FIRST_REQUEST, lobby);
        (port, rx, room, key)
    }

    /// Wait up to 5 s for `f` to hold (the server's threads finish on their own time).
    fn eventually(f: impl Fn() -> bool) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(5) {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn an_invite_s_key_gets_a_copy_of_its_own_and_the_window_sees_nothing() {
        let (port, rx, room, key) = boot_shared(Role::Editor);
        let mut c = Conn::open(port);
        let r =
            c.call(&format!(r#"{{"id":1,"key":"{key}","method":"engine.execute","params":{{"command":"text.insert","params":{{"text":"Hi "}}}}}}"#));
        assert_eq!((r["ok"].clone(), r["id"].clone()), (json!(true), json!(1)), "{r}");
        assert_eq!(room.document().1.plain_text(StoryRef::Body), "Hi Hello world");
        // Later requests may leave the key out, or carry it again.
        let r = c.call(r#"{"id":2,"method":"document.inspect","params":{"text":true}}"#);
        assert!(r["result"].to_string().contains("Hi Hello world"), "{r}");
        let r = c.call(&format!(r#"{{"id":3,"key":"{key}","method":"room.participants"}}"#));
        assert_eq!((r["id"].clone(), r["result"]["you"]["name"].clone()), (json!(3), json!("Agent")));
        // The window's methods and files are refused, and the connection stays.
        let r = c.call(r#"{"id":4,"method":"ui.screenshot","params":{"path":"/tmp/x.png"}}"#);
        assert_eq!(r["ok"], false);
        assert!(r["error"].as_str().unwrap().contains("window"), "{r}");
        let r = c.call(r#"{"id":5,"method":"engine.execute","params":{"command":"file.save","params":{"path":"/tmp/x.docx"}}}"#);
        assert_eq!(r["ok"], false);
        // The window's key on this connection is refused, and closes it.
        let r = c.call(&format!(r#"{{"id":6,"key":"{KEY}","method":"document.inspect"}}"#));
        assert_eq!(r["error"], "unauthorized");
        assert!(c.closed());
        assert!(rx.try_recv().is_err(), "nothing reached the window");
        // Its copy left the room with the connection; `auth` with the key joins again.
        assert!(eventually(|| room.participants().is_empty()));
        let mut c = Conn::open(port);
        // `auth` says what the key reaches: a participant's copy, whose (#74).
        let r = c.call(&format!(r#"{{"id":1,"method":"auth","params":{{"key":"{key}"}}}}"#));
        assert_eq!(r["result"], json!({"as": "participant", "name": "Agent", "role": "editor"}), "{r}");
        assert_eq!(c.call(r#"{"id":2,"method":"engine.execute","params":{"command":"caret.docEnd"}}"#)["ok"], true);
        assert_eq!(room.participants().len(), 1);
        // The window's key still reaches the window.
        let mut w = Conn::open(port);
        w.send(&format!(r#"{{"id":1,"key":"{KEY}","method":"document.inspect"}}"#));
        answer(&rx);
        assert_eq!(w.recv()["ok"], true);
    }

    /// An agent invited as an owner changes no option, which files agents can use among them, and
    /// never reads the window's key, whatever files agents may use: the settings folder is out of
    /// every agent's reach (#31).
    #[test]
    fn an_owner_s_invite_changes_no_option_and_reads_no_key() {
        let (port, rx, room, key) = boot_shared(Role::Owner);
        room.set_agent_files(wordcraft_engine::agent_files::AgentFiles::All, vec![]);
        let settings = std::env::temp_dir().join(format!("wordcraft-control-settings-{}", std::process::id()));
        std::fs::create_dir_all(&settings).unwrap();
        let key_file = settings.join("control-key.local-7981");
        std::fs::write(&key_file, KEY).unwrap();
        wordcraft_engine::file_access::protect(&settings);
        let mut c = Conn::open(port);
        let r = c.call(&format!(
            r#"{{"id":1,"key":"{key}","method":"engine.execute","params":{{"command":"file.options","params":{{"agentFiles":"all"}}}}}}"#
        ));
        assert_eq!(r["ok"], false);
        assert!(r["error"].as_str().unwrap().contains("set at the window"), "{r}");
        let path = serde_json::to_string(&*key_file.to_string_lossy()).unwrap();
        let r = c.call(&format!(r#"{{"id":2,"method":"engine.execute","params":{{"command":"insert.textFromFile","params":{{"path":{path}}}}}}}"#));
        assert_eq!(r["ok"], false);
        assert!(r["error"].as_str().unwrap().contains("settings folder is out of reach"), "{r}");
        assert!(!room.document().1.plain_text(StoryRef::Body).contains(KEY));
        assert!(rx.try_recv().is_err(), "nothing reached the window");
        let _ = std::fs::remove_dir_all(&settings);
    }

    #[test]
    fn revoking_the_key_or_sharing_no_more_ends_the_connection() {
        let (port, rx, room, key) = boot_shared(Role::Suggester);
        let mut c = Conn::open(port);
        assert_eq!(c.call(&format!(r#"{{"id":1,"method":"auth","key":"{key}"}}"#))["ok"], true);
        assert!(room.revoke(&key));
        let r = c.call(r#"{"id":2,"method":"document.inspect"}"#);
        assert!(r["error"].as_str().unwrap().contains("taken back"), "{r}");
        assert!(c.closed());
        let mut c = Conn::open(port);
        assert_eq!(c.call(&format!(r#"{{"id":1,"method":"auth","key":"{key}"}}"#))["error"], "unauthorized");
        assert!(c.closed());
        // Sharing stops: the connections end, and no key lets anyone in.
        let other = room.invite("Reader", Role::Viewer).unwrap();
        let mut c = Conn::open(port);
        assert_eq!(c.call(&format!(r#"{{"id":1,"method":"auth","key":"{other}"}}"#))["ok"], true);
        room.close();
        let r = c.call(r#"{"id":2,"method":"document.inspect"}"#);
        assert!(r["error"].as_str().unwrap().contains("isn't shared"), "{r}");
        assert!(c.closed());
        let mut c = Conn::open(port);
        assert_eq!(c.call(&format!(r#"{{"id":1,"method":"auth","key":"{other}"}}"#))["error"], "unauthorized");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn with_nothing_shared_an_invite_s_key_is_any_wrong_key() {
        let (port, rx) = boot();
        let mut c = Conn::open(port);
        assert_eq!(c.call(r#"{"id":1,"key":"0123456789abcdef0123456789abcdef","method":"document.inspect"}"#)["error"], "unauthorized");
        assert!(c.closed());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn keys_compare_whole() {
        assert!(keys_match(KEY, KEY));
        assert!(!keys_match(KEY, &KEY[..63]));
        assert!(!keys_match(&KEY[..63], KEY));
        assert!(!keys_match(KEY, &KEY.replace('f', "e")));
        assert!(!keys_match("", ""), "an empty key never matches");
    }

    #[test]
    fn random_keys_are_256_bits() {
        let (a, b) = (random_key().unwrap(), random_key().unwrap());
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wordcraft-control-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_key_file_is_written_after_the_bind_and_removed_on_exit() {
        let dir = temp_dir("life");
        // The port is taken: no key file.
        let taken = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = taken.local_addr().unwrap().port();
        assert!(start(port, egui::Context::default(), Some(&dir), Lobby::default()).is_none());
        assert!(!dir.exists() || std::fs::read_dir(&dir).unwrap().next().is_none(), "no key file after a failed bind");
        drop(taken);
        // Bound: the key file holds the key that opens the port.
        let mut free_port = port.wrapping_add(1).max(1024);
        let mut started = None;
        for _ in 0..100 {
            if let Some(res) = start(free_port, egui::Context::default(), Some(&dir), Lobby::default()) {
                started = Some((res, free_port));
                break;
            }
            free_port = free_port.wrapping_add(1).max(1024);
        }
        let ((rx, key_file), port) = started.unwrap();
        let key_file = key_file.unwrap();
        assert_eq!(key_file.path(), wordcraft_control_key::key_path(&dir, port));
        let key = std::fs::read_to_string(key_file.path()).unwrap();
        assert_eq!(key.len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(key_file.path()).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let mut c = Conn::open(port);
        c.send(&format!(r#"{{"id":1,"key":"{key}","method":"document.inspect"}}"#));
        answer(&rx);
        assert_eq!(c.recv()["ok"], true);
        key_file.remove();
        assert!(!key_file.path().exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_key_file_is_replaced() {
        let dir = temp_dir("stale");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("control-key.test");
        std::fs::write(&p, "old").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        write_key_file(&p, KEY).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), KEY);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

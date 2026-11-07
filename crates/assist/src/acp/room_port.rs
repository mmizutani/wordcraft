//! The shared document's room on a loopback port of its own ([`RoomPort`]), which only invites'
//! keys open: the built-in assistant's agents reach their copies of the document through it
//! (WordCraft's MCP server, `wordcraft-cli mcp --connect`), whether or not the window runs its
//! control channel.
//!
//! It speaks the control channel's protocol (`docs/control-protocol.md`): one JSON request per
//! line, one reply per line. A connection's first request must carry the key of an invite to
//! the document the window shares (top-level `key`, or `auth`'s); from then on the holder's own
//! copy answers it ([`hosted::call`]), and later requests carry the same key or none. Every
//! connection with the same key works on the same copy ([`Seats`]), so an agent that connects
//! for each request (`wordcraft-cli call` in its shell) finds what it selected still selected,
//! and stays one participant. The window's own key opens nothing here: the port doesn't know it,
//! and no key file names it, so an MCP server that went looking for a key would find none that
//! works.

use std::io::{BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wordcraft_engine::room::{Lobby, ParticipantId, Replica, Room, hosted};
use wordcraft_mcp::server::{BoundedLine, read_bounded_line};

/// Longest request line; a longer one is refused and the connection closed.
pub const MAX_REQUEST_LINE: usize = 1 << 20;

/// A connection that sends no request with an invite's key in this time is closed.
pub const FIRST_REQUEST: Duration = Duration::from_secs(30);

/// Most connections served at once; more are refused.
pub const MAX_CONNECTIONS: usize = 64;

/// Most copies the port holds at once ([`Seats`]).
pub const MAX_SEATS: usize = 64;

/// How often the port lets go of the copies whose holders are out of the room.
const SWEEP: Duration = Duration::from_secs(2);

/// The room's own port, listening on `127.0.0.1` (a port the system picks) for the room the
/// lobby opens. The lobby says where it is ([`Lobby::addr`]) while it listens. Dropping it
/// closes the port, unless it was kept open ([`RoomPort::keep_open`]).
pub struct RoomPort {
    addr: SocketAddr,
    lobby: Lobby,
    open: Arc<AtomicBool>,
    seats: Seats,
    kept: bool,
}

impl RoomPort {
    /// Listen for `lobby`'s room, and say where in the lobby.
    pub fn open(lobby: &Lobby) -> std::io::Result<RoomPort> {
        RoomPort::open_with(lobby, FIRST_REQUEST)
    }

    fn open_with(lobby: &Lobby, first: Duration) -> std::io::Result<RoomPort> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let addr = listener.local_addr()?;
        let open = Arc::new(AtomicBool::new(true));
        let seats = Seats::default();
        let (room, listening, held) = (lobby.clone(), open.clone(), seats.clone());
        std::thread::Builder::new().name("room port".into()).spawn(move || accept(&listener, &room, &held, &listening, first))?;
        let (listening, held_too) = (open.clone(), seats.clone());
        std::thread::Builder::new().name("room port seats".into()).spawn(move || {
            while listening.load(Ordering::SeqCst) {
                std::thread::sleep(SWEEP);
                seats.sweep();
            }
            seats.list().clear();
        })?;
        lobby.set_addr(Some(addr));
        log::info!("the shared document's room listens on {addr} (invites' keys only)");
        Ok(RoomPort { addr, lobby: lobby.clone(), open, seats: held_too, kept: false })
    }

    /// Where it listens.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Keep the port open until the process ends (the app's window), whatever becomes of this
    /// handle.
    pub fn keep_open(mut self) {
        self.kept = true;
    }
}

impl Drop for RoomPort {
    fn drop(&mut self) {
        if self.kept {
            return;
        }
        self.open.store(false, Ordering::SeqCst);
        if self.lobby.addr() == Some(self.addr) {
            self.lobby.set_addr(None);
        }
        // Its copies leave the room; a connection still served keeps its own till it ends.
        self.seats.list().clear();
        // Wake the listener, which then sees it's closed.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(500));
    }
}

/// Counts a connection while it's served.
struct Live(Arc<AtomicUsize>);

impl Live {
    fn new(count: &Arc<AtomicUsize>) -> Live {
        count.fetch_add(1, Ordering::SeqCst);
        Live(count.clone())
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The copies of the shared document the port holds, one for each invite key that opened it:
/// every connection with that key works on the same copy, one request at a time, so what one
/// request selects the next one finds selected, and a holder who connects for each request
/// (`wordcraft-cli call`) stays one participant rather than joining the room again each time.
/// A copy is let go once its holder is out of the room (the invite was taken back, or sharing
/// stopped), and when the port closes.
#[derive(Clone, Default)]
struct Seats(Arc<Mutex<Vec<Seat>>>);

/// The copy the port holds for one invite key.
struct Seat {
    key: String,
    copy: Arc<Mutex<Replica>>,
    used: Instant,
}

impl Seats {
    fn list(&self) -> MutexGuard<'_, Vec<Seat>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The copy for invite `key`: the one the port holds, or a new one, joined with the key.
    /// `None` when the key opens nothing.
    fn take(&self, lobby: &Lobby, key: &str) -> Option<Arc<Mutex<Replica>>> {
        if key.is_empty() {
            return None;
        }
        let mut seats = self.list();
        sweep(&mut seats);
        // Every seat is compared, so how long it takes says nothing about the key.
        let found = seats.iter_mut().fold(None, |found, seat| if same_key(key, &seat.key) { Some(seat) } else { found });
        if let Some(seat) = found {
            seat.used = Instant::now();
            return Some(seat.copy.clone());
        }
        let copy = Arc::new(Mutex::new(lobby.join_with(key).ok()?));
        if seats.len() >= MAX_SEATS {
            // The one used longest ago goes; a connection that has it keeps it till it ends.
            let oldest = seats.iter().enumerate().min_by_key(|(_, seat)| seat.used).map(|(i, _)| i);
            if let Some(i) = oldest {
                seats.swap_remove(i);
            }
        }
        seats.push(Seat { key: key.to_string(), copy: copy.clone(), used: Instant::now() });
        Some(copy)
    }

    /// Let go of the copies whose holders are out of the room.
    fn sweep(&self) {
        sweep(&mut self.list());
    }
}

/// Let go of the copies in `seats` whose holders are out of the room, and of any a request
/// broke; a copy busy with a request stays.
fn sweep(seats: &mut Vec<Seat>) {
    seats.retain(|seat| match seat.copy.try_lock() {
        Ok(copy) => hosted::ended(&copy).is_none(),
        Err(TryLockError::WouldBlock) => true,
        Err(TryLockError::Poisoned(_)) => false,
    });
}

/// The listener's thread: a thread for each connection, until the port closes.
fn accept(listener: &TcpListener, lobby: &Lobby, seats: &Seats, open: &AtomicBool, first: Duration) {
    let live = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        if !open.load(Ordering::SeqCst) {
            return;
        }
        let mut stream = match stream {
            Ok(s) => s,
            Err(e) => {
                // Out of file descriptors, say: wait a little rather than spin.
                log::debug!("room port: {e}");
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        if live.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
            let _ = writeln!(stream, "{}", json!({"ok": false, "error": "too many connections"}));
            continue;
        }
        let counted = Live::new(&live);
        let (lobby, seats) = (lobby.clone(), seats.clone());
        let spawned = std::thread::Builder::new().name("room port connection".into()).spawn(move || {
            let _counted = counted;
            serve(stream, &lobby, &seats, first);
        });
        if let Err(e) = spawned {
            log::warn!("room port: no thread for a connection: {e}");
        }
    }
}

/// The read side of a connection, with an overall deadline while it has one: each read waits
/// only for the time left, so blank lines or a slow drip of bytes can't extend it.
/// Authenticated reads keep partial lines while checking that the participant is still in.
struct Deadline {
    stream: TcpStream,
    until: Option<Instant>,
    participant: Option<(Arc<Room>, ParticipantId)>,
}

impl Read for Deadline {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let timeout = if let Some(until) = self.until {
                until.checked_duration_since(Instant::now()).filter(|l| !l.is_zero()).ok_or(std::io::ErrorKind::TimedOut)?
            } else {
                if self.participant.as_ref().is_some_and(|(room, id)| room.is_closed() || !room.is_in(*id)) {
                    return Ok(0);
                }
                Duration::from_millis(250)
            };
            self.stream.set_read_timeout(Some(timeout))?;
            match self.stream.read(buf) {
                Err(e) if self.until.is_none() && matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) => continue,
                result => return result,
            }
        }
    }
}

/// A request: its id, method and params, and the key it carries (top-level `key`, or `auth`'s
/// `params.key`; for other methods `params.key` is an ordinary parameter).
struct Request {
    id: Value,
    method: String,
    params: Value,
    key: Option<String>,
}

/// The next request; `None` when the connection ends: at its end, after a read error, or after
/// a line that is too long, isn't UTF-8 or isn't JSON (answered first).
fn next_request(reader: &mut BufReader<Deadline>, out: &mut TcpStream) -> Option<Request> {
    loop {
        let line = match read_bounded_line(reader, MAX_REQUEST_LINE) {
            Ok(BoundedLine::Line(line)) => line,
            Ok(BoundedLine::TooLong) => {
                let _ = writeln!(out, "{}", json!({"ok": false, "error": "line too long"}));
                return None;
            }
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                let _ = writeln!(out, "{}", json!({"ok": false, "error": "not UTF-8"}));
                return None;
            }
            Ok(BoundedLine::Eof) | Err(_) => return None,
        };
        if line.trim().is_empty() {
            continue;
        }
        let message = match serde_json::from_str::<Value>(&line) {
            Ok(m) => m,
            Err(e) => {
                let _ = writeln!(out, "{}", json!({"ok": false, "error": format!("bad JSON: {e}; closing the connection")}));
                return None;
            }
        };
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let method = message.get("method").and_then(Value::as_str).unwrap_or_default().to_string();
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
        let key = message
            .get("key")
            .and_then(Value::as_str)
            .or_else(|| if method == "auth" { params.get("key").and_then(Value::as_str) } else { None })
            .map(str::to_string);
        return Some(Request { id, method, params, key });
    }
}

/// One deadline covers the entire response, even when a peer reads a few bytes at a time.
struct WriteDeadline<'a> {
    stream: &'a mut TcpStream,
    until: Instant,
}

impl Write for WriteDeadline<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        loop {
            let left = self.until.checked_duration_since(Instant::now()).filter(|left| !left.is_zero()).ok_or(std::io::ErrorKind::TimedOut)?;
            match self.stream.write(bytes) {
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(left.min(Duration::from_millis(10))),
                result => return result,
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

/// Write reply `v` to request `id`; whether the connection is still there.
fn send(out: &mut TcpStream, id: &Value, mut v: Value) -> bool {
    if let Some(o) = v.as_object_mut() {
        o.insert("id".into(), id.clone());
    }
    // A blocking write timeout can restart while the peer makes tiny progress inside one
    // syscall. Nonblocking writes keep the total deadline under this loop's control.
    if out.set_nonblocking(true).is_err() {
        return false;
    }
    let mut writer = WriteDeadline { stream: out, until: Instant::now() + Duration::from_secs(2) };
    let sent = writeln!(writer, "{v}").is_ok();
    // Reader and writer are clones of one socket; restore blocking reads after the response.
    out.set_nonblocking(false).is_ok() && sent
}

/// Whether `given` is `key`, compared in a time that doesn't depend on where they differ. An
/// empty key is never one.
fn same_key(given: &str, key: &str) -> bool {
    let (a, b) = (given.as_bytes(), key.as_bytes());
    !b.is_empty() && a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A connection: its first request must carry an invite's key, whose holder's copy answers
/// the rest.
fn serve(stream: TcpStream, lobby: &Lobby, seats: &Seats, first: Duration) {
    let _ = stream.set_nodelay(true);
    let Ok(read) = stream.try_clone() else { return };
    let mut out = stream;
    let mut reader = BufReader::new(Deadline { stream: read, until: Some(Instant::now() + first), participant: None });
    let Some(request) = next_request(&mut reader, &mut out) else { return };
    let key = request.key.clone().unwrap_or_default();
    let Some(copy) = seats.take(lobby, &key) else {
        let _ = send(&mut out, &request.id, json!({"ok": false, "error": "unauthorized"}));
        return;
    };
    let deadline = reader.get_mut();
    deadline.until = None;
    deadline.participant = Some({
        let Ok(copy) = copy.lock() else {
            let _ = send(&mut out, &request.id, json!({"ok": false, "error": "the copy of the shared document broke: connect again"}));
            return;
        };
        (copy.room().clone(), copy.id())
    });
    serve_copy(&mut reader, &mut out, copy, &key, request);
}

/// The rest of a connection whose first request, `first`, carried invite `key`: `copy`, its
/// holder's copy of the shared document, answers each request (`auth` just says yes). Every
/// request carries that key or none. The connection ends when the copy is out of the room (the
/// invite was taken back, or sharing stopped).
fn serve_copy(reader: &mut BufReader<Deadline>, out: &mut TcpStream, copy: Arc<Mutex<Replica>>, key: &str, first: Request) {
    let mut next = Some(first);
    while let Some(request) = next.take().or_else(|| next_request(reader, out)) {
        if request.key.as_deref().is_some_and(|k| !same_key(k, key)) {
            let _ = send(out, &request.id, json!({"ok": false, "error": "unauthorized"}));
            return;
        }
        let answered = match copy.lock() {
            // Another connection's request broke the copy: the next connection gets a new one.
            Err(_) => Err("the copy of the shared document broke: connect again".to_string()),
            Ok(mut copy) => match hosted::ended(&copy) {
                Some(why) => Err(why),
                None => Ok(match request.method.as_str() {
                    "auth" => json!({"ok": true, "result": {}}),
                    method => match hosted::call(&mut copy, method, &request.params) {
                        Ok(v) => json!({"ok": true, "result": v}),
                        Err(e) => json!({"ok": false, "error": e}),
                    },
                }),
            },
        };
        let reply = match answered {
            Ok(reply) => reply,
            Err(why) => {
                let _ = send(out, &request.id, json!({"ok": false, "error": why}));
                return;
            }
        };
        if !send(out, &request.id, reply) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::{Value, json};
    use wordcraft_doc::{Document, StoryRef};
    use wordcraft_engine::Session;
    use wordcraft_engine::room::{Lobby, Replica, Role, Room};

    use super::RoomPort;

    /// A window sharing "one two three" through `lobby`.
    fn share(lobby: &Lobby) -> (Session, Arc<Room>, Replica) {
        let mut session = Session::new(Document::from_text("one two three"));
        session.author = "Minoru".into();
        let (room, own) = Room::host(&mut session, "Minoru").unwrap();
        lobby.open(room.clone());
        (session, room, own)
    }

    struct Conn(BufReader<TcpStream>, TcpStream);

    impl Conn {
        fn open(port: &RoomPort) -> Conn {
            let s = TcpStream::connect(port.addr()).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
            Conn(BufReader::new(s.try_clone().unwrap()), s)
        }

        fn send(&mut self, line: &str) {
            let _ = self.1.write_all(line.as_bytes()).and_then(|()| self.1.write_all(b"\n"));
        }

        fn recv(&mut self) -> Value {
            let mut line = String::new();
            self.0.read_line(&mut line).unwrap();
            serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"))
        }

        fn call(&mut self, line: &str) -> Value {
            self.send(line);
            self.recv()
        }

        /// Whether the server closed the connection.
        fn closed(&mut self) -> bool {
            let mut line = String::new();
            matches!(self.0.read_line(&mut line), Ok(0) | Err(_))
        }
    }

    #[test]
    fn an_invites_key_gets_its_holder_a_copy_whose_edits_are_tracked_in_their_name() {
        let lobby = Lobby::default();
        let port = RoomPort::open(&lobby).unwrap();
        assert_eq!(lobby.addr(), Some(port.addr()));
        assert!(port.addr().ip().is_loopback());
        let (mut session, room, mut own) = share(&lobby);
        let key = room.invite("Claude Code", Role::Suggester).unwrap();
        let mut c = Conn::open(&port);
        let line = |id: u32, method: &str, params: Value| json!({"id": id, "key": key, "method": method, "params": params}).to_string();
        let reply = c.call(&line(1, "select.text", json!({"text": "two"})));
        assert_eq!((reply["id"].clone(), reply["ok"].clone()), (json!(1), json!(true)), "{reply}");
        let reply = c.call(&json!({"id": 2, "method": "text.insert", "params": {"text": "2"}}).to_string());
        assert_eq!(reply["ok"], json!(true), "{reply}");
        // The window takes the room's change in: a tracked change in the agent's name.
        for _ in 0..500 {
            let _ = own.lend(&mut session, Replica::sync);
            if session.doc.revisions.iter().any(|r| r.author == "Claude Code") {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(session.doc.revisions.iter().any(|r| r.author == "Claude Code"), "no tracked change in the agent's name");
        assert!(session.doc.final_text(StoryRef::Body).contains("one 2 three"));
        // `auth` with the same key says yes; another key is refused, and the connection closed.
        assert_eq!(c.call(&json!({"id": 3, "method": "auth", "key": key}).to_string())["ok"], json!(true));
        let reply = c.call(&json!({"id": 4, "key": "0".repeat(32), "method": "document.inspect"}).to_string());
        assert_eq!((reply["id"].clone(), reply["error"].clone()), (json!(4), json!("unauthorized")));
        assert!(c.closed());
        // A copy saves no file (and reaches none, as File › Options › Agents is at None).
        let mut c = Conn::open(&port);
        let reply = c.call(&line(5, "file.saveAs", json!({"path": std::env::temp_dir().join("wordcraft-room-port.docx")})));
        assert_eq!(reply["ok"], json!(false), "{reply}");
    }

    /// `wordcraft-cli call` connects for each request: what one selects the next one finds
    /// selected, and the holder stays one participant.
    #[test]
    fn every_connection_with_a_key_works_on_the_same_copy() {
        let lobby = Lobby::default();
        let port = RoomPort::open(&lobby).unwrap();
        let (mut session, room, mut own) = share(&lobby);
        let key = room.invite("Pi", Role::Suggester).unwrap();
        let call = |id: u32, method: &str, params: Value| {
            let mut c = Conn::open(&port);
            c.call(&json!({"id": id, "key": key, "method": method, "params": params}).to_string())
        };
        assert_eq!(call(1, "select.text", json!({"text": "two"}))["ok"], json!(true));
        assert_eq!(call(2, "text.insert", json!({"text": "2", "raw": true}))["ok"], json!(true));
        for _ in 0..500 {
            let _ = own.lend(&mut session, Replica::sync);
            if session.doc.final_text(StoryRef::Body) == "one 2 three" {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(session.doc.final_text(StoryRef::Body), "one 2 three");
        let pis = |room: &Room| room.participants().iter().filter(|p| p.name == "Pi").count();
        assert_eq!(pis(&room), 1);
        // A connection that stays (an MCP server) and one per request share it too.
        let mut stays = Conn::open(&port);
        assert_eq!(stays.call(&json!({"id": 3, "key": key, "method": "select.text", "params": {"text": "three"}}).to_string())["ok"], json!(true));
        let reply = call(4, "document.inspect", json!({}));
        assert!(reply["result"].to_string().contains("three"), "{reply}");
        assert_eq!(pis(&room), 1);
        // Taken back: out of the room, and the key opens nothing.
        assert!(room.revoke(&key));
        assert_eq!(pis(&room), 0);
        assert_eq!(call(5, "document.inspect", json!({}))["error"], json!("unauthorized"));
    }

    #[test]
    fn the_port_holds_a_bounded_number_of_copies() {
        let lobby = Lobby::default();
        let port = RoomPort::open(&lobby).unwrap();
        let (_session, room, _own) = share(&lobby);
        let keys: Vec<String> = (0..=super::MAX_SEATS).map(|i| room.invite(&format!("Agent {i}"), Role::Viewer).unwrap()).collect();
        for key in keys.iter().chain(keys.first()) {
            let mut c = Conn::open(&port);
            assert_eq!(c.call(&json!({"id": 1, "key": key, "method": "room.participants"}).to_string())["ok"], json!(true));
        }
        assert!(port.seats.list().len() <= super::MAX_SEATS);
    }

    #[test]
    fn keys_that_arent_invites_open_nothing() {
        let lobby = Lobby::default();
        let port = RoomPort::open(&lobby).unwrap();
        // Nothing shared: no key works.
        let mut c = Conn::open(&port);
        assert_eq!(c.call(&json!({"id": 1, "key": "k", "method": "document.inspect"}).to_string())["error"], json!("unauthorized"));
        assert!(c.closed());
        let (_session, room, _own) = share(&lobby);
        let key = room.invite("Codex", Role::Editor).unwrap();
        for line in [
            json!({"id": 1, "method": "document.inspect"}).to_string(),
            json!({"id": 1, "key": "", "method": "document.inspect"}).to_string(),
            json!({"id": 1, "key": key.to_uppercase() + "x", "method": "document.inspect"}).to_string(),
            json!({"id": 1, "method": "auth", "params": {"nokey": key}}).to_string(),
            json!({"id": 1, "method": "document.inspect", "params": {"key": key}}).to_string(),
        ] {
            let mut c = Conn::open(&port);
            assert_eq!(c.call(&line)["error"], json!("unauthorized"), "{line}");
            assert!(c.closed(), "{line}");
        }
        // `auth` carries the key in its params; later requests may leave it out.
        let mut c = Conn::open(&port);
        assert_eq!(c.call(&json!({"id": 1, "method": "auth", "params": {"key": key}}).to_string())["ok"], json!(true));
        assert_eq!(c.call(&json!({"id": 2, "method": "room.participants"}).to_string())["result"]["you"]["name"], json!("Codex"));
    }

    #[test]
    fn junk_is_answered_and_the_connection_closed() {
        let lobby = Lobby::default();
        let port = RoomPort::open(&lobby).unwrap();
        let (_session, room, _own) = share(&lobby);
        let key = room.invite("Pi", Role::Viewer).unwrap();
        let mut c = Conn::open(&port);
        assert!(c.call("GET / HTTP/1.1")["error"].as_str().unwrap().starts_with("bad JSON"));
        assert!(c.closed());
        let mut c = Conn::open(&port);
        c.send(&"x".repeat(super::MAX_REQUEST_LINE + 10));
        assert_eq!(c.recv()["error"], json!("line too long"));
        assert!(c.closed());
        let mut c = Conn::open(&port);
        let _ = c.1.write_all(b"\xff\xfe\n");
        assert_eq!(c.recv()["error"], json!("not UTF-8"));
        assert!(c.closed());
        // After all that, the invite still works.
        let mut c = Conn::open(&port);
        assert_eq!(c.call(&json!({"id": 9, "key": key, "method": "document.inspect"}).to_string())["ok"], json!(true));
    }

    #[test]
    fn a_silent_connection_is_closed_and_blank_lines_dont_keep_it() {
        let lobby = Lobby::default();
        let port = RoomPort::open_with(&lobby, Duration::from_millis(300)).unwrap();
        let mut c = Conn::open(&port);
        c.send("");
        c.send("");
        assert!(c.closed());
    }

    #[test]
    fn taking_the_invite_back_or_stopping_sharing_ends_the_connection() {
        let lobby = Lobby::default();
        let port = RoomPort::open(&lobby).unwrap();
        let (_session, room, _own) = share(&lobby);
        let key = room.invite("Claude Code", Role::Suggester).unwrap();
        let mut c = Conn::open(&port);
        assert_eq!(c.call(&json!({"id": 1, "key": key, "method": "document.inspect"}).to_string())["ok"], json!(true));
        assert!(room.revoke(&key));
        assert!(c.closed());
        // The key opens nothing any more.
        let mut c = Conn::open(&port);
        assert_eq!(c.call(&json!({"id": 3, "key": key, "method": "document.inspect"}).to_string())["error"], json!("unauthorized"));
        // Sharing stops.
        let key = room.invite("Codex", Role::Suggester).unwrap();
        let mut c = Conn::open(&port);
        assert_eq!(c.call(&json!({"id": 4, "key": key, "method": "document.inspect"}).to_string())["ok"], json!(true));
        lobby.close();
        assert!(c.closed());
    }

    #[test]
    fn idle_connections_release_their_slots_when_invites_or_rooms_end() {
        let lobby = Lobby::default();
        let port = RoomPort::open(&lobby).unwrap();
        let (_session, room, _own) = share(&lobby);
        for stop_sharing in [false, true] {
            let key = room.invite(if stop_sharing { "Closing agent" } else { "Revoked agent" }, Role::Viewer).unwrap();
            let mut connections = Vec::new();
            for _ in 0..super::MAX_CONNECTIONS {
                let mut c = Conn::open(&port);
                assert_eq!(c.call(&json!({"key": key, "method": "auth"}).to_string())["ok"], json!(true));
                c.0.get_ref().set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                connections.push(c);
            }
            if stop_sharing {
                lobby.close();
            } else {
                assert!(room.revoke(&key));
            }
            for mut c in connections {
                assert_eq!(c.0.read_line(&mut String::new()).unwrap(), 0, "idle socket stayed open");
            }
        }
        // A new room uses the listener after all the old connection slots were released.
        let (_session, room, _own) = share(&lobby);
        let key = room.invite("New agent", Role::Viewer).unwrap();
        let mut c = Conn::open(&port);
        assert_eq!(c.call(&json!({"key": key, "method": "auth"}).to_string())["ok"], json!(true));
    }

    #[test]
    fn an_authenticated_partial_request_survives_idle_read_checks() {
        let lobby = Lobby::default();
        let port = RoomPort::open(&lobby).unwrap();
        let (_session, room, _own) = share(&lobby);
        let key = room.invite("Slow agent", Role::Viewer).unwrap();
        let mut c = Conn::open(&port);
        assert_eq!(c.call(&json!({"key": key, "method": "auth"}).to_string())["ok"], json!(true));
        c.1.write_all(b"{\"id\":2,").unwrap();
        std::thread::sleep(Duration::from_millis(700));
        c.send("\"method\":\"document.inspect\"}");
        let reply = c.recv();
        assert_eq!((reply["id"].clone(), reply["ok"].clone()), (json!(2), json!(true)));
    }

    #[test]
    fn a_response_to_a_client_that_never_reads_is_bounded() {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let (finished, result) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let sent = super::send(&mut stream, &json!(1), json!({"text": "x".repeat(32 << 20)}));
            let _ = finished.send(sent);
        });
        let client = TcpStream::connect(addr).unwrap();
        let observed = result.recv_timeout(Duration::from_secs(5));
        // Unblock and join the writer even when the regression fails.
        drop(client);
        worker.join().unwrap();
        assert!(!observed.unwrap(), "an unread response must close its connection");
    }

    #[test]
    fn slow_response_reads_do_not_extend_the_write_deadline() {
        use std::io::Read;
        use std::sync::atomic::{AtomicBool, Ordering};
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let (finished, result) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = finished.send(super::send(&mut stream, &json!(1), json!({"text": "x".repeat(32 << 20)})));
        });
        let mut client = TcpStream::connect(addr).unwrap();
        client.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let reader = std::thread::spawn(move || {
            let mut bytes = [0; 4096];
            while !stopped.load(Ordering::SeqCst) {
                if matches!(client.read(&mut bytes), Ok(0)) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let observed = result.recv_timeout(Duration::from_secs(5));
        stop.store(true, Ordering::SeqCst);
        reader.join().unwrap();
        writer.join().unwrap();
        assert!(!observed.unwrap(), "slow progress must not keep a response alive indefinitely");
    }

    #[test]
    fn dropping_the_port_closes_it_unless_it_was_kept_open() {
        let lobby = Lobby::default();
        let port = RoomPort::open(&lobby).unwrap();
        let addr = port.addr();
        drop(port);
        assert_eq!(lobby.addr(), None);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
            assert!(std::time::Instant::now() < deadline, "the port still listens");
            std::thread::sleep(Duration::from_millis(20));
        }
        let port = RoomPort::open(&lobby).unwrap();
        let addr = port.addr();
        port.keep_open();
        assert_eq!(lobby.addr(), Some(addr));
        assert!(TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok());
    }
}

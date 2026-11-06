//! A small HTTP/1.1 server on 127.0.0.1 for the tests: the sign-in's and the API's endpoints,
//! scripted by each test. Every connection gets one answer and is closed.
// The streaming parts are for the backend's tests, which come with it.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::http::parse_query;

/// A request the server took.
#[derive(Clone, Debug, Default)]
pub(crate) struct Request {
    pub(crate) method: String,
    /// The path, without the query.
    pub(crate) path: String,
    pub(crate) query: String,
    /// Header names in lower case.
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

impl Request {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// A field of the form body.
    pub(crate) fn field(&self, name: &str) -> Option<String> {
        parse_query(&String::from_utf8_lossy(&self.body)).into_iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }

    /// The form body's field names.
    pub(crate) fn fields(&self) -> Vec<String> {
        parse_query(&String::from_utf8_lossy(&self.body)).into_iter().map(|(k, _)| k).collect()
    }

    pub(crate) fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// A piece of an answer's body.
#[derive(Clone, Debug)]
pub(crate) enum Part {
    /// These bytes, as one chunk.
    Bytes(Vec<u8>),
    /// Wait this long.
    Wait(Duration),
    /// Send nothing more until the server stops (at most 30 seconds), then close.
    Hold,
    /// Close the connection here, without ending the body.
    Cut,
}

/// An answer.
#[derive(Clone, Debug)]
pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) parts: Vec<Part>,
    /// Sent in chunks (a stream) rather than with a length.
    pub(crate) chunked: bool,
}

impl Reply {
    pub(crate) fn json(status: u16, body: &Value) -> Reply {
        Reply::bytes(status, "application/json", body.to_string().into_bytes())
    }

    pub(crate) fn bytes(status: u16, content_type: &str, body: Vec<u8>) -> Reply {
        Reply { status, headers: vec![("Content-Type".into(), content_type.into())], parts: vec![Part::Bytes(body)], chunked: false }
    }

    pub(crate) fn empty(status: u16) -> Reply {
        Reply { status, headers: Vec::new(), parts: Vec::new(), chunked: false }
    }

    /// A stream of server-sent events: each JSON value as one event, in a chunk of its own.
    pub(crate) fn sse(events: &[Value]) -> Reply {
        Reply::stream(events.iter().map(|e| Part::Bytes(format!("data: {e}\n\n").into_bytes())).collect())
    }

    /// A 200 event stream of `parts`.
    pub(crate) fn stream(parts: Vec<Part>) -> Reply {
        Reply { status: 200, headers: vec![("Content-Type".into(), "text/event-stream".into())], parts, chunked: true }
    }

    pub(crate) fn header(mut self, name: &str, value: &str) -> Reply {
        self.headers.push((name.into(), value.into()));
        self
    }
}

type Handler = dyn Fn(&Request, usize) -> Reply + Send + Sync;

/// The server: it answers each request with what the handler gives for it, with the number of
/// requests to the same path before it.
pub(crate) struct Server {
    base: String,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
}

impl Server {
    pub(crate) fn start(handler: impl Fn(&Request, usize) -> Reply + Send + Sync + 'static) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests: Arc<Mutex<Vec<Request>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let handler: Arc<Handler> = Arc::new(handler);
        let (seen, stopping) = (requests.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let (seen, stopping, handler) = (seen.clone(), stopping.clone(), handler.clone());
                        std::thread::spawn(move || serve(stream, &seen, &stopping, handler.as_ref()));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        Server { base, requests, stop }
    }

    /// `http://127.0.0.1:<port>`.
    pub(crate) fn base(&self) -> &str {
        &self.base
    }

    /// The requests so far.
    pub(crate) fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    /// The requests to `path` so far.
    pub(crate) fn to(&self, path: &str) -> Vec<Request> {
        self.requests().into_iter().filter(|r| r.path == path).collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn serve(mut stream: TcpStream, seen: &Mutex<Vec<Request>>, stop: &AtomicBool, handler: &Handler) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    let n = {
        let mut all = seen.lock().unwrap();
        let n = all.iter().filter(|r| r.path == request.path).count();
        all.push(request.clone());
        n
    };
    let reply = handler(&request, n);
    let reason = if reply.status < 400 { "OK" } else { "Error" };
    let mut head = format!("HTTP/1.1 {} {reason}\r\nConnection: close\r\n", reply.status);
    for (k, v) in &reply.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if reply.chunked {
        head.push_str("Transfer-Encoding: chunked\r\n\r\n");
    } else {
        let length: usize = reply.parts.iter().map(|p| if let Part::Bytes(b) = p { b.len() } else { 0 }).sum();
        head.push_str(&format!("Content-Length: {length}\r\n\r\n"));
    }
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    for part in reply.parts {
        let sent = match part {
            Part::Bytes(b) if reply.chunked => {
                stream.write_all(format!("{:x}\r\n", b.len()).as_bytes()).and_then(|()| stream.write_all(&b)).and_then(|()| stream.write_all(b"\r\n"))
            }
            Part::Bytes(b) => stream.write_all(&b),
            Part::Wait(d) => {
                std::thread::sleep(d);
                Ok(())
            }
            Part::Hold => {
                let until = Instant::now() + Duration::from_secs(30);
                while !stop.load(Ordering::Relaxed) && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(10));
                }
                return;
            }
            Part::Cut => return,
        };
        if sent.and_then(|()| stream.flush()).is_err() {
            return;
        }
    }
    if reply.chunked {
        let _ = stream.write_all(b"0\r\n\r\n");
    }
    let _ = stream.flush();
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut data = Vec::new();
    let mut chunk = [0u8; 4096];
    let end = loop {
        if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if data.len() > 64 << 10 {
            return None;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        data.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&data[..end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, target) = (first.next()?.to_string(), first.next()?.to_string());
    let (path, query) = target.split_once('?').map_or((target.clone(), String::new()), |(p, q)| (p.to_string(), q.to_string()));
    let headers: Vec<(String, String)> =
        lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string())).collect();
    let length = headers.iter().find(|(k, _)| k == "content-length").and_then(|(_, v)| v.parse::<usize>().ok()).unwrap_or(0);
    let mut body = data[end..].to_vec();
    while body.len() < length.min(32 << 20) {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Some(Request { method, path, query, headers, body })
}

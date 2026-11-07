use std::io::{BufReader, Cursor, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

use super::{Config, Head, Limits, Request, Response, bearer, read_body, read_head, same_secret, serve, write_response};

fn parse(raw: &[u8]) -> Result<(Request, bool), super::Refused> {
    let mut r = BufReader::new(Cursor::new(raw.to_vec()));
    let limits = Limits { head: 512, headers: 8, body: 64 };
    match read_head(&mut r, &limits)? {
        Head::Closed => Err(super::refused(0, "closed")),
        Head::Request(mut req, http10) => {
            read_body(&mut r, &mut req, &limits)?;
            Ok((req, http10))
        }
    }
}

#[test]
fn reads_a_request_with_a_length_or_chunks() {
    let (req, http10) = parse(b"POST /mcp?x=1 HTTP/1.1\r\nHost: 127.0.0.1:9\r\ncontent-length: 5\r\n\r\nhello").unwrap();
    assert_eq!((req.method.as_str(), req.target.as_str(), req.path(), http10), ("POST", "/mcp?x=1", "/mcp", false));
    assert_eq!(req.header("CONTENT-LENGTH"), Some("5"));
    assert_eq!(req.body, b"hello");
    let (req, _) = parse(b"POST /mcp HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nhel\r\n2;x=y\r\nlo\r\n0\r\n\r\n").unwrap();
    assert_eq!(req.body, b"hello");
    let (req, http10) = parse(b"\r\nGET / HTTP/1.0\n\n").unwrap();
    assert_eq!((req.method.as_str(), req.body.len(), http10), ("GET", 0, true));
}

#[test]
fn hostile_requests_are_refused_with_a_status() {
    let status = |raw: &[u8]| parse(raw).map(|_| 0).unwrap_or_else(|e| e.status);
    assert_eq!(status(b"POST /mcp HTTP/1.1\r\nContent-Length: 65\r\n\r\n"), 413, "over the body cap");
    assert_eq!(status(b"POST /mcp HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n41\r\n"), 413, "chunked over the cap");
    assert_eq!(status(b"POST /mcp HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n20\r\n0123456789abcdef0123456789abcdef\r\n20\r\n0123456789abcdef0123456789abcdef\r\n1\r\nx\r\n0\r\n\r\n"), 413, "chunks adding up");
    assert_eq!(status(b"POST /mcp HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\nffffffffffffffffffff\r\n"), 400, "a size that overflows");
    assert_eq!(status(b"POST /mcp HTTP/1.1\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\n"), 400, "smuggling");
    assert_eq!(status(b"POST /mcp HTTP/1.1\r\nTransfer-Encoding: gzip\r\n\r\n"), 501);
    assert_eq!(status(b"POST /mcp HTTP/1.1\r\nContent-Length: -1\r\n\r\n"), 400);
    assert_eq!(status(b"POST /mcp HTTP/1.1\r\nContent-Length: 5\r\n\r\nhi"), 400, "a body that ends early");
    assert_eq!(status(b"GET / HTTP/2\r\n\r\n"), 505);
    assert_eq!(status(b"GET  / HTTP/1.1\r\n\r\n"), 400);
    assert_eq!(status(b"get / HTTP/1.1\r\n\r\n"), 400);
    assert_eq!(status(b"GET http://x/ HTTP/1.1\r\n\r\n"), 400, "absolute targets are for proxies");
    assert_eq!(status(b"GET / HTTP/1.1\r\nX: 1\r\n folded\r\n\r\n"), 400);
    assert_eq!(status(b"GET / HTTP/1.1\r\nBad Name: 1\r\n\r\n"), 400);
    assert_eq!(status(b"GET / HTTP/1.1\r\nNoColon\r\n\r\n"), 400);
    assert_eq!(status(b"GET / HTTP/1.1\r\nHost: x\r\n"), 400, "ends in its headers");
    let many: Vec<u8> = [&b"GET / HTTP/1.1\r\n"[..], &b"A: 1\r\n".repeat(9), b"\r\n"].concat();
    assert_eq!(status(&many), 431, "too many headers");
    let long: Vec<u8> = [&b"GET / HTTP/1.1\r\nA: "[..], &[b'x'; 600], b"\r\n\r\n"].concat();
    assert_eq!(status(&long), 431, "head too large");
    assert_eq!(status(b"\xff\n"), 400, "not UTF-8");
    assert_eq!(status(&b"\r\n".repeat(100)), 400, "only blank lines");
    // Nothing at all: the connection closed, no answer.
    let mut r = BufReader::new(Cursor::new(Vec::new()));
    assert!(matches!(read_head(&mut r, &Limits::default()), Ok(Head::Closed)));
}

#[test]
fn responses_carry_their_length_and_no_injected_headers() {
    let mut out = Vec::new();
    let resp = Response::json(200, "{}").with_header("X-Ok", "1").with_header("X-Bad", "a\r\nInjected: 1").with_header("Content-Length", "99");
    write_response(&mut out, &resp, true).unwrap();
    let s = String::from_utf8(out).unwrap();
    assert!(s.starts_with("HTTP/1.1 200 OK\r\n"), "{s}");
    assert!(s.contains("Content-Type: application/json\r\n") && s.contains("X-Ok: 1\r\n") && s.contains("Content-Length: 2\r\n"));
    assert!(s.contains("Connection: close\r\n") && s.ends_with("\r\n\r\n{}"));
    assert!(!s.contains("Injected") && !s.contains("99"), "{s}");
    let mut out = Vec::new();
    write_response(&mut out, &Response::new(204), false).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "HTTP/1.1 204 No Content\r\n\r\n");
}

#[test]
fn secrets_and_bearer_tokens() {
    assert!(same_secret("abc", "abc"));
    assert!(!same_secret("abc", "abd") && !same_secret("abc", "ab") && !same_secret("", "a"));
    let req = |v: &str| Request { headers: vec![("authorization".into(), v.into())], ..Request::default() };
    assert_eq!(bearer(&req("Bearer k1")), Some("k1"));
    assert_eq!(bearer(&req("bearer  k1 ")), Some("k1"));
    assert_eq!(bearer(&req("Basic k1")), None);
    assert_eq!(bearer(&req("Bearer ")), None);
    assert_eq!(bearer(&Request::default()), None);
}

fn exchange(port: u16, raw: &[u8]) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(raw).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out
}

/// [`exchange`], once the server has a place for it: with one connection allowed, the one before
/// may still hold its place for a moment after it closed (its thread hasn't ended yet).
fn exchange_served(port: u16, raw: &[u8]) -> String {
    for _ in 0..200 {
        let out = exchange(port, raw);
        if !out.starts_with("HTTP/1.1 503") {
            return out;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("never served");
}

#[test]
fn serves_keep_alive_requests_expect_continue_and_a_connection_cap() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = Config { max_connections: 1, ..Config::default() };
    std::thread::spawn(move || {
        serve(listener, config, Arc::new(|r: &Request| Response::text(200, format!("{} {}", r.method, String::from_utf8_lossy(&r.body)))))
    });
    // Two requests on one connection, the second closing it.
    let out = exchange_served(port, b"POST /a HTTP/1.1\r\nContent-Length: 2\r\n\r\nhiGET /b HTTP/1.1\r\nConnection: close\r\n\r\n");
    assert_eq!(out.matches("HTTP/1.1 200 OK").count(), 2, "{out}");
    assert!(out.contains("POST hi") && out.ends_with("GET "), "{out}");
    // Expect: 100-continue gets its interim answer before the body is read.
    let out = exchange_served(port, b"POST /a HTTP/1.1\r\nExpect: 100-continue\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
    assert!(out.starts_with("HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK"), "{out}");
    // A bad request closes the connection after its answer.
    let out = exchange_served(port, b"POST /a HTTP/1.1\r\nContent-Length: x\r\n\r\nGET /b HTTP/1.1\r\n\r\n");
    assert!(out.starts_with("HTTP/1.1 400") && out.matches("HTTP/1.1").count() == 1, "{out}");
    // One connection at a time: a second, while the first is open (and was served, so it holds
    // the place), is answered 503.
    let first = (0..200)
        .find_map(|_| {
            let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
            c.write_all(b"GET /hold HTTP/1.1\r\n\r\n").unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 256];
            while !got.ends_with(b"\r\n\r\nGET ") {
                match c.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => got.extend_from_slice(&buf[..n]),
                }
            }
            if got.starts_with(b"HTTP/1.1 200") {
                return Some(c);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            None
        })
        .expect("the first connection is served");
    let out = exchange(port, b"GET / HTTP/1.1\r\n\r\n");
    assert!(out.starts_with("HTTP/1.1 503"), "{out}");
    drop(first);
}

/// A client that sends a byte now and then, each well within the idle time, is cut off once the
/// request has taken its whole deadline: it can't hold a connection (and the cap) for long.
#[test]
fn a_slow_drip_of_bytes_is_cut_off_at_the_request_deadline() {
    use std::time::{Duration, Instant};
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = Config { idle: Duration::from_millis(400), request: Duration::from_millis(600), ..Config::default() };
    std::thread::spawn(move || serve(listener, config, Arc::new(|_: &Request| Response::text(200, "ok"))));
    for raw in [
        &b"POST /a HTTP/1.1\r\nX-Slow: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"[..],
        &b"POST /a HTTP/1.1\r\nContent-Length: 40\r\n\r\naaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"[..],
    ] {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let started = Instant::now();
        let head = raw.len() - 40;
        s.write_all(&raw[..head]).unwrap();
        // One byte every 100 ms: each read is quick, the whole would take 4 s.
        for b in &raw[head..] {
            std::thread::sleep(Duration::from_millis(100));
            if s.write_all(std::slice::from_ref(b)).is_err() {
                break;
            }
        }
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        assert!(out.starts_with("HTTP/1.1 408"), "{out:?}");
        assert!(started.elapsed() < Duration::from_millis(3000), "{:?}", started.elapsed());
    }
}

/// Connections turned away at the cap that keep trickling bytes don't hold up the accept loop:
/// once a place is free, the next request is answered at once.
#[test]
fn turned_away_connections_dont_hold_up_the_next_one() {
    use std::time::{Duration, Instant};
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = Config { max_connections: 1, ..Config::default() };
    std::thread::spawn(move || serve(listener, config, Arc::new(|_: &Request| Response::text(200, "ok"))));
    let first = TcpStream::connect(("127.0.0.1", port)).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    // Three turned away, each sending a byte every 100 ms for two seconds.
    let tricklers: Vec<_> = (0..3)
        .map(|_| {
            std::thread::spawn(move || {
                let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) else { return };
                for _ in 0..20 {
                    if s.write_all(b"x").is_err() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
        })
        .collect();
    std::thread::sleep(Duration::from_millis(300));
    drop(first);
    std::thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    let out = exchange(port, b"GET / HTTP/1.1\r\nConnection: close\r\n\r\n");
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    assert!(started.elapsed() < Duration::from_millis(700), "waited {:?}", started.elapsed());
    for t in tricklers {
        t.join().unwrap();
    }
}

/// A connection's place under the cap is given back when its thread ends, and when no thread
/// could be started for it (the closure that would have run is dropped).
#[test]
fn a_connection_s_place_is_given_back_even_when_its_thread_never_starts() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let open = Arc::new(AtomicUsize::new(2));
    let never_run = {
        let slot = super::Slot(open.clone());
        move || drop(slot)
    };
    drop(never_run);
    assert_eq!(open.load(Ordering::SeqCst), 1);
    let slot = super::Slot(open.clone());
    std::thread::spawn(move || {
        let _slot = slot;
    })
    .join()
    .unwrap();
    assert_eq!(open.load(Ordering::SeqCst), 0);
}

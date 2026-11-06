//! #25: `wordcraft-cli call`, against a stand-in for WordCraft's control port.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use serde_json::{Value, json};

const KEY: &str = "the-right-key-0123456789abcdef";

/// A control port on 127.0.0.1 that answers each request line with what `answer` makes of it,
/// and passes the requests on.
struct Port {
    addr: String,
    got: Receiver<Value>,
}

impl Port {
    fn new(answer: impl Fn(&Value) -> String + Send + 'static) -> Port {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (tx, got) = channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
                    let request: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
                    let reply = answer(&request);
                    let _ = tx.send(request);
                    if writeln!(stream, "{reply}").is_err() {
                        break;
                    }
                    line.clear();
                }
            }
        });
        Port { addr, got }
    }

    /// Like WordCraft: the right key gets `result`, any other is unauthorized.
    fn app(result: Value) -> Port {
        Port::new(move |r| {
            if r["key"] == KEY {
                json!({"id": r["id"], "ok": true, "result": result}).to_string()
            } else {
                json!({"id": r["id"], "ok": false, "error": "unauthorized"}).to_string()
            }
        })
    }

    /// The next request it got.
    fn request(&self) -> Value {
        self.got.recv_timeout(Duration::from_secs(20)).unwrap()
    }
}

/// `wordcraft-cli call` with `args`, `env` on top of an environment with no key or address of
/// its own and an empty profile, and `input` on its stdin.
fn call(args: &[&str], env: &[(&str, &str)], input: &str) -> Output {
    let profile = std::env::temp_dir().join(format!("wordcraft-call-profile-{}", std::process::id()));
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_wordcraft-cli"));
    cmd.arg("call")
        .args(args)
        .env_remove("WORDCRAFT_CONTROL_KEY")
        .env_remove("WORDCRAFT_CONTROL_ADDR")
        .env("WORDCRAFT_PROFILE_DIR", &profile)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let _ = stdin.write_all(input.as_bytes());
    drop(stdin);
    child.wait_with_output().unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// It failed with status 1 and an error, not a panic, and printed nothing.
fn failed(out: &Output) -> String {
    let err = stderr(out);
    assert_eq!(out.status.code(), Some(1), "stderr: {err}");
    assert!(!err.contains("panicked") && err.starts_with("wordcraft-cli: call: "), "{err}");
    assert_eq!(stdout(out), "");
    err
}

#[test]
fn the_reply_is_printed_as_json() {
    let port = Port::app(json!({"text": "one two three", "n": 3}));
    let out = call(&["document.text", "--connect", &port.addr], &[("WORDCRAFT_CONTROL_KEY", KEY)], "");
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(serde_json::from_str::<Value>(&stdout(&out)).unwrap(), json!({"text": "one two three", "n": 3}));
    let request = port.request();
    assert_eq!((&request["method"], &request["params"], &request["key"]), (&json!("document.text"), &json!({}), &json!(KEY)));

    // Params as an argument, the address from the environment.
    let params = json!({"command": "text.insert", "params": {"text": "it's \"two\"", "raw": true}});
    let text = params.to_string();
    let env = [("WORDCRAFT_CONTROL_KEY", KEY), ("WORDCRAFT_CONTROL_ADDR", port.addr.as_str())];
    let out = call(&["engine.execute", text.as_str()], &env, "");
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(port.request()["params"], params);

    // Params on stdin, and --connect over the environment's address.
    let env = [("WORDCRAFT_CONTROL_KEY", KEY), ("WORDCRAFT_CONTROL_ADDR", "127.0.0.1:9")];
    let out = call(&["engine.execute", "-", "--connect", &port.addr], &env, &text);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(port.request()["params"], params);

    // localhost is the app's 127.0.0.1.
    let localhost = port.addr.replace("127.0.0.1", "localhost");
    let out = call(&["document.text", "--connect", &localhost], &[("WORDCRAFT_CONTROL_KEY", KEY)], "");
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn a_wrong_key_or_none_is_refused() {
    let port = Port::app(json!({"text": "secret"}));
    let out = call(&["document.text", "--connect", &port.addr], &[("WORDCRAFT_CONTROL_KEY", "a-wrong-key")], "");
    assert!(failed(&out).contains("unauthorized"));
    assert_eq!(port.request()["key"], json!("a-wrong-key"));
    // No key, and no key file for the port in the profile.
    let out = call(&["document.text", "--connect", &port.addr], &[], "");
    assert!(failed(&out).contains("no control key"));
}

#[test]
fn the_key_goes_to_wordcrafts_own_address_only() {
    let port = Port::app(json!({}));
    let at = port.addr.rsplit_once(':').unwrap().1;
    let near = [format!("[::1]:{at}"), format!("127.0.0.2:{at}"), format!("0.0.0.0:{at}")];
    for addr in ["192.0.2.1:7981", "example.com:80"].into_iter().chain(near.iter().map(String::as_str)) {
        let out = call(&["document.text", "--connect", addr], &[("WORDCRAFT_CONTROL_KEY", KEY)], "");
        assert!(failed(&out).contains("isn't WordCraft's address"), "{addr}");
        let env = [("WORDCRAFT_CONTROL_KEY", KEY), ("WORDCRAFT_CONTROL_ADDR", addr)];
        assert!(failed(&call(&["document.text"], &env, "")).contains("isn't WordCraft's address"), "{addr}");
    }
    assert!(port.got.recv_timeout(Duration::from_millis(200)).is_err(), "a request reached the port");
}

#[test]
fn junk_arguments_are_an_error_never_a_panic() {
    let port = Port::app(json!({}));
    let env = [("WORDCRAFT_CONTROL_KEY", KEY), ("WORDCRAFT_CONTROL_ADDR", port.addr.as_str())];
    for params in ["{nope", "[1, 2]", "\"text\"", "null", "", "{\"a\": 1} {\"b\": 2}"] {
        assert!(failed(&call(&["engine.execute", params], &env, "")).contains("params"), "{params:?}");
    }
    for input in ["\u{FEFF}{", "{\"text\": \"x\"", "\n"] {
        assert!(failed(&call(&["engine.execute", "-"], &env, input)).contains("params"), "{input:?}");
    }
    let long = "m".repeat(10_000);
    let cases: [&[&str]; 7] = [&[], &["  "], &["a\u{7}b"], &[long.as_str()], &["a", "{}", "c"], &["a", "--connect"], &["a", "--frob"]];
    for args in cases {
        failed(&call(args, &env, ""));
    }
    for addr in ["nowhere", "127.0.0.1:", "127.0.0.1:99999", ":80", "127.0.0.1:x"] {
        assert!(failed(&call(&["a", "--connect", addr], &env, "")).contains("HOST:PORT"), "{addr}");
    }
    // Nothing listening there.
    let closed = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
    assert!(failed(&call(&["a", "--connect", &closed], &env, "")).contains("isn't reachable"));
    assert!(port.got.recv_timeout(Duration::from_millis(200)).is_err(), "a junk call reached the port");
}

#[test]
fn junk_replies_are_an_error_never_a_panic() {
    let env = [("WORDCRAFT_CONTROL_KEY", KEY)];
    for reply in ["garbage", "[]", "{\"ok\": false}", "{\"ok\": \"yes\"}", "\u{0}\u{1b}[2J"] {
        let port = Port::new(move |_| reply.to_string());
        failed(&call(&["a", "--connect", &port.addr], &env, ""));
    }
    // An error that would drive the terminal is shown escaped, on one line.
    let port = Port::new(|_| json!({"ok": false, "error": "bad\u{1b}[31m\nnews\u{202e}"}).to_string());
    let err = failed(&call(&["a", "--connect", &port.addr], &env, ""));
    assert!(err.contains("bad\\u{1b}[31m\\nnews\\u{202e}") && err.trim_end().lines().count() == 1, "{err:?}");
    // An answer with no result is null.
    let port = Port::new(|r| json!({"id": r["id"], "ok": true}).to_string());
    let out = call(&["a", "--connect", &port.addr], &env, "");
    assert_eq!((out.status.success(), stdout(&out).trim()), (true, "null"));
}

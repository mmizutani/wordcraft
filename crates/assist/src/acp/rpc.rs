//! An ACP agent's process and the JSON-RPC 2.0 lines it speaks on its stdin and stdout.
//!
//! Three threads per process, so the conversation's thread never blocks on the agent: one reads
//! its stdout a bounded line at a time and hands each message over ([`Incoming`]), one keeps
//! the tail of its stderr for error messages, and one writes what WordCraft sends. Everything
//! the agent writes is hostile input: a line that is too long, isn't UTF-8 or isn't a JSON
//! object is skipped, and a flood of messages waits for the conversation to read them.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wordcraft_mcp::server::{BoundedLine, read_bounded_line};

use super::Launch;

/// Longest line read from an agent; longer ones are skipped.
pub const MAX_LINE: usize = 4 << 20;

/// Most bytes of an agent's stderr kept for error messages.
pub const STDERR_TAIL: usize = 8 << 10;

/// Messages from the agent waiting for the conversation to read them; the reader waits when
/// there are this many.
const QUEUE: usize = 256;

/// Lines waiting to be written to the agent; one more is refused (the agent isn't reading).
const OUTBOX: usize = 64;

/// JSON-RPC's error for a method the client doesn't offer.
pub const METHOD_NOT_FOUND: i64 = -32601;

/// ACP's error for a session that needs the person to sign in first.
pub const AUTH_REQUIRED: i64 = -32000;

/// What the agent's stdout gave.
#[derive(Debug)]
pub enum Incoming {
    /// A JSON object: a request, a notification or a response.
    Message(serde_json::Map<String, Value>),
    /// A line that was skipped, and why.
    Junk(&'static str),
    /// Its stdout closed: the agent exited, or closed it.
    Closed,
}

/// One JSON-RPC message from the agent, sorted.
#[derive(Debug, PartialEq)]
pub enum Message<'a> {
    /// A request the agent sends WordCraft, which answers it with `id`.
    Request { id: &'a Value, method: &'a str, params: &'a Value },
    /// A notification.
    Notification { method: &'a str, params: &'a Value },
    /// The answer to one of WordCraft's requests.
    Response { id: &'a Value, result: Result<&'a Value, RpcError> },
    /// Not a JSON-RPC message.
    Invalid,
}

/// An error answer to one of WordCraft's requests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    /// The agent's message, as a label.
    pub message: String,
}

/// Sort `m` into a [`Message`].
pub fn classify(m: &serde_json::Map<String, Value>) -> Message<'_> {
    static NULL: Value = Value::Null;
    let params = m.get("params").unwrap_or(&NULL);
    match (m.get("method").and_then(Value::as_str), m.get("id")) {
        (Some(method), Some(id)) => Message::Request { id, method, params },
        (Some(method), None) => Message::Notification { method, params },
        (None, Some(id)) => {
            if let Some(e) = m.get("error") {
                let code = e.get("code").and_then(Value::as_i64).unwrap_or(0);
                let message = crate::backend::label(e.get("message").and_then(Value::as_str).unwrap_or("the agent gave no reason"));
                Message::Response { id, result: Err(RpcError { code, message }) }
            } else if let Some(result) = m.get("result") {
                Message::Response { id, result: Ok(result) }
            } else {
                Message::Invalid
            }
        }
        (None, None) => Message::Invalid,
    }
}

/// The last bytes an agent wrote to its stderr.
#[derive(Default)]
struct Tail {
    bytes: VecDeque<u8>,
    /// Whether its stderr closed: nothing more comes.
    done: bool,
}

impl Tail {
    fn push(&mut self, bytes: &[u8]) {
        let keep = bytes.get(bytes.len().saturating_sub(STDERR_TAIL)..).unwrap_or_default();
        let over = (self.bytes.len() + keep.len()).saturating_sub(STDERR_TAIL);
        self.bytes.drain(..over.min(self.bytes.len()));
        self.bytes.extend(keep);
    }
}

/// A running agent.
pub struct Process {
    child: Child,
    /// Lines for the writer thread; `None` once WordCraft closed the agent's stdin.
    outbox: Option<SyncSender<String>>,
    incoming: Receiver<Incoming>,
    stderr: Arc<Mutex<Tail>>,
    next_id: i64,
    /// Whether its stdout closed.
    closed: bool,
    /// Whether it was waited for.
    reaped: Option<ExitStatus>,
}

impl Process {
    /// Start `launch` in `cwd` with exactly `env` (nothing else of WordCraft's environment), its
    /// stdin, stdout and stderr piped. On Unix it leads a process group of its own, which
    /// [`Process::stop`] ends as a whole (an adapter run through `npx` is a few processes).
    pub fn spawn(launch: &Launch, cwd: &Path, env: Vec<(OsString, OsString)>) -> std::io::Result<Process> {
        let mut command = Command::new(&launch.program);
        command.args(&launch.args).current_dir(cwd).env_clear().envs(env).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut child = command.spawn()?;
        let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::other("the agent's stdin, stdout or stderr couldn't be piped"));
        };
        let (incoming_tx, incoming) = sync_channel(QUEUE);
        let (outbox, lines) = sync_channel::<String>(OUTBOX);
        let tail = Arc::new(Mutex::new(Tail::default()));
        let started = spawn_thread("agent stdout", move || read_stdout(stdout, &incoming_tx))
            .and_then(|()| {
                let tail = tail.clone();
                spawn_thread("agent stderr", move || read_stderr(stderr, &tail))
            })
            .and_then(|()| spawn_thread("agent stdin", move || write_stdin(stdin, &lines)));
        if let Err(e) = started {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
        Ok(Process { child, outbox: Some(outbox), incoming, stderr: tail, next_id: 0, closed: false, reaped: None })
    }

    /// Send request `method` with `params`: its id, which the answer carries.
    pub fn request(&mut self, method: &str, params: Value) -> Result<i64, String> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        Ok(id)
    }

    /// Send notification `method` with `params`.
    pub fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    /// Answer the agent's request `id` with `result`.
    pub fn respond(&mut self, id: &Value, result: Value) -> Result<(), String> {
        self.send(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    /// Answer the agent's request `id` with an error.
    pub fn respond_error(&mut self, id: &Value, code: i64, message: &str) -> Result<(), String> {
        self.send(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
    }

    fn send(&mut self, message: Value) -> Result<(), String> {
        let Some(outbox) = &self.outbox else { return Err("the agent's input is closed".into()) };
        match outbox.try_send(message.to_string()) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err("the agent isn't reading what WordCraft sends it".into()),
            Err(TrySendError::Disconnected(_)) => Err("the agent's input is closed".into()),
        }
    }

    /// The next thing its stdout gave, waiting at most `timeout`; `None` when nothing came.
    pub fn next(&mut self, timeout: Duration) -> Option<Incoming> {
        if self.closed {
            return Some(Incoming::Closed);
        }
        match self.incoming.recv_timeout(timeout) {
            Ok(Incoming::Closed) | Err(RecvTimeoutError::Disconnected) => {
                self.closed = true;
                Some(Incoming::Closed)
            }
            Ok(i) => Some(i),
            Err(RecvTimeoutError::Timeout) => None,
        }
    }

    /// Whether the agent still runs: its stdout is open and it hasn't exited.
    pub fn is_running(&mut self) -> bool {
        !self.closed && self.exit_status().is_none()
    }

    /// How it exited, if it has.
    pub fn exit_status(&mut self) -> Option<ExitStatus> {
        if self.reaped.is_none() {
            #[cfg(all(unix, not(target_os = "openbsd")))]
            {
                use rustix::process::{WaitId, WaitIdOptions, waitid};
                let pid = rustix::process::Pid::from_child(&self.child);
                if !matches!(waitid(WaitId::Pid(pid), WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT), Ok(Some(_))) {
                    return None;
                }
                // Keep the exited leader's PID reserved until its descendants have been killed.
                let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
            }
            #[cfg(not(target_os = "openbsd"))]
            {
                self.reaped = self.child.try_wait().ok().flatten();
            }
            // shortcut: OpenBSD has no nonreaping waitid; retain the leader until stop,
            // and add nonreaping exit detection if prompt-level detection is needed there.
        }
        self.reaped
    }

    /// The end of what it wrote to stderr, as text without control characters but newlines, at
    /// most `max` characters.
    pub fn stderr_tail(&self, max: usize) -> String {
        let bytes: Vec<u8> = self.stderr.lock().unwrap_or_else(PoisonError::into_inner).bytes.iter().copied().collect();
        let text = String::from_utf8_lossy(&bytes);
        let text: String = text.chars().filter(|c| *c == '\n' || !c.is_control()).collect();
        let text = text.trim();
        let skip = text.chars().count().saturating_sub(max);
        text.chars().skip(skip).collect()
    }

    /// Why it stopped, said plainly with `name`: how it exited and the last lines of its stderr.
    pub fn why_gone(&mut self, name: &str) -> String {
        // It may close its stdout a moment before it exits, and its last words may still be on
        // their way.
        let until = Instant::now() + Duration::from_millis(500);
        while (self.exit_status().is_none() || !self.stderr.lock().unwrap_or_else(PoisonError::into_inner).done) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        let how = match self.exit_status() {
            Some(s) if s.success() => format!("{name} stopped"),
            Some(s) => format!("{name} stopped ({s})"),
            None => format!("{name} closed its output"),
        };
        let tail = self.stderr_tail(600);
        let mut lines: Vec<&str> = tail.lines().map(str::trim).filter(|l| !l.is_empty()).rev().take(6).collect();
        lines.reverse();
        if lines.is_empty() { how } else { format!("{how}: {}", lines.join(" / ")) }
    }

    /// End the agent: close its stdin (an ACP agent exits when its input ends), wait up to
    /// `grace` for it to exit, then kill it (on Unix, its whole process group), and wait for it.
    pub fn stop(&mut self, grace: Duration) {
        self.outbox = None;
        let until = Instant::now() + grace;
        while self.exit_status().is_none() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        if self.reaped.is_none() {
            // The unreaped leader reserves its PID even when it has already exited.
            #[cfg(unix)]
            {
                let pid = rustix::process::Pid::from_child(&self.child);
                let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
            }
            let _ = self.child.kill();
            self.reaped = self.child.wait().ok();
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stop(Duration::from_millis(500));
    }
}

fn spawn_thread(name: &str, f: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    std::thread::Builder::new().name(name.to_string()).spawn(f).map(|_| ())
}

/// The stdout thread: each line to `tx`, until the stream ends or nobody reads any more.
fn read_stdout(stdout: impl Read, tx: &SyncSender<Incoming>) {
    let mut reader = BufReader::with_capacity(64 << 10, stdout);
    loop {
        let incoming = match read_bounded_line(&mut reader, MAX_LINE) {
            Ok(BoundedLine::Eof) => break,
            Ok(BoundedLine::TooLong) => Incoming::Junk("a line longer than WordCraft reads"),
            Ok(BoundedLine::Line(line)) if line.trim().is_empty() => continue,
            Ok(BoundedLine::Line(line)) => match serde_json::from_str::<Value>(&line) {
                Ok(Value::Object(m)) => Incoming::Message(m),
                Ok(_) => Incoming::Junk("a line that isn't a JSON object"),
                Err(_) => Incoming::Junk("a line that isn't JSON"),
            },
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => Incoming::Junk("a line that isn't UTF-8"),
            Err(_) => break,
        };
        if tx.send(incoming).is_err() {
            return;
        }
    }
    let _ = tx.send(Incoming::Closed);
}

/// The stderr thread: keep its tail.
fn read_stderr(mut stderr: impl Read, tail: &Mutex<Tail>) {
    let mut buf = [0u8; 4096];
    loop {
        match stderr.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => tail.lock().unwrap_or_else(PoisonError::into_inner).push(buf.get(..n).unwrap_or_default()),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    tail.lock().unwrap_or_else(PoisonError::into_inner).done = true;
}

/// The stdin thread: each line WordCraft sends, until it closes the agent's input or the agent
/// stops reading.
fn write_stdin(mut stdin: ChildStdin, lines: &Receiver<String>) {
    while let Ok(line) = lines.recv() {
        if stdin.write_all(line.as_bytes()).and_then(|()| stdin.write_all(b"\n")).and_then(|()| stdin.flush()).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Message, RpcError, Tail, classify};
    use serde_json::{Value, json};

    fn obj(v: Value) -> serde_json::Map<String, Value> {
        v.as_object().cloned().unwrap()
    }

    #[test]
    fn messages_sort_into_requests_notifications_and_responses() {
        let r = obj(json!({"jsonrpc": "2.0", "id": 3, "method": "fs/read_text_file", "params": {"path": "/etc/passwd"}}));
        assert!(matches!(classify(&r), Message::Request { method: "fs/read_text_file", .. }));
        let n = obj(json!({"method": "session/update"}));
        assert_eq!(classify(&n), Message::Notification { method: "session/update", params: &Value::Null });
        let ok = obj(json!({"id": 1, "result": {"stopReason": "end_turn"}}));
        assert!(matches!(classify(&ok), Message::Response { result: Ok(_), .. }));
        let err = obj(json!({"id": 1, "error": {"code": -32000, "message": "Authentication\u{7} required"}}));
        assert_eq!(
            classify(&err),
            Message::Response { id: &json!(1), result: Err(RpcError { code: -32000, message: "Authentication required".into() }) }
        );
        let junk = obj(json!({"id": {"nested": [1]}, "error": "not an object"}));
        assert!(matches!(classify(&junk), Message::Response { result: Err(RpcError { code: 0, .. }), .. }));
        for bad in [json!({}), json!({"id": 1}), json!({"method": 5, "id": 1}), json!({"result": 1})] {
            assert_eq!(classify(&obj(bad.clone())), Message::Invalid, "{bad}");
        }
    }

    #[cfg(all(unix, not(target_os = "openbsd")))]
    #[test]
    fn stopping_an_exited_launcher_also_closes_its_workers_pipes() {
        use super::{Incoming, Process};
        use crate::acp::Launch;
        use std::time::{Duration, Instant};
        for on_eof in [false, true] {
            let script = if on_eof {
                "sleep 30 & printf '{\"worker\":%s}\\n' \"$!\"; cat >/dev/null"
            } else {
                "sleep 30 & printf '{\"worker\":%s}\\n' \"$!\"; exit 0"
            };
            let launch = Launch::new("/bin/sh", ["-c", script]);
            let mut process = Process::spawn(&launch, &std::env::temp_dir(), vec![("PATH".into(), "/bin:/usr/bin".into())]).unwrap();
            let Some(Incoming::Message(report)) = process.next(Duration::from_secs(5)) else { panic!("no worker report") };
            let worker = rustix::process::Pid::from_raw(i32::try_from(report["worker"].as_u64().unwrap()).unwrap()).unwrap();
            if !on_eof {
                let until = Instant::now() + Duration::from_secs(5);
                while process.exit_status().is_none() {
                    assert!(Instant::now() < until, "launcher did not exit");
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            process.stop(Duration::from_secs(1));
            let closed = matches!(process.next(Duration::from_secs(1)), Some(Incoming::Closed));
            // Clean up even when the regression fails.
            let _ = rustix::process::kill_process(worker, rustix::process::Signal::KILL);
            assert!(closed, "the launcher's descendant kept stdout open (on EOF: {on_eof})");
        }
    }

    #[test]
    fn the_stderr_tail_keeps_the_last_bytes_only() {
        let mut t = Tail::default();
        for _ in 0..100 {
            t.push(&[b'a'; 1000]);
        }
        t.push(b"the end");
        assert_eq!(t.bytes.len(), super::STDERR_TAIL);
        assert!(t.bytes.iter().rev().take(7).eq(b"the end".iter().rev()));
        t.push(&vec![b'z'; 3 * super::STDERR_TAIL]);
        assert_eq!(t.bytes.len(), super::STDERR_TAIL);
        assert!(t.bytes.iter().all(|b| *b == b'z'));
    }
}

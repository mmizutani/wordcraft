//! Server-sent events, read with bounds: the Responses API streams its answer as `data:` lines
//! of JSON, an event per blank-line-ended block.
//!
//! A line or an event longer than [`MAX_EVENT`], or a stream longer than [`MAX_STREAM`], is an
//! error; comments (`:` lines, keep-alives) and the `event`, `id` and `retry` fields are skipped
//! (each event's JSON says its type). The last event counts even without its blank line.

use std::io::{BufRead, BufReader, Read};

use super::http::redact;

/// Most bytes of one event (its data lines together), and of one line.
pub(crate) const MAX_EVENT: usize = 4 << 20;

/// Most bytes of one stream.
pub(crate) const MAX_STREAM: u64 = 64 << 20;

/// The events of a stream, one by one.
pub(crate) struct Events<R> {
    reader: BufReader<R>,
    /// Bytes read so far.
    read: u64,
    /// The data of the event being read.
    data: Vec<u8>,
    has_data: bool,
    ended: bool,
}

impl<R: Read> Events<R> {
    pub(crate) fn new(reader: R) -> Events<R> {
        Events { reader: BufReader::with_capacity(64 << 10, reader), read: 0, data: Vec::new(), has_data: false, ended: false }
    }

    /// The next event's data: None once the stream ended; an error when it can't be read or
    /// is too long.
    pub(crate) fn next_event(&mut self) -> Result<Option<String>, String> {
        while !self.ended {
            let Some(line) = self.line()? else {
                self.ended = true;
                break;
            };
            if line.is_empty() {
                if let Some(data) = self.take() {
                    return Ok(Some(data));
                }
                continue;
            }
            let (field, value) = match line.iter().position(|&b| b == b':') {
                // A comment.
                Some(0) => continue,
                Some(i) => (line.get(..i).unwrap_or_default(), line.get(i + 1..).unwrap_or_default()),
                None => (line.as_slice(), &[][..]),
            };
            if field == b"data" {
                let value = value.strip_prefix(b" ").unwrap_or(value);
                if self.data.len().saturating_add(value.len()).saturating_add(1) > MAX_EVENT {
                    return Err(format!("an event of the answer is longer than {} MB", MAX_EVENT >> 20));
                }
                if self.has_data {
                    self.data.push(b'\n');
                }
                self.data.extend_from_slice(value);
                self.has_data = true;
            }
        }
        Ok(self.take())
    }

    /// The data of the event read so far, if it has any.
    fn take(&mut self) -> Option<String> {
        if !self.has_data {
            return None;
        }
        self.has_data = false;
        Some(String::from_utf8_lossy(&std::mem::take(&mut self.data)).into_owned())
    }

    /// The next line without its end (`\n` or `\r\n`), or None at the stream's end.
    fn line(&mut self) -> Result<Option<Vec<u8>>, String> {
        let mut line = Vec::new();
        loop {
            let buf = match self.reader.fill_buf() {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(format!("the answer couldn't be read: {}", redact(&e.to_string(), &[]))),
            };
            if buf.is_empty() {
                return Ok(if line.is_empty() { None } else { Some(line) });
            }
            let (take, found) = match buf.iter().position(|&b| b == b'\n') {
                Some(i) => (i.saturating_add(1), true),
                None => (buf.len(), false),
            };
            if line.len().saturating_add(take) > MAX_EVENT.saturating_add(2) {
                return Err(format!("a line of the answer is longer than {} MB", MAX_EVENT >> 20));
            }
            line.extend_from_slice(buf.get(..take).unwrap_or_default());
            self.reader.consume(take);
            self.read = self.read.saturating_add(u64::try_from(take).unwrap_or(u64::MAX));
            if self.read > MAX_STREAM {
                return Err(format!("the answer is longer than {} MB", MAX_STREAM >> 20));
            }
            if found {
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(Some(line));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(bytes: &[u8]) -> Result<Vec<String>, String> {
        let mut events = Events::new(bytes);
        let mut out = Vec::new();
        while let Some(e) = events.next_event()? {
            out.push(e);
        }
        Ok(out)
    }

    #[test]
    fn events_are_read_with_their_data_lines_joined_and_the_rest_skipped() {
        let stream = b": keep-alive\n\nevent: response.created\nid: 1\nretry: 10\ndata: {\"a\":1}\n\ndata:{\"b\":2}\r\ndata: x\r\n\r\n\n\ndata: [DONE]\n\nfield-without-colon\ndata: last";
        assert_eq!(all(stream).unwrap(), ["{\"a\":1}", "{\"b\":2}\nx", "[DONE]", "last"]);
        assert_eq!(all(b"").unwrap(), Vec::<String>::new());
        assert_eq!(all(b"\n\n\n: only comments\n").unwrap(), Vec::<String>::new());
        assert_eq!(all(b"data:\n\n").unwrap(), [""]);
        assert_eq!(all(b"data: \xff\xfe\n\n").unwrap(), ["\u{fffd}\u{fffd}"]);
        // Once it ended, it stays ended.
        let mut events = Events::new(&b"data: a\n\n"[..]);
        assert_eq!(events.next_event().unwrap().as_deref(), Some("a"));
        assert_eq!(events.next_event().unwrap(), None);
        assert_eq!(events.next_event().unwrap(), None);
    }

    #[test]
    fn huge_lines_events_and_streams_are_errors() {
        let line = [b"data: ".to_vec(), vec![b'x'; MAX_EVENT + 10]].concat();
        assert!(all(&line).unwrap_err().contains("longer than"));
        let no_newline = vec![b'y'; MAX_EVENT + 10];
        assert!(all(&no_newline).unwrap_err().contains("longer than"));
        let many_lines = format!("data: {}\n", "z".repeat(1 << 20)).repeat(5);
        assert!(all(many_lines.as_bytes()).unwrap_err().contains("event"));
        let event = format!("data: {}\n\n", "w".repeat(1 << 20));
        assert!(all(event.repeat(70).as_bytes()).unwrap_err().contains("MB"));
        assert_eq!(all(event.repeat(3).as_bytes()).unwrap().len(), 3);
    }
}

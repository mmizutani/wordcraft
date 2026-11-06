//! Server-sent events, read with bounds: the Responses API streams its answer as `data:` lines
//! of JSON, an event per blank-line-ended block.
//!
//! A line or an event longer than [`MAX_EVENT`], or a stream longer than [`MAX_STREAM`], is an
//! error; comments (`:` lines, keep-alives) and the `event`, `id` and `retry` fields are skipped
//! (each event's JSON says its type). The last event counts even without its blank line. The
//! reader's caller is asked before each read whether to go on, so a stream that sends only
//! comments, or a byte at a time, can still be stopped.

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

    /// The next event's data: None once the stream ended; an error when it can't be read, is
    /// too long, or `watch` (asked before each read) says to stop.
    pub(crate) fn next_event(&mut self, watch: &mut impl FnMut() -> Result<(), String>) -> Result<Option<String>, String> {
        while !self.ended {
            let Some(line) = self.line(watch)? else {
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
    fn line(&mut self, watch: &mut impl FnMut() -> Result<(), String>) -> Result<Option<Vec<u8>>, String> {
        let mut line = Vec::new();
        loop {
            watch()?;
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

    fn go_on() -> Result<(), String> {
        Ok(())
    }

    fn all(bytes: &[u8]) -> Result<Vec<String>, String> {
        let mut events = Events::new(bytes);
        let mut out = Vec::new();
        while let Some(e) = events.next_event(&mut go_on)? {
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
        assert_eq!(events.next_event(&mut go_on).unwrap().as_deref(), Some("a"));
        assert_eq!(events.next_event(&mut go_on).unwrap(), None);
        assert_eq!(events.next_event(&mut go_on).unwrap(), None);
    }

    /// A stream that sends one byte at a time, without end.
    struct Trickle(u8);

    impl Read for Trickle {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match buf.first_mut() {
                Some(b) => {
                    *b = self.0;
                    Ok(1)
                }
                None => Ok(0),
            }
        }
    }

    #[test]
    fn the_watch_is_asked_before_every_read_even_between_comments() {
        // Blank lines without end, or a comment and then a line a byte at a time: the watch
        // still stops it.
        for endless in [Box::new(std::io::repeat(b'\n')) as Box<dyn Read>, Box::new(b": keep-alive\n".as_slice().chain(Trickle(b'x')))] {
            let mut events = Events::new(endless);
            let mut asked = 0;
            let mut watch = || {
                asked += 1;
                if asked > 1000 { Err("stopped".to_string()) } else { Ok(()) }
            };
            assert_eq!(events.next_event(&mut watch), Err("stopped".to_string()));
        }
        // It is asked before the event it lets through, too.
        let mut events = Events::new(&b"data: a\n\n"[..]);
        assert_eq!(events.next_event(&mut || Err("no".to_string())), Err("no".to_string()));
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

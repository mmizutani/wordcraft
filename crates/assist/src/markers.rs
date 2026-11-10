//! What the model writes in its answer for WordCraft rather than for the person (#86, #107): the
//! markers `[[remember: …]]`, a proposal to keep a preference, `[[used: m1]]`, which preference or
//! recalled item shaped this answer, and `[[case]]{json}[[/case]]`, a note for the conversation's
//! case. A marker works on every backend, adds no route into the window and costs no extra
//! request, and it carries no authority of its own: a document can make a model write one, so a
//! proposal is a card the person answers, and a case note is stored unconfirmed in the
//! conversation's own case and nowhere else ([`crate::cases::marker`]).
//!
//! Markers are parsed from answers only, never from thoughts, and stripped from what the pane
//! shows, including while one is split across pieces of the stream. Inside code, fenced or inline,
//! a marker is an example the answer explains: shown as written, never acted on.

const REMEMBER: &str = "[[remember:";
const USED: &str = "[[used:";
const CLOSE: &str = "]]";
const CASE: &str = "[[case]]";
/// A case marker closes with its own tag, so its JSON may hold `]]`.
const CASE_CLOSE: &str = "[[/case]]";

/// Most bytes held back waiting for a marker to end: what starts like one and goes on longer isn't.
const MAX_HELD: usize = 2 << 10;

/// Most bytes of a case marker's body, between its tags. Past this it is dropped and counted,
/// never shown: half a note in the answer would read as text.
pub const MAX_CASE: usize = 8 << 10;

/// A piece of an answer, in the order it came.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Piece {
    Text(String),
    Remember(String),
    Used(String),
    /// The body of a `[[case]]…[[/case]]` marker, unparsed.
    Case(String),
    /// A case marker too long to keep, or unclosed when the answer ended.
    CaseDropped,
}

/// The start of a marker held back until the rest of it comes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Markers {
    held: String,
    /// A case marker ran past [`MAX_CASE`]: everything up to its close is thrown away, and this
    /// is where the scan of its body stood when the held text was cut to a tag's start.
    dropping: Option<Scan>,
    code: Code,
}

/// Where a case marker's body stands: its close counts only outside a JSON string and at the
/// nesting depth it opened at, so `"[[/case]]"` in a note's text doesn't end it and a marker
/// opened inside another is one body, dropped whole by the parser.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Scan {
    in_string: bool,
    escaped: bool,
    depth: u32,
}

impl Scan {
    /// Where in `text` the body's close starts, scanning on from this state; `None` after taking
    /// in all of `text`.
    fn find_close(&mut self, text: &str) -> Option<usize> {
        let bytes = text.as_bytes();
        let mut i = 0;
        while let Some(rest) = bytes.get(i..).filter(|rest| !rest.is_empty()) {
            if !self.in_string {
                if rest.starts_with(CASE_CLOSE.as_bytes()) {
                    if self.depth == 0 {
                        return Some(i);
                    }
                    self.depth = self.depth.saturating_sub(1);
                    i = i.saturating_add(CASE_CLOSE.len());
                    continue;
                }
                if rest.starts_with(CASE.as_bytes()) {
                    self.depth = self.depth.saturating_add(1);
                    i = i.saturating_add(CASE.len());
                    continue;
                }
            }
            match (self.in_string, self.escaped, rest.first()) {
                (true, true, _) => self.escaped = false,
                (true, false, Some(b'\\')) => self.escaped = true,
                (true, false, Some(b'"')) => self.in_string = false,
                (false, _, Some(b'"')) => self.in_string = true,
                _ => {}
            }
            i = i.saturating_add(1);
        }
        None
    }
}

/// Whether the answer's text stands inside code, where a marker is an example.
/// Which code the answer so far left open, as Markdown reads it (the renderer's `Fences` rule): a
/// fence opens on a line starting with three or more backticks or tildes and closes only on a line
/// of nothing but at least as many of the same character; inline code closes on a backtick run of
/// its own length. A marker inside code is an example, never a write.
/// ponytail: an indented code block isn't tracked, and a stray backtick keeps inline code open until
/// a run of its length; both only ever stop a write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Code {
    open: Open,
    /// Nothing but spaces so far on this line, and how many.
    at_start: bool,
    indent: u8,
    /// This line so far is a fence's closing run and spaces: it closes at the line's end.
    closing: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Open {
    None,
    Inline(usize),
    Fence(char, usize),
}

impl Default for Code {
    fn default() -> Code {
        Code { open: Open::None, at_start: true, indent: 0, closing: false }
    }
}

impl Code {
    fn is_open(&self) -> bool {
        self.open != Open::None
    }

    /// Take in shown text, whose backtick and tilde runs are whole: a run at the end of a piece is
    /// held back.
    fn feed(&mut self, text: &str) {
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\n' => {
                    if self.closing {
                        self.open = Open::None;
                    }
                    *self = Code { open: self.open, ..Code::default() };
                }
                ' ' | '\t' => {
                    if self.at_start {
                        self.indent = self.indent.saturating_add(1);
                    }
                }
                '`' | '~' => {
                    let mut n = 1usize;
                    while chars.peek() == Some(&c) {
                        chars.next();
                        n = n.saturating_add(1);
                    }
                    self.run(c, n);
                }
                _ => {
                    self.at_start = false;
                    self.closing = false;
                }
            }
        }
    }

    fn run(&mut self, c: char, n: usize) {
        let fence_line = self.at_start && self.indent < 4;
        self.closing = false;
        self.open = match self.open {
            Open::None if fence_line && n >= 3 => Open::Fence(c, n),
            Open::None if c == '`' => Open::Inline(n),
            Open::Inline(k) if c == '`' && n == k => Open::None,
            Open::Fence(f, m) if fence_line && f == c && n >= m => {
                self.closing = true;
                Open::Fence(f, m)
            }
            open => open,
        };
        self.at_start = false;
    }
}

impl Markers {
    /// Take the next piece of an answer.
    pub(crate) fn feed(&mut self, text: &str) -> Vec<Piece> {
        let mut out = Vec::new();
        self.held.push_str(text);
        if let Some(before) = self.dropping.take() {
            let all = std::mem::take(&mut self.held);
            let mut scan = before;
            match scan.find_close(&all) {
                Some(end) => self.held = all.get(end.saturating_add(CASE_CLOSE.len())..).unwrap_or_default().to_string(),
                None => {
                    // Only a tag's start could straddle this piece and the next: the scan goes on
                    // from the state before it, over what the next piece can't change.
                    let tail = tag_tail(&all);
                    let mut scan = before;
                    scan.find_close(all.get(..all.len().saturating_sub(tail.len())).unwrap_or_default());
                    self.dropping = Some(scan);
                    self.held = tail.to_string();
                    return out;
                }
            }
        }
        let all = std::mem::take(&mut self.held);
        let mut shown = String::new();
        let mut rest = all.as_str();
        while let Some(at) = rest.find('[') {
            let (before, from) = rest.split_at(at);
            shown.push_str(before);
            self.code.feed(before);
            if self.code.is_open() {
                shown.push('[');
                rest = from.get(1..).unwrap_or_default();
                continue;
            }
            if from.starts_with(CASE) {
                let body = from.get(CASE.len()..).unwrap_or_default();
                match Scan::default().find_close(body) {
                    Some(end) if end <= MAX_CASE => {
                        if !shown.is_empty() {
                            out.push(Piece::Text(std::mem::take(&mut shown)));
                        }
                        out.push(Piece::Case(body.get(..end).unwrap_or_default().trim().to_string()));
                        rest = body.get(end.saturating_add(CASE_CLOSE.len())..).unwrap_or_default();
                    }
                    Some(end) => {
                        if !shown.is_empty() {
                            out.push(Piece::Text(std::mem::take(&mut shown)));
                        }
                        out.push(Piece::CaseDropped);
                        rest = body.get(end.saturating_add(CASE_CLOSE.len())..).unwrap_or_default();
                    }
                    None => {
                        let tail = tag_tail(body);
                        if body.len().saturating_sub(tail.len()) <= MAX_CASE {
                            self.held = from.to_string();
                        } else {
                            if !shown.is_empty() {
                                out.push(Piece::Text(std::mem::take(&mut shown)));
                            }
                            out.push(Piece::CaseDropped);
                            self.dropping = Some(resume(body, tail.len()));
                            self.held = tail.to_string();
                        }
                        rest = "";
                    }
                }
                continue;
            }
            let opener = [REMEMBER, USED].into_iter().find(|o| from.starts_with(o));
            if let Some(opener) = opener {
                match from.find(CLOSE) {
                    Some(end) if end <= MAX_HELD => {
                        let inner = from.get(opener.len()..end).unwrap_or_default().trim().to_string();
                        if !shown.is_empty() {
                            out.push(Piece::Text(std::mem::take(&mut shown)));
                        }
                        out.push(if opener == REMEMBER { Piece::Remember(inner) } else { Piece::Used(inner) });
                        rest = from.get(end.saturating_add(CLOSE.len())..).unwrap_or_default();
                        continue;
                    }
                    None if from.len() <= MAX_HELD => {
                        self.held = from.to_string();
                        rest = "";
                        break;
                    }
                    _ => {}
                }
            } else if REMEMBER.starts_with(from) || USED.starts_with(from) || CASE.starts_with(from) {
                self.held = from.to_string();
                rest = "";
                break;
            }
            shown.push('[');
            rest = from.get(1..).unwrap_or_default();
        }
        if self.held.is_empty() {
            // A run of backticks or tildes may go on in the next piece: it is read whole or not yet.
            let kept = rest.len().saturating_sub(rest.trim_end_matches(['`', '~']).len()).min(MAX_HELD);
            let (now, later) = rest.split_at(rest.len().saturating_sub(kept));
            self.held = later.to_string();
            rest = now;
        }
        shown.push_str(rest);
        self.code.feed(rest);
        if !shown.is_empty() {
            out.push(Piece::Text(shown));
        }
        out
    }

    /// The answer stopped or something else came: what was held back was only text, except an
    /// unclosed case marker, which is dropped and counted rather than shown as half a note.
    pub(crate) fn flush(&mut self) -> Vec<Piece> {
        let held = std::mem::take(&mut self.held);
        self.code = Code::default();
        if self.dropping.take().is_some() || held.starts_with(CASE) {
            return vec![Piece::CaseDropped];
        }
        if held.is_empty() { Vec::new() } else { vec![Piece::Text(held)] }
    }
}

/// The scan of `body` short of its last `tail` bytes, which the next piece may complete into a tag.
fn resume(body: &str, tail: usize) -> Scan {
    let mut scan = Scan::default();
    scan.find_close(body.get(..body.len().saturating_sub(tail)).unwrap_or_default());
    scan
}

/// The longest tail of `text` that is a proper prefix of a case tag, on a char boundary.
fn tag_tail(text: &str) -> &str {
    let open = tail_prefix_of(text, CASE);
    let close = tail_prefix_of(text, CASE_CLOSE);
    if open.len() >= close.len() { open } else { close }
}

/// The longest tail of `text` that is a proper prefix of `pattern`, on a char boundary.
fn tail_prefix_of<'a>(text: &'a str, pattern: &str) -> &'a str {
    (1..pattern.len())
        .rev()
        .filter_map(|n| text.len().checked_sub(n).and_then(|at| text.get(at..)))
        .find(|tail| pattern.starts_with(tail))
        .unwrap_or_default()
}

//! An answer as the pane shows it: what the model wrote, read as Markdown with the parser the
//! file formats use, down to plain blocks and spans the window draws. Models write Markdown, and
//! nothing but the pane reads it this way: the transcript keeps the words as they came.
//!
//! The text is the model's, so it is hostile: the work is held to [`MAX_PARSED`] bytes and
//! [`MAX_BLOCKS`] blocks, and what is past them is shown as plain text ([`Formatted::rest`]).

use std::borrow::Cow;

use wordcraft_formats::markdown;
use wordcraft_formats::model::{FBlock, Fmt, Inline, Kind, Para};

use crate::backend::{cut, label, shown};

/// Most bytes of an answer read as Markdown, so an answer as long as a transcript keeps
/// ([`crate::transcript::MAX_ENTRY_BYTES`]) still takes one frame to read.
pub const MAX_PARSED: usize = 64 << 10;

/// Most blocks and spans of an answer drawn as widgets, so a list of 60 000 one-letter items
/// doesn't take a frame's time to draw.
pub const MAX_BLOCKS: usize = 512;
pub const MAX_SPANS: usize = 4096;

/// A run of text in one format.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    /// The address it links to, as the model wrote it: the window shows it and opens it only
    /// when [`opens`] says so, on the person's click.
    pub link: Option<String>,
}

/// One block of an answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Para(Vec<Span>),
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    /// A list item: its bullet or number, and how deep it is (from 0).
    Item {
        marker: String,
        level: u8,
        spans: Vec<Span>,
    },
    Quote(Vec<Span>),
    Code(String),
    Rule,
}

/// An answer, read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Formatted {
    pub blocks: Vec<Block>,
    /// What is past the limits, as plain text.
    pub rest: String,
}

/// Whether the window may open `address` on the person's click: a web address only, so a model
/// can't have a click start a program or run a script.
pub fn opens(address: &str) -> bool {
    let lower = address.trim_start().get(..8).map(str::to_ascii_lowercase).unwrap_or_default();
    lower.starts_with("https://") || lower.starts_with("http://")
}

/// Read `text` (an answer's) as Markdown. While it is `streaming`, what its last paragraph hasn't
/// finished is closed or held back ([`settled`]), so no `**` shows and then goes away.
pub fn format(text: &str, streaming: bool) -> Formatted {
    let (read, past) = split_at_line(text, MAX_PARSED);
    let read = if streaming { settled(read) } else { Cow::Borrowed(read) };
    let flow = markdown::parse(&read);
    let mut out = Builder::default();
    for block in &flow.blocks {
        out.block(block);
    }
    let mut formatted = out.done();
    formatted.rest.push_str(past);
    formatted
}

/// `text` up to about `max` bytes, ending with a line where it can, and what follows.
fn split_at_line(text: &str, max: usize) -> (&str, &str) {
    if text.len() <= max {
        return (text, "");
    }
    let end = cut(text.get(..max.saturating_add(4).min(text.len())).unwrap_or_default().to_string(), max).len();
    let end = text.get(..end).and_then(|t| t.rfind('\n')).map_or(end, |i| i.saturating_add(1));
    (text.get(..end).unwrap_or_default(), text.get(end..).unwrap_or_default())
}

#[derive(Default)]
struct Builder {
    formatted: Formatted,
    spans: usize,
    /// The next number of the ordered list open at each depth.
    numbers: Vec<u32>,
}

impl Builder {
    fn done(self) -> Formatted {
        self.formatted
    }

    fn full(&self) -> bool {
        self.formatted.blocks.len() >= MAX_BLOCKS || self.spans >= MAX_SPANS
    }

    fn block(&mut self, block: &FBlock) {
        match block {
            FBlock::Para(p) => self.para(p),
            // A pane is too narrow for a grid: each row reads as a line, its cells apart.
            // ponytail: a real grid when answers with tables turn out to be common.
            FBlock::Table(t) => {
                for row in &t.rows {
                    let mut line = Vec::new();
                    for cell in row.iter().filter(|c| !c.covered) {
                        if !line.is_empty() {
                            line.push(Span { text: " · ".into(), ..Span::default() });
                        }
                        for b in &cell.blocks {
                            if let FBlock::Para(p) = b {
                                line.extend(spans(p, cell.header));
                            }
                        }
                    }
                    self.push(Block::Para(line));
                }
            }
        }
    }

    fn para(&mut self, p: &Para) {
        let block = match (&p.kind, &p.list) {
            (Kind::Code, _) => {
                let line = p.inlines.iter().map(inline_text).collect::<String>();
                if let Some(Block::Code(code)) = self.formatted.blocks.last_mut() {
                    code.push('\n');
                    code.push_str(&line);
                    return;
                }
                Block::Code(line)
            }
            (Kind::Rule, _) => Block::Rule,
            (Kind::Heading(level), _) => Block::Heading { level: *level, spans: spans(p, false) },
            (Kind::Title, _) => Block::Heading { level: 1, spans: spans(p, false) },
            (_, Some(list)) => {
                let level = usize::from(list.level);
                self.numbers.truncate(level.saturating_add(1));
                let marker = if list.ordered {
                    self.numbers.resize(level.saturating_add(1), 0);
                    let next = self.numbers.get_mut(level).map_or(list.start, |n| {
                        *n = if *n == 0 { list.start } else { n.saturating_add(1) };
                        *n
                    });
                    format!("{next}.")
                } else {
                    "•".to_string()
                };
                self.push(Block::Item { marker, level: list.level, spans: spans(p, false) });
                return;
            }
            (Kind::Quote, None) => Block::Quote(spans(p, false)),
            (Kind::Normal, None) => Block::Para(spans(p, false)),
        };
        self.numbers.clear();
        self.push(block);
    }

    fn push(&mut self, block: Block) {
        if self.full() {
            let rest = &mut self.formatted.rest;
            rest.push_str(&block_text(&block));
            rest.push('\n');
            return;
        }
        self.spans = self.spans.saturating_add(match &block {
            Block::Para(s) | Block::Quote(s) | Block::Heading { spans: s, .. } | Block::Item { spans: s, .. } => s.len(),
            Block::Code(_) | Block::Rule => 1,
        });
        self.formatted.blocks.push(block);
    }
}

/// A block as plain text.
fn block_text(block: &Block) -> String {
    let words = |s: &[Span]| s.iter().map(|s| s.text.as_str()).collect::<String>();
    match block {
        Block::Para(s) | Block::Quote(s) | Block::Heading { spans: s, .. } => words(s),
        Block::Item { marker, spans, .. } => format!("{marker} {}", words(spans)),
        Block::Code(text) => text.clone(),
        Block::Rule => String::new(),
    }
}

/// A paragraph's text as spans; `bold`: all of it is (a table's header).
fn spans(p: &Para, bold: bool) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for inline in &p.inlines {
        let (text, fmt) = match inline {
            Inline::Text(text, fmt) => (Cow::Borrowed(text.as_str()), Some(fmt)),
            other => (Cow::Owned(inline_text(other)), None),
        };
        // The parser decodes `&#x202E;` and the like, which the event's own filter never saw.
        let text = shown(text.into_owned());
        if text.is_empty() {
            continue;
        }
        let span = span_of(text, fmt, bold);
        match out.last_mut() {
            Some(last) if Span { text: String::new(), ..last.clone() } == Span { text: String::new(), ..span.clone() } => {
                last.text.push_str(&span.text)
            }
            _ => out.push(span),
        }
    }
    out
}

fn span_of(text: String, fmt: Option<&Fmt>, bold: bool) -> Span {
    let Some(f) = fmt else { return Span { text, bold, ..Span::default() } };
    Span { text, bold: bold || f.bold, italic: f.italic, strike: f.strike, code: f.code, link: f.link.as_deref().map(label) }
}

/// The words of an inline: a picture's description, never the picture.
fn inline_text(inline: &Inline) -> String {
    match inline {
        Inline::Text(text, _) => text.clone(),
        Inline::Image(img) => img.alt.clone(),
        Inline::Figure(alt) => alt.clone(),
        Inline::Equation { linear, .. } => linear.clone(),
        Inline::Ruby { base, .. } => base.clone(),
        _ => String::new(),
    }
}

/// `text`, a piece of an answer still arriving, as it can be read now: the markers its last
/// paragraph left open are closed, a marker that may not be whole yet is left out, and a link
/// not finished yet is held back until it is. The same words read the same way before and after
/// the marker that closes them comes, so the person sees no `**` come and go.
fn settled(text: &str) -> Cow<'_, str> {
    if in_fence(text) {
        return Cow::Borrowed(text);
    }
    let start = text.rfind("\n\n").map_or(0, |i| i.saturating_add(2));
    let last = text.get(start..).unwrap_or_default();
    let mut read = scan(last);
    let mut kept = last;
    if let Some(from) = read.hold_from {
        kept = last.get(..from).unwrap_or_default();
        read = scan(kept);
    }
    let kept = kept.get(..read.whole).unwrap_or(kept);
    let kept = if read.open.is_empty() { kept } else { kept.trim_end() };
    if read.open.is_empty() && kept.len() == last.len() {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(start.saturating_add(kept.len()).saturating_add(16));
    out.push_str(text.get(..start).unwrap_or_default());
    out.push_str(kept);
    for marker in read.open.iter().rev() {
        out.push_str(marker);
    }
    Cow::Owned(out)
}

/// Whether `text` ends inside a code fence.
fn in_fence(text: &str) -> bool {
    let mut open: Option<&str> = None;
    for line in text.lines() {
        let t = line.trim_start();
        for fence in ["```", "~~~"] {
            if t.starts_with(fence) {
                match open {
                    None => open = Some(fence),
                    Some(f) if f == fence => open = None,
                    Some(_) => {}
                }
            }
        }
    }
    open.is_some()
}

/// What a paragraph still arriving left unfinished.
struct Scan {
    /// The markers left open, oldest first.
    open: Vec<&'static str>,
    /// Where a link not finished yet starts.
    hold_from: Option<usize>,
    /// The bytes before a marker at the very end that doesn't finish anything: it may not be
    /// whole yet.
    whole: usize,
}

/// What `text`, one paragraph, left unfinished.
fn scan(text: &str) -> Scan {
    let bytes = text.as_bytes();
    let run_at = |i: usize| bytes.get(i..).map_or(1, |r| r.iter().take_while(|b| **b == bytes[i]).count());
    let mut open: Vec<&'static str> = Vec::new();
    let mut brackets: Vec<usize> = Vec::new();
    let mut hold_from = None;
    let mut ticks: Option<usize> = None;
    let mut whole = text.len();
    let mut i = 0;
    while let Some(&c) = bytes.get(i) {
        match c {
            b'\\' if ticks.is_none() => {
                i = i.saturating_add(2);
                continue;
            }
            b'`' => {
                let run = run_at(i);
                let ends = i.saturating_add(run) == bytes.len();
                match ticks {
                    Some(n) if n == run => ticks = None,
                    _ if ends => whole = i,
                    Some(_) => {}
                    None => ticks = Some(run),
                }
                i = i.saturating_add(run);
                continue;
            }
            _ if ticks.is_some() => {}
            b'*' | b'_' | b'~' => {
                let run = run_at(i);
                let before = i.checked_sub(1).and_then(|p| bytes.get(p)).copied();
                let after = bytes.get(i.saturating_add(run)).copied();
                let word = |b: Option<u8>| b.is_some_and(|b| b.is_ascii_alphanumeric());
                let space = |b: Option<u8>| b.is_none_or(|b| b.is_ascii_whitespace());
                let opens = !space(after) && (c != b'_' || !word(before));
                let closes = !space(before) && (c != b'_' || !word(after));
                let held = open.len();
                match (c, run) {
                    (b'~', 2) => toggle(&mut open, "~~", opens, closes),
                    (b'*', 1) => toggle(&mut open, "*", opens, closes),
                    (b'*', 2) => toggle(&mut open, "**", opens, closes),
                    (b'_', 1) => toggle(&mut open, "_", opens, closes),
                    (b'_', 2) => toggle(&mut open, "__", opens, closes),
                    (b'*' | b'_', 3) => {
                        let (two, one) = if c == b'*' { ("**", "*") } else { ("__", "_") };
                        toggle(&mut open, one, opens, closes);
                        toggle(&mut open, two, opens, closes);
                    }
                    _ => {}
                }
                // At the very end it either closes something or may be the start of a longer
                // marker: `**` arrives one `*` at a time.
                if after.is_none() && open.len() == held {
                    whole = i;
                }
                i = i.saturating_add(run);
                continue;
            }
            b'[' => brackets.push(i),
            b']' => {
                // A link's address, not ended or not begun yet: all of the link waits for it.
                let closes_link = |at: usize| bytes.get(at..).and_then(|r| r.iter().position(|b| *b == b')'));
                match (brackets.pop(), bytes.get(i.saturating_add(1))) {
                    (Some(from), None) => {
                        hold_from = Some(from);
                        break;
                    }
                    (Some(from), Some(b'(')) => match closes_link(i) {
                        Some(to) => {
                            i = i.saturating_add(to).saturating_add(1);
                            continue;
                        }
                        None => {
                            hold_from = Some(from);
                            break;
                        }
                    },
                    _ => {}
                }
            }
            _ => {}
        }
        i = i.saturating_add(1);
    }
    // A `[` not closed may be the start of a link, on the line the answer has got to.
    let last_line = text.rfind('\n').map_or(0, |n| n.saturating_add(1));
    let hold_from = hold_from.or_else(|| brackets.into_iter().find(|b| *b >= last_line));
    // A picture's `!` goes with its `[`.
    let hold_from = hold_from.map(|from| if from > 0 && bytes.get(from - 1) == Some(&b'!') { from - 1 } else { from });
    if let Some(n) = ticks {
        open.push(match n {
            1 => "`",
            2 => "``",
            _ => "```",
        });
    }
    Scan { open, hold_from, whole }
}

/// A run of `marker`: it closes what is open last, else it opens.
fn toggle(open: &mut Vec<&'static str>, marker: &'static str, opens: bool, closes: bool) {
    if closes && open.last() == Some(&marker) {
        open.pop();
    } else if opens {
        open.push(marker);
    }
}

#[cfg(test)]
mod tests;

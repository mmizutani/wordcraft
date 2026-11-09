//! An answer as the pane shows it: what the model wrote, read as Markdown with the parser the
//! file formats use, down to plain blocks and spans the window draws. Models write Markdown, and
//! nothing but the pane reads it this way: the transcript keeps the words as they came.
//!
//! The text is the model's, so it is hostile: the work is held to [`MAX_PARSED`] bytes and
//! [`MAX_BLOCKS`] blocks, and what is past them is shown as plain text ([`Formatted::rest`]).

use std::borrow::Cow;

use wordcraft_formats::markdown;
use wordcraft_formats::model::{FBlock, Fmt, Inline, Kind, ListInfo, Para};

use crate::backend::{MAX_LABEL, label, shown};

/// Most bytes of an answer read as Markdown, so an answer as long as a transcript keeps
/// ([`crate::transcript::MAX_ENTRY_BYTES`]) still takes one frame to read.
pub const MAX_PARSED: usize = 64 << 10;

/// Most blocks and spans of an answer drawn as widgets, so a list of 60 000 one-letter items
/// doesn't take a frame's time to draw.
pub const MAX_BLOCKS: usize = 512;
pub const MAX_SPANS: usize = 4096;

/// Most characters of the words a citation holds; a longer marker stays as text.
pub const MAX_CITE_CHARS: usize = 200;

/// Most citations of an answer that show; markers past them go without a chip.
pub const MAX_CITES: usize = 64;

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
    /// It stands for this citation of [`Formatted::citations`], and has no text of its own.
    pub cite: Option<usize>,
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
    /// The words of its citations, in order. Nothing says they are real: the window looks.
    pub citations: Vec<String>,
}

/// Whether the window may open `address` on the person's click: a web address only, so a model
/// can't have a click start a program or run a script.
pub fn opens(address: &str) -> bool {
    let has = |scheme: &str| address.get(..scheme.len()).is_some_and(|start| start.eq_ignore_ascii_case(scheme));
    has("https://") || has("http://")
}

/// Read `text` (an answer's) as Markdown. While it is `streaming`, what its last paragraph hasn't
/// finished is closed or held back ([`settled`]), so no `**` shows and then goes away.
pub fn format(text: &str, streaming: bool) -> Formatted {
    let text: Cow<'_, str> = if text.contains(OURS) { Cow::Owned(text.replace(OURS, "")) } else { Cow::Borrowed(text) };
    let (text, words) = with_cites(&text, streaming, usize::MAX);
    let (read, past) = split_at_line(&text, MAX_PARSED);
    let read = if streaming { settled(read) } else { Cow::Borrowed(read) };
    let (flow, cut) = parsed(read);
    // The parser reads documents, which hold tables to limits. An answer's words all stay.
    if cut {
        return Formatted { blocks: Vec::new(), rest: strip_cites(&text), citations: Vec::new() };
    }
    let mut out = Builder::default();
    for block in &flow.blocks {
        out.block(block);
    }
    let cited = std::mem::take(&mut out.cited);
    let mut formatted = out.done();
    formatted.rest.push_str(&strip_cites(past));
    formatted.citations = cited.iter().filter_map(|i| words.get(*i).cloned()).collect();
    formatted
}

/// The words of the citations in `text`, whole markers only, at most [`MAX_CITES`]: those that show,
/// so not a fenced code block's. Details asks for every answer each frame, so this reads fences
/// without reading the Markdown.
// ponytail: a marker in code by indentation alone is listed though it shows no chip; read that too
// if models cite inside such code.
pub fn citations(text: &str) -> Vec<String> {
    let mut fences = Fences::default();
    let mut prose = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let was_open = fences.open.is_some();
        fences.line(line);
        if !was_open && fences.open.is_none() {
            prose.push_str(line);
        }
    }
    with_cites(&prose, false, MAX_CITES).1
}

/// `text` with each citation's marker, `[[cite: "the exact words"]]`, as an index between two
/// [`CITE`]s, and the words. A marker not closed yet is held back while the answer is `streaming`,
/// and stays as text once it isn't: it was no marker. So does one too long to be a citation. The
/// next close is looked for only once the reading has passed the last one found, so the whole text
/// costs one pass however many false markers it holds: Details reads every answer each frame. It
/// keeps `most` citations, or marks as many as there are for [`Builder`] to choose those that show.
fn with_cites(text: &str, streaming: bool, most: usize) -> (Cow<'_, str>, Vec<String>) {
    const OPEN: &str = "[[cite:";
    const CLOSE: &str = "\"]]";
    if !text.contains(OPEN) {
        return (Cow::Borrowed(text), Vec::new());
    }
    let longest = MAX_CITE_CHARS.saturating_mul(4);
    let mut out = String::with_capacity(text.len());
    let mut words = Vec::new();
    let mut rest = text;
    let mut close = text.find(CLOSE);
    while let Some(at) = rest.find(OPEN) {
        let after = rest.get(at.saturating_add(OPEN.len())..).unwrap_or_default();
        out.push_str(rest.get(..at).unwrap_or_default());
        let Some(q) = after.trim_start_matches(' ').strip_prefix('"') else {
            out.push_str(OPEN);
            rest = after;
            continue;
        };
        let from = text.len().saturating_sub(q.len());
        if close.is_some_and(|c| c < from) {
            close = text.get(from..).and_then(|t| t.find(CLOSE)).map(|c| c.saturating_add(from));
        }
        let cited = close.and_then(|c| q.get(..c.saturating_sub(from))).filter(|c| c.len() <= longest && c.chars().count() <= MAX_CITE_CHARS);
        match cited {
            Some(cited) => {
                if !cited.trim().is_empty() && words.len() < most {
                    out.push(CITE);
                    out.push_str(&words.len().to_string());
                    out.push(CITE);
                    words.push(shown(cited.to_string()));
                }
                rest = q.get(cited.len().saturating_add(CLOSE.len())..).unwrap_or_default();
            }
            None if streaming && close.is_none() && q.len() <= longest => return (Cow::Owned(out), words),
            None => {
                out.push_str(OPEN);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    (Cow::Owned(out), words)
}

/// `text` as the parser reads it for an answer, and whether a table lost words.
fn parsed(text: Cow<'_, str>) -> (wordcraft_formats::model::Flow, bool) {
    let text = separate_fences(text);
    markdown::parse_reporting_cuts(&without_embedded(&text))
}

/// Stands for the `:` of a `data:` address while the parser reads an answer.
const NOT_DATA: &str = "\u{F8FE}";

/// Stands either side of a citation's index while the parser reads an answer.
const CITE: char = '\u{F8FD}';

/// The characters this reads an answer with, which an answer's own text gives up.
const OURS: &[char] = &['\u{F8FD}', '\u{F8FE}', '\u{F8FF}'];

/// `text` with no `data:` address the parser would decode, which [`shown_again`] undoes. An answer
/// draws a picture's description only, and a reference repeated thousands of times to one long
/// address would decode it each time: hundreds of megabytes from a few kilobytes of answer.
fn without_embedded(text: &str) -> Cow<'_, str> {
    let lower = text.to_ascii_lowercase();
    if !lower.contains("data:") {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut from = 0usize;
    for (at, _) in lower.match_indices("data:") {
        let colon = at.saturating_add(4);
        out.push_str(text.get(from..colon).unwrap_or_default());
        out.push_str(NOT_DATA);
        from = colon;
    }
    out.push_str(text.get(from..).unwrap_or_default());
    Cow::Owned(out)
}

/// What [`without_embedded`] did, undone.
fn shown_again(text: &str) -> String {
    text.replace(NOT_DATA, "")
}

/// A paragraph of this alone says two code blocks are two: the parser gives each line of code as a
/// paragraph of its own, and nothing between one fence's lines and the next one's.
const BETWEEN_FENCES: &str = "\u{F8FF}";

/// `text` with [`BETWEEN_FENCES`] between a closing fence and the next opening one.
fn separate_fences(text: Cow<'_, str>) -> Cow<'_, str> {
    let mut fences = Fences::default();
    let mut closed = false;
    let mut at = 0usize;
    let mut places = Vec::new();
    for line in text.split_inclusive('\n') {
        let was_open = fences.open.is_some();
        fences.line(line);
        match (was_open, fences.open.is_some()) {
            (true, false) => closed = true,
            (false, true) if closed => {
                places.push(at);
                closed = false;
            }
            (false, false) if !line.trim().is_empty() => closed = false,
            _ => {}
        }
        at = at.saturating_add(line.len());
    }
    if places.is_empty() {
        return text;
    }
    let mut out = String::with_capacity(text.len().saturating_add(places.len().saturating_mul(BETWEEN_FENCES.len() + 2)));
    let mut from = 0usize;
    for place in places {
        out.push_str(text.get(from..place).unwrap_or_default());
        out.push_str(BETWEEN_FENCES);
        out.push_str("\n\n");
        from = place;
    }
    out.push_str(text.get(from..).unwrap_or_default());
    Cow::Owned(out)
}

/// `text` up to about `max` bytes, ending with a line where it can, and what follows.
fn split_at_line(text: &str, max: usize) -> (&str, &str) {
    if text.len() <= max {
        return (text, "");
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    let end = text.get(..end).and_then(|t| t.rfind('\n')).map_or(end, |i| i.saturating_add(1));
    (text.get(..end).unwrap_or_default(), text.get(end..).unwrap_or_default())
}

#[derive(Default)]
struct Builder {
    formatted: Formatted,
    spans: usize,
    /// The last number of the ordered list open at each depth.
    numbers: Vec<Option<u32>>,
    /// The code block drawn last ended: the next line of code starts another.
    code_ended: bool,
    /// The citation the next chip must be.
    next_cite: usize,
    /// The citations with a chip, at most [`MAX_CITES`]: a marker in code has none. A chip's number
    /// is its place here.
    cited: Vec<usize>,
}

impl Builder {
    fn done(self) -> Formatted {
        self.formatted
    }

    /// A paragraph's text as spans, with its citations as spans of their own. Chips come in the
    /// order of their markers, each once, so what a character entity in an answer makes of our
    /// placeholder can't add one past the limit.
    fn spans_of(&mut self, p: &Para, bold: bool) -> Vec<Span> {
        let spans = spans(p, bold);
        if !spans.iter().any(|s| s.text.contains(CITE)) {
            return spans;
        }
        let mut out = Vec::new();
        for span in spans {
            for (n, piece) in span.text.split(CITE).enumerate() {
                if n % 2 == 1 {
                    // Past ones code ate, whose markers show nowhere.
                    if let Some(cite) = piece.parse().ok().filter(|cite| *cite >= self.next_cite && self.cited.len() < MAX_CITES) {
                        out.push(Span { cite: Some(self.cited.len()), ..Span::default() });
                        self.cited.push(cite);
                        self.next_cite = cite.saturating_add(1);
                    }
                } else if !piece.is_empty() {
                    out.push(Span { text: piece.to_string(), ..span.clone() });
                }
            }
        }
        out
    }

    fn block(&mut self, block: &FBlock) {
        match block {
            FBlock::Para(p) => self.para(p),
            // A pane is too narrow for a grid: each row reads as a line, its cells apart.
            // ponytail: a real grid when answers with tables turn out to be common.
            FBlock::Table(t) => {
                self.numbers.clear();
                for row in &t.rows {
                    let mut line = Vec::new();
                    for cell in row.iter().filter(|c| !c.covered) {
                        if !line.is_empty() {
                            line.push(Span { text: " · ".into(), ..Span::default() });
                        }
                        for b in &cell.blocks {
                            if let FBlock::Para(p) = b {
                                line.extend(self.spans_of(p, cell.header));
                            }
                        }
                    }
                    self.push(Block::Para(line));
                }
            }
        }
    }

    fn para(&mut self, p: &Para) {
        if matches!(p.inlines.as_slice(), [Inline::Text(text, _)] if text == BETWEEN_FENCES) {
            self.code_ended = true;
            return;
        }
        // What can't carry a marker comes after one of its own.
        if let (Kind::Code | Kind::Rule, Some(list)) = (&p.kind, &p.list) {
            let marker = self.marker(list);
            self.push(Block::Item { marker, level: list.level, spans: Vec::new() });
        }
        let block = match (&p.kind, &p.list) {
            (Kind::Code, _) => {
                let line = strip_cites(&shown_again(&p.inlines.iter().map(inline_text).collect::<String>()));
                // Once something has gone to the plain text, what follows goes after it.
                if self.formatted.rest.is_empty()
                    && !self.code_ended
                    && let Some(Block::Code(code)) = self.formatted.blocks.last_mut()
                {
                    code.push('\n');
                    code.push_str(&line);
                    return;
                }
                self.code_ended = false;
                Block::Code(line)
            }
            (Kind::Rule, _) => Block::Rule,
            (Kind::Heading(level), None) => Block::Heading { level: *level, spans: self.spans_of(p, false) },
            (Kind::Title, None) => Block::Heading { level: 1, spans: self.spans_of(p, false) },
            (_, Some(list)) => {
                let marker = self.marker(list);
                // A heading in a list keeps its place there, and stands out by its weight.
                let heading = matches!(p.kind, Kind::Heading(_) | Kind::Title);
                let spans = self.spans_of(p, heading);
                self.push(Block::Item { marker, level: list.level, spans });
                return;
            }
            (Kind::Quote, None) => Block::Quote(self.spans_of(p, false)),
            (Kind::Normal, None) => Block::Para(self.spans_of(p, false)),
        };
        if p.list.is_none() {
            self.numbers.clear();
        }
        self.push(block);
    }

    /// The marker of the list item `list` says this is, counting it.
    fn marker(&mut self, list: &ListInfo) -> String {
        let level = usize::from(list.level);
        self.numbers.truncate(level.saturating_add(1));
        if list.ordered {
            self.numbers.resize(level.saturating_add(1), None);
            let next = self.numbers.get(level).copied().flatten().map_or(list.start, |n| n.saturating_add(1));
            if let Some(n) = self.numbers.get_mut(level) {
                *n = Some(next);
            }
            format!("{next}.")
        } else {
            // A bullet ends an ordered list at this depth: the next one starts again.
            if let Some(n) = self.numbers.get_mut(level) {
                *n = None;
            }
            "•".to_string()
        }
    }

    fn push(&mut self, block: Block) {
        let spans = self.spans.saturating_add(match &block {
            Block::Para(s) | Block::Quote(s) | Block::Heading { spans: s, .. } | Block::Item { spans: s, .. } => s.len(),
            Block::Code(_) | Block::Rule => 1,
        });
        // Past a limit everything goes to the plain text, so nothing comes before what it followed.
        if self.formatted.blocks.len() >= MAX_BLOCKS || spans > MAX_SPANS || !self.formatted.rest.is_empty() {
            let rest = &mut self.formatted.rest;
            rest.push_str(&block_text(&block));
            rest.push('\n');
            return;
        }
        self.spans = spans;
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
        let text = shown(shown_again(&text));
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

/// `text` without the citations [`with_cites`] left in it: code shows no chip.
fn strip_cites(text: &str) -> String {
    if !text.contains(CITE) {
        return text.to_string();
    }
    text.split(CITE).step_by(2).collect()
}

fn span_of(text: String, fmt: Option<&Fmt>, bold: bool) -> Span {
    let Some(f) = fmt else { return Span { text, bold, ..Span::default() } };
    // An address too long to show whole isn't a link: cut, it could go somewhere else.
    let link = f.link.as_deref().map(|l| strip_cites(&shown_again(l))).filter(|l| l.chars().count() <= MAX_LABEL).map(|l| label(&l));
    Span { text, bold: bold || f.bold, italic: f.italic, strike: f.strike, code: f.code, link, cite: None }
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
    let Some(after_fence) = after_fences(text) else {
        return Cow::Borrowed(text);
    };
    let start = text.rfind("\n\n").map_or(0, |i| i.saturating_add(2)).max(after_fence);
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

/// Which code fence the lines so far left open, as the parser reads them: one closes on a line of
/// nothing but at least as many of the same character.
#[derive(Default)]
struct Fences {
    open: Option<(char, usize)>,
}

impl Fences {
    fn line(&mut self, line: &str) {
        // Four spaces in is code, or a line of it, as the parser reads it.
        let indent = line.chars().take_while(|c| *c == ' ').count();
        if indent >= 4 || line.starts_with('\t') {
            return;
        }
        let t = line.trim();
        let Some(c) = t.chars().next().filter(|c| matches!(c, '`' | '~')) else { return };
        let n = t.chars().take_while(|x| *x == c).count();
        match self.open {
            None if n >= 3 && (c != '`' || !t.get(n..).is_some_and(|info| info.contains('`'))) => self.open = Some((c, n)),
            Some((f, at_least)) if f == c && n >= at_least && t.len() == n => self.open = None,
            _ => {}
        }
    }
}

/// Where `text`'s last closed code fence ends, 0 without one; `None` when it ends inside a fence.
fn after_fences(text: &str) -> Option<usize> {
    let mut fences = Fences::default();
    let mut after = 0usize;
    let mut at = 0usize;
    for line in text.split_inclusive('\n') {
        let was_open = fences.open.is_some();
        fences.line(line);
        at = at.saturating_add(line.len());
        if was_open && fences.open.is_none() {
            after = at;
        }
    }
    fences.open.is_none().then_some(after)
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
    let run_at = |i: usize, c: u8| bytes.get(i..).map_or(1, |r| r.iter().take_while(|b| **b == c).count());
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
                let run = run_at(i, c);
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
                let run = run_at(i, c);
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
                        // Closing, the one opened last goes first.
                        let order = if open.last() == Some(&two) { [two, one] } else { [one, two] };
                        toggle(&mut open, order[0], opens, closes);
                        toggle(&mut open, order[1], opens, closes);
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
                // Its address may hold parentheses of its own, in pairs.
                let closes_link = |at: usize| {
                    let mut depth = 0usize;
                    bytes.get(at..)?.iter().position(|b| {
                        match b {
                            b'(' => depth = depth.saturating_add(1),
                            b')' => depth = depth.saturating_sub(1),
                            _ => return false,
                        }
                        depth == 0
                    })
                };
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

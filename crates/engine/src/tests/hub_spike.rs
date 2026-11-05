//! Hub spike (A0.5, a throwaway branch): an in-process hub with the accept rule and three copies,
//! one person typing and two scripted agents, on a 188-page document, two of them in one
//! paragraph; and the same with a second person typing in that paragraph instead of the first
//! agent. It measures what the plan asks: how big changes are, what applying one costs, and how
//! often changes bounce.
//!
//! `cargo nextest run -p wordcraft-engine --run-ignored only -E 'test(/hub_spike/)' --no-capture`
//!
//! The model is the plan's "one edit, end to end": a participant's command runs in its own copy
//! and yields a ChangeSet, which the copy shows at once and sends with the version it had (its
//! base) and a sequence number. The hub applies a change if what it touched is unchanged since its
//! base (`Document::apply` finds the blocks by id and checks their fingerprints), logs it and
//! sends it to every copy; the sender's copy takes its own as the acknowledgement. A change that
//! raced another bounces: by then the copy has received what it missed, so it goes back to the
//! hub's version, replays its pending commands and sends them again. A copy that receives a
//! change its pending ones overlap does the same at once, without waiting for the bounce.
//!
//! Time runs in ticks of 50 ms; every message takes `latency` ticks each way.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wordcraft_doc::change::{Anchor, Mapped};
use wordcraft_doc::{ChangeSet, Document, Pos};

use crate::Session;

const TICK_MS: u64 = 50;
const PERSON: usize = 0;
const NEAR: usize = 1;
const FAR: usize = 2;
const NAMES: [&str; 3] = ["person", "agent 1", "agent 2"];

/// What a participant meant: replayed as it was meant after a bounce.
#[derive(Clone, Debug)]
enum Intent {
    /// The person types this at their caret.
    Type(String),
    /// An agent's commands, which find their text by searching, so they replay anywhere.
    Commands(Vec<(&'static str, Value)>),
}

enum Msg {
    Submit { from: usize, seq: u64, base: u64, cs: Arc<ChangeSet> },
    Entry(Entry),
    Bounced { seq: u64 },
}

/// A change the hub accepted, as it sends it to every copy.
struct Entry {
    version: u64,
    author: usize,
    seq: u64,
    cs: Arc<ChangeSet>,
}

struct Net {
    latency: u64,
    hub: VecDeque<(u64, Msg)>,
    copies: Vec<VecDeque<(u64, Msg)>>,
}

impl Net {
    fn send_to_hub(&mut self, now: u64, m: Msg) {
        self.hub.push_back((now + self.latency, m));
    }
    fn send_to_copy(&mut self, c: usize, now: u64, m: Msg) {
        self.copies[c].push_back((now + self.latency, m));
    }
    fn due(q: &mut VecDeque<(u64, Msg)>, now: u64) -> Option<Msg> {
        if q.front().is_some_and(|(t, _)| *t <= now) { q.pop_front().map(|(_, m)| m) } else { None }
    }
    fn idle(&self) -> bool {
        self.hub.is_empty() && self.copies.iter().all(VecDeque::is_empty)
    }
}

#[derive(Default)]
struct Hub {
    doc: Document,
    version: u64,
    log: Vec<(u64, usize, Arc<ChangeSet>)>,
    applied: Vec<Duration>,
    refused: Vec<Duration>,
    accepted: [usize; 3],
    bounced: [usize; 3],
    /// Bounces where the change and every change by others it missed edit the text of the same
    /// one paragraph in place, at ranges that don't overlap: those an OT-style transform of text
    /// splices at the hub would have accepted (an upper bound: formatting isn't checked).
    transformable: usize,
}

impl Hub {
    fn submit(&mut self, from: usize, seq: u64, base: u64, cs: Arc<ChangeSet>, net: &mut Net, now: u64) {
        let t = Instant::now();
        let r = self.doc.apply(&cs);
        let dt = t.elapsed();
        match r {
            Ok(()) => {
                self.applied.push(dt);
                self.version += 1;
                self.log.push((self.version, from, cs.clone()));
                self.accepted[from] += 1;
                for c in 0..net.copies.len() {
                    net.send_to_copy(c, now, Msg::Entry(Entry { version: self.version, author: from, seq, cs: cs.clone() }));
                }
            }
            Err(_) => {
                self.refused.push(dt);
                self.bounced[from] += 1;
                if self.transformable(from, base, &cs) {
                    self.transformable += 1;
                }
                net.send_to_copy(from, now, Msg::Bounced { seq });
            }
        }
    }

    fn transformable(&self, from: usize, base: u64, cs: &ChangeSet) -> bool {
        let Some((id, at, end)) = text_edit(cs) else { return false };
        let missed = self.log.iter().filter(|(v, a, _)| *v > base && *a != from);
        let mut touching = missed.filter(|(_, _, m)| m.splices.iter().any(|s| s.removed.iter().any(|b| b.id == id))).peekable();
        touching.peek().is_some()
            && touching
                .all(|(_, _, m)| text_edit(m).is_some_and(|(mid, mat, mend)| mid == id && (mend <= at || end <= mat) && !(mat == at && mend == end)))
    }
}

/// A change that edits one paragraph's text in place: its id and the byte range it replaced.
fn text_edit(cs: &ChangeSet) -> Option<(wordcraft_doc::BlockId, usize, usize)> {
    let [s] = cs.splices.as_slice() else { return None };
    let ([old], [new]) = (s.removed.as_slice(), s.inserted.as_slice()) else { return None };
    let text_changed = s.text.removed > 0 || !s.text.inserted.is_empty();
    (cs.tables.is_empty() && cs.parts.is_empty() && cs.fields.is_empty() && old.id == new.0.id() && text_changed).then_some((
        old.id,
        s.text.at,
        s.text.at + s.text.removed,
    ))
}

struct Pend {
    seq: u64,
    intent: Intent,
    /// When the participant did it: each keystroke of a replayed run of typing.
    at: Vec<u64>,
}

/// Several intents as one: the person's typing run, or an agent's commands in order.
fn combine(ps: Vec<Pend>) -> (Intent, Vec<u64>) {
    let at = ps.iter().flat_map(|p| p.at.iter().copied()).collect();
    let typed: Option<String> = ps.iter().map(|p| if let Intent::Type(t) = &p.intent { Some(t.as_str()) } else { None }).collect();
    let intent = match typed {
        Some(t) => Intent::Type(t),
        None => Intent::Commands(ps.into_iter().flat_map(|p| if let Intent::Commands(c) = p.intent { c } else { Vec::new() }).collect()),
    };
    (intent, at)
}

#[derive(Default)]
struct Stats {
    keystrokes: usize,
    sent: usize,
    replays: usize,
    acked: usize,
    bounced: usize,
    rebuilt: usize,
    /// Bounces of changes not yet replayed: the hub refused what applied here.
    surprise_bounces: usize,
    /// Own changes the hub accepted after this copy had replayed them (they'd be doubled).
    surprise_accepts: usize,
    sizes: Vec<usize>,
    /// Ticks from each keystroke or agent edit to its acknowledgement, replays included.
    wait: Vec<u64>,
    /// Applying another participant's change: to the hub's version and to the shown copy.
    remote: Vec<Duration>,
    /// … and laying the shown copy out again (the person's copy only).
    remote_layout: Vec<Duration>,
    /// Laying the shown copy out again after the person's own keystroke, for comparison.
    local_layout: Vec<Duration>,
}

struct Copy {
    s: Session,
    confirmed: Document,
    base: u64,
    pending: VecDeque<Pend>,
    /// With batching: what was done while a change was in flight, sent as one when it's
    /// acknowledged.
    held: Vec<Pend>,
    batch: bool,
    dead: HashSet<u64>,
    next_seq: u64,
    /// The person's caret: where their pending typing starts, in the hub's version.
    start: Option<Anchor>,
    layout: bool,
    stats: Stats,
}

impl Copy {
    fn new(doc: &Document, layout: bool, batch: bool) -> Copy {
        let mut s = Session::new(doc.clone());
        if layout {
            let _ = s.layout();
        }
        Copy {
            s,
            confirmed: doc.clone(),
            base: 0,
            pending: VecDeque::new(),
            held: Vec::new(),
            batch,
            dead: HashSet::new(),
            next_seq: 0,
            start: None,
            layout,
            stats: Stats::default(),
        }
    }

    fn typed(&self) -> usize {
        self.pending.iter().chain(&self.held).map(|p| if let Intent::Type(t) = &p.intent { t.len() } else { 0 }).sum()
    }

    /// Runs an intent on the shown copy: the change it made.
    fn run(&mut self, intent: &Intent) -> Option<ChangeSet> {
        let before = self.s.doc.clone();
        match intent {
            Intent::Type(text) => {
                let a = self.start?;
                let block = self.s.doc.body.iter().position(|b| b.id() == a.block)?;
                let pos = Pos::body(block, a.off + self.typed());
                self.s.run("caret.set", &json!({"pos": pos})).ok()?;
                self.s.run("text.insert", &json!({"text": text})).ok()?;
            }
            Intent::Commands(cmds) => {
                for (k, (id, p)) in cmds.iter().enumerate() {
                    if self.s.run(id, p).is_err() {
                        assert_eq!(k, 0, "{id} failed after the search found its text");
                        return None;
                    }
                }
            }
        }
        let cs = ChangeSet::between(&before, &self.s.doc);
        (!cs.is_empty()).then_some(cs)
    }

    fn act(&mut self, me: usize, intent: Intent, now: u64, net: &mut Net) {
        if matches!(intent, Intent::Type(_)) {
            self.stats.keystrokes += 1;
        }
        if let Some(cs) = self.run(&intent) {
            if self.layout {
                let t = Instant::now();
                let _ = self.s.layout();
                self.stats.local_layout.push(t.elapsed());
            }
            if self.batch && !self.pending.is_empty() {
                self.held.push(Pend { seq: u64::MAX, intent, at: vec![now] });
            } else {
                self.send(me, intent, cs, vec![now], now, net);
            }
        }
    }

    /// With batching, after an acknowledgement: everything held, as one change.
    fn flush(&mut self, me: usize, now: u64, net: &mut Net) {
        if self.held.is_empty() || !self.pending.is_empty() {
            return;
        }
        let held = std::mem::take(&mut self.held);
        let cs = ChangeSet::between(&self.confirmed, &self.s.doc);
        let (intent, at) = combine(held);
        if !cs.is_empty() {
            self.send(me, intent, cs, at, now, net);
        }
    }

    fn send(&mut self, me: usize, intent: Intent, cs: ChangeSet, at: Vec<u64>, now: u64, net: &mut Net) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.stats.sent += 1;
        self.stats.sizes.push(serde_json::to_vec(&cs).map(|v| v.len()).unwrap_or(0));
        net.send_to_hub(now, Msg::Submit { from: me, seq, base: self.base, cs: Arc::new(cs) });
        self.pending.push_back(Pend { seq, intent, at });
    }

    fn entry(&mut self, me: usize, e: &Entry, now: u64, net: &mut Net) {
        let (cs, seq) = (&*e.cs, e.seq);
        let t = Instant::now();
        self.confirmed.apply(cs).expect("the hub's changes apply to its version in its order");
        self.base = e.version;
        if e.author == me {
            if self.dead.remove(&seq) {
                self.stats.surprise_accepts += 1;
                return;
            }
            let p = self.pending.pop_front().expect("an acknowledgement for a pending change");
            assert_eq!(p.seq, seq, "acknowledgements come in order");
            self.stats.acked += 1;
            self.stats.wait.extend(p.at.iter().map(|t| now - t));
            if let (Intent::Type(text), Some(a)) = (&p.intent, self.start.as_mut()) {
                a.off += text.len();
            }
            self.flush(me, now, net);
            return;
        }
        if let Some(a) = self.start {
            self.start = match cs.map(a) {
                Mapped::At(a) => Some(a),
                _ => panic!("the person's paragraph is never removed"),
            };
        }
        let shown = self.s.doc.apply(cs).is_ok();
        let applied = t.elapsed();
        if !shown {
            self.rebuild(me, now, net);
            return;
        }
        self.s.touch();
        self.s.forget_changes();
        self.stats.remote.push(applied);
        if self.layout {
            let _ = self.s.layout();
            self.stats.remote_layout.push(t.elapsed());
        }
    }

    fn bounced(&mut self, me: usize, seq: u64, now: u64, net: &mut Net) {
        self.stats.bounced += 1;
        if !self.dead.remove(&seq) {
            self.stats.surprise_bounces += 1;
            self.rebuild(me, now, net);
        }
    }

    /// Back to the hub's version, then every pending intent again, sent anew: the person's typing
    /// as one change at their caret, mapped through what the others did.
    fn rebuild(&mut self, me: usize, now: u64, net: &mut Net) {
        self.stats.rebuilt += 1;
        let mut pending: Vec<Pend> = self.pending.drain(..).collect();
        self.dead.extend(pending.iter().map(|p| p.seq));
        pending.append(&mut self.held);
        if self.batch && !pending.is_empty() {
            self.s.doc = self.confirmed.clone();
            self.s.touch();
            self.s.forget_changes();
            let (intent, at) = combine(pending);
            if let Some(cs) = self.run(&intent) {
                self.stats.replays += 1;
                self.send(me, intent, cs, at, now, net);
            }
            if self.layout {
                let _ = self.s.layout();
            }
            return;
        }
        self.s.doc = self.confirmed.clone();
        self.s.touch();
        self.s.forget_changes();
        let typed: String = pending.iter().filter_map(|p| if let Intent::Type(t) = &p.intent { Some(t.as_str()) } else { None }).collect();
        let at: Vec<u64> = pending.iter().filter(|p| matches!(p.intent, Intent::Type(_))).flat_map(|p| p.at.iter().copied()).collect();
        if !at.is_empty() {
            let intent = Intent::Type(typed);
            if let Some(cs) = self.run(&intent) {
                self.stats.replays += 1;
                self.send(me, intent, cs, at, now, net);
            }
        }
        for p in pending.into_iter().filter(|p| matches!(p.intent, Intent::Commands(_))) {
            if let Some(cs) = self.run(&p.intent) {
                self.stats.replays += 1;
                self.send(me, p.intent, cs, p.at, now, net);
            }
        }
        if self.layout {
            let _ = self.s.layout();
        }
    }

    fn receive(&mut self, me: usize, m: Msg, now: u64, net: &mut Net) {
        match m {
            Msg::Entry(e) => self.entry(me, &e, now, net),
            Msg::Bounced { seq } => self.bounced(me, seq, now, net),
            Msg::Submit { .. } => {}
        }
    }
}

const SENTENCES: [&str; 12] = [
    "The committee reviewed the quarterly figures and found the trend encouraging.",
    "Several regional offices reported delays in delivering the new equipment.",
    "Our survey of customers suggests that response times matter more than price.",
    "The pilot programme will continue for another six months before a decision.",
    "Training sessions were well attended, although the evening slots filled first.",
    "Costs rose slightly because of higher energy prices across the winter.",
    "The new process cut the average handling time by almost a fifth.",
    "Staff turnover remained low, and most vacancies were filled within weeks.",
    "We recommend publishing the results together with the raw survey data.",
    "A second review is planned once the remaining offices have migrated.",
    "Feedback from partners was mixed but broadly constructive.",
    "The appendix lists the assumptions behind each of the forecasts.",
];

/// A long report in our own words: `paras` paragraphs of three to seven sentences, a short
/// heading line every twelve. The shared paragraph, the one after it and a far one carry words no
/// other has, for the agents to find.
fn fixture(paras: usize) -> (Document, [usize; 3]) {
    let shared = paras / 2;
    let marked = [shared, shared + 1, paras / 5];
    let lines: Vec<String> = (0..paras)
        .map(|i| match marked.iter().position(|&m| m == i) {
            Some(0) => "The shared paragraph: the Wombat team met the Quince group to agree the plan. Everyone else may read it.".into(),
            Some(1) => "The next paragraph: the Heron office sent the Larch figures a day late. Nobody minded much.".into(),
            Some(_) => "A far paragraph: the Kestrel unit audited the Willow accounts and closed the year.".into(),
            None if i % 12 == 0 => format!("Section {}", i / 12 + 1),
            None => (0..3 + i % 5).map(|k| SENTENCES[(i * 7 + k * 5) % SENTENCES.len()]).collect::<Vec<_>>().join(" "),
        })
        .collect();
    let mut d = Document::from_text(&lines.join("\n"));
    d.ensure_block_ids(None);
    (d, marked)
}

/// Agent 1's edits: alternately swap a word in its paragraph, bold one, swap it back, unbold.
fn near_intent(step: usize, words: (&str, &str, &str)) -> Intent {
    let (a, b, bold) = words;
    let (from, to) = if step % 4 < 2 { (a, b) } else { (b, a) };
    Intent::Commands(match step % 2 {
        0 => vec![("select.text", json!({"text": from})), ("text.insert", json!({"text": to}))],
        _ => vec![("select.text", json!({"text": bold})), ("format.bold", json!({}))],
    })
}

/// Agent 2's edits in the far paragraph: swap a word, add a sentence after the paragraph, swap the
/// word back, add another.
fn far_intent(step: usize) -> Intent {
    Intent::Commands(match step % 4 {
        0 => vec![("select.text", json!({"text": "Kestrel"})), ("text.insert", json!({"text": "Falcon"}))],
        2 => vec![("select.text", json!({"text": "Falcon"})), ("text.insert", json!({"text": "Kestrel"}))],
        _ => vec![
            ("select.text", json!({"text": "closed the year."})),
            ("select.collapse", json!({"end": true})),
            ("text.newParagraph", json!({})),
            ("text.insert", json!({"text": format!("Note {step} from the audit agent.")})),
        ],
    })
}

/// Who works beside the person, as copy 1.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Second {
    /// Agent 1, editing words in the person's paragraph.
    AgentSameParagraph,
    /// Agent 1, editing words in the paragraph after it.
    AgentNextParagraph,
    /// A second person, typing in the person's paragraph nearer its start.
    Typist,
}

struct Scenario {
    name: &'static str,
    latency: u64,
    seconds: u64,
    second: Second,
    /// Copies keep one change in flight and send what they did meanwhile as one when it's
    /// acknowledged.
    batch: bool,
}

impl Scenario {
    fn names(&self) -> [&'static str; 3] {
        if self.second == Second::Typist { ["person", "person 2", "agent 2"] } else { NAMES }
    }
}

fn run(sc: &Scenario, doc: &Document, marked: [usize; 3]) {
    let mut hub = Hub { doc: doc.clone(), ..Hub::default() };
    let mut copies = [Copy::new(doc, true, sc.batch), Copy::new(doc, false, sc.batch), Copy::new(doc, false, sc.batch)];
    for c in &copies {
        assert_eq!(c.s.doc, hub.doc);
        assert_eq!(c.s.doc.block_ids(), hub.doc.block_ids());
    }
    // The person types at the end of the shared paragraph.
    let p = &doc.body[marked[0]];
    copies[PERSON].start = Some(Anchor { block: p.id(), off: p.as_para().map_or(0, |p| p.text.len()) });
    // A second person types after "The shared paragraph:".
    if sc.second == Second::Typist {
        copies[NEAR].start = Some(Anchor { block: p.id(), off: "The shared paragraph:".len() });
    }
    let near_words = if sc.second == Second::AgentNextParagraph { ("Heron", "Egret", "Larch") } else { ("Wombat", "Numbat", "Quince") };
    let typing: Vec<char> = " Typed by the person, letter by letter, while the agents work.".chars().collect();
    let typing2: Vec<char> = " A second person adds a clause here,".chars().collect();
    let mut net = Net { latency: sc.latency, hub: VecDeque::new(), copies: (0..3).map(|_| VecDeque::new()).collect() };
    let ticks = sc.seconds * 1000 / TICK_MS;
    let (mut typed, mut typed2, mut near, mut far) = (0usize, 0usize, 0usize, 0usize);
    let mut now = 0u64;
    loop {
        while let Some(Msg::Submit { from, seq, base, cs }) = Net::due(&mut net.hub, now) {
            hub.submit(from, seq, base, cs, &mut net, now);
        }
        for (me, c) in copies.iter_mut().enumerate() {
            while let Some(m) = Net::due(&mut net.copies[me], now) {
                c.receive(me, m, now, &mut net);
            }
        }
        if now < ticks {
            // About 6.7 keys a second for each person; agent 1 every second, agent 2 twice a
            // second.
            if now.is_multiple_of(3) {
                let ch = typing[typed % typing.len()];
                typed += 1;
                copies[PERSON].act(PERSON, Intent::Type(ch.to_string()), now, &mut net);
            }
            match sc.second {
                Second::Typist if now % 3 == 1 => {
                    let ch = typing2[typed2 % typing2.len()];
                    typed2 += 1;
                    copies[NEAR].act(NEAR, Intent::Type(ch.to_string()), now, &mut net);
                }
                Second::AgentSameParagraph | Second::AgentNextParagraph if now % 20 == 7 => {
                    copies[NEAR].act(NEAR, near_intent(near, near_words), now, &mut net);
                    near += 1;
                }
                _ => {}
            }
            if now % 10 == 4 {
                copies[FAR].act(FAR, far_intent(far), now, &mut net);
                far += 1;
            }
        } else if net.idle() && copies.iter().all(|c| c.pending.is_empty() && c.held.is_empty()) {
            break;
        }
        now += 1;
        assert!(now < ticks + 10_000, "{}: the copies never settled", sc.name);
    }
    // Everyone ends with the hub's document, ids and all.
    for (k, c) in copies.iter().enumerate() {
        assert!(c.s.doc == hub.doc && c.s.doc.block_ids() == hub.doc.block_ids(), "{}: {} differs from the hub", sc.name, sc.names()[k]);
        assert!(c.confirmed == hub.doc && c.confirmed.block_ids() == hub.doc.block_ids());
    }
    report(sc, &hub, &copies, doc);
}

fn percentiles(mut v: Vec<f64>) -> String {
    if v.is_empty() {
        return "–".into();
    }
    v.sort_by(f64::total_cmp);
    let at = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
    format!("median {:.0}, p90 {:.0}, max {:.0}", at(0.5), at(0.9), at(1.0))
}

fn micros(v: &[Duration]) -> String {
    percentiles(v.iter().map(|d| d.as_secs_f64() * 1e6).collect())
}

fn report(sc: &Scenario, hub: &Hub, copies: &[Copy; 3], doc: &Document) {
    let mut out = format!(
        "\n## {}\n{} paragraphs; latency {} ms each way; {} s of work; versions at the hub: {}\n",
        sc.name,
        doc.body.len(),
        sc.latency * TICK_MS,
        sc.seconds,
        hub.version
    );
    out += &format!(
        "hub apply, µs: accepted {} ({}); refused {} ({})\n",
        micros(&hub.applied),
        hub.applied.len(),
        micros(&hub.refused),
        hub.refused.len()
    );
    let names = sc.names();
    for (k, c) in copies.iter().enumerate() {
        let s = &c.stats;
        let offered = s.sent - s.replays;
        out += &format!(
            "{}: {} changes ({} keystrokes), {} sent with {} replays; bounced {} ({:.0}% of sent), rebuilt {}; surprises: bounces {}, accepts {}\n",
            names[k],
            offered,
            s.keystrokes,
            s.sent,
            s.replays,
            s.bounced,
            100.0 * s.bounced as f64 / s.sent.max(1) as f64,
            s.rebuilt,
            s.surprise_bounces,
            s.surprise_accepts
        );
        out += &format!("  size, JSON bytes: {}\n", percentiles(s.sizes.iter().map(|&b| b as f64).collect()));
        out += &format!("  until acknowledged, ms: {}\n", percentiles(s.wait.iter().map(|&t| (t * TICK_MS) as f64).collect()));
        out += &format!("  applying another's change, µs: {}\n", micros(&s.remote));
        if !s.remote_layout.is_empty() {
            out += &format!("  … with relayout, µs: {}\n", micros(&s.remote_layout));
            out += &format!("  relayout after an own keystroke, µs: {}\n", micros(&s.local_layout));
        }
        assert_eq!(hub.accepted[k], s.acked + s.surprise_accepts, "{}", names[k]);
        assert_eq!(hub.bounced[k], s.bounced, "{}", names[k]);
    }
    let bounces: usize = hub.bounced.iter().sum();
    out += &format!("bounces a text-splice transform at the hub would have accepted: {} of {}\n", hub.transformable, bounces);
    eprintln!("{out}");
}

#[test]
#[ignore = "a measurement; run by hand"]
fn hub_spike() {
    let (doc, marked) = report_document();
    let mut s = Session::new(doc.clone());
    let pages = s.layout().pages.len();
    eprintln!("# Hub spike\n{} paragraphs, {pages} pages", doc.body.len());
    for (latency, ms) in [(1, 50), (4, 200)] {
        for (second, batch, name) in [
            (Second::AgentSameParagraph, false, "a person and an agent in one paragraph, every change sent at once"),
            (Second::AgentSameParagraph, true, "a person and an agent in one paragraph, one change in flight per copy"),
            (Second::AgentNextParagraph, true, "a person and an agent in neighbouring paragraphs, one change in flight per copy"),
            (Second::Typist, false, "two people typing in one paragraph, every change sent at once"),
            (Second::Typist, true, "two people typing in one paragraph, one change in flight per copy"),
        ] {
            let name: &'static str = Box::leak(format!("{name}, {ms} ms").into_boxed_str());
            run(&Scenario { name, latency, seconds: 60, second, batch }, &doc, marked);
        }
    }
}

/// The 188-page document, as a session would hold it.
fn report_document() -> (Document, [usize; 3]) {
    let (doc, marked) = fixture(PARAGRAPHS);
    (Session::new(doc).doc, marked)
}

/// Paragraphs for about 188 pages.
const PARAGRAPHS: usize = 1_550;

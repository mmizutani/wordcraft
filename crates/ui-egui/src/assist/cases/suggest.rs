//! The lineage suggestion: when the pane shows a document bound to no case, a worker takes the
//! document's print ([`DraftPrint`]) from a clone and compares it with the prints of the most
//! recently changed cases, in every client's folder, up to [`MAX_CASES`] of them and [`MAX_BYTES`]
//! of their files; one worker runs at a time. This is the one thing that reads across walls, and
//! its result goes only to the person's screen: a card with the percentage, the case and **Use
//! it**, **Not this**, **New case**. Nothing is bound without the person's click, and a
//! control-channel click on Use it binds nothing, since Use it runs the person-only
//! `assist.memory.case.bind`. The percentage is shown because two contracts from one template
//! overlap too: it informs the person and decides nothing. The print of a document the person
//! binds goes into the case the same way, off the frame ([`print_bound`]).

use std::collections::BTreeSet;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};

use egui::{RichText, Ui};
use serde_json::json;
use wordcraft_assist::cases::{CaseRef, Click, DraftPrint, PersonOp, Shelf};

use crate::WordApp;
use crate::i18n::fmt;
use crate::theme::Tokens;

/// The least share of the document's paragraphs a past draft must hold to be suggested. Half: a
/// later draft of the same contract keeps most clauses, while two contracts from one template share
/// its boilerplate and differ in the rest, so below half the card would show on every document
/// made from a common template and name a case it has nothing to do with. The percentage is shown
/// either way, so the person decides the ambiguous middle.
pub(crate) const THRESHOLD: f32 = 0.5;

/// Fewest printed paragraphs a document needs before it is compared: a near-empty document
/// matches any case on a signature block.
pub(crate) const MIN_PARAGRAPHS: usize = 8;

/// Accepted changes to the document before its print is taken again while it stays unbound: a
/// person pasting a contract into a new document gets the card without reopening it, and typing
/// doesn't hash the document every frame.
const RECHECK_EVERY: u64 = 64;

/// A document's version: its id and how many changes it has taken.
type Version = (u64, u64);

#[derive(Default)]
pub(crate) struct SuggestState {
    /// The document and version the result stands for, and its body paragraphs then, counted up
    /// to [`MIN_PARAGRAPHS`] ([`paragraphs`]).
    computed: Option<(Version, usize)>,
    pending: Option<(Version, usize, Receiver<Option<Suggestion>>)>,
    found: Option<(u64, Suggestion)>,
    /// Documents the person said Not this for, in this window. No file write.
    dismissed: BTreeSet<u64>,
    /// Test-only: a worker waits for this before its work, so a test can hold one running.
    #[cfg(test)]
    hold: Arc<std::sync::RwLock<()>>,
    /// Test-only: workers started in this window.
    #[cfg(test)]
    started: u32,
}

/// Most cases a worker compares, the most recently changed first: the person is most likely
/// continuing a negotiation they worked on lately, and the scan reads across every client, so it
/// stops at a bound whatever the folder holds.
pub(crate) const MAX_CASES: usize = 128;

/// Most bytes of case files a worker reads, recall's budget for a wall's other cases.
const MAX_BYTES: u64 = 16 << 20;

/// What the card says: the best match at or above [`THRESHOLD`].
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Suggestion {
    pub(crate) at: CaseRef,
    pub(crate) title: String,
    pub(crate) client: String,
    pub(crate) percent: u8,
}

/// The suggestion for the shown document, if any: the worker's result is read, and a worker is
/// started when the document shown has no result yet or has changed by [`RECHECK_EVERY`] since.
/// Nothing while a case is bound or after Not this.
pub(crate) fn refresh(app: &mut WordApp) -> Option<Suggestion> {
    if !app.has_document() || super::bound_for(app).is_some() {
        return None;
    }
    let now = version(app);
    let state = &mut app.assist.cases.suggest;
    if let Some((at, seen, rx)) = &state.pending {
        // Anything but Empty: the worker is done, and one that ended without an answer found nothing.
        let answer = rx.try_recv();
        if !matches!(answer, Err(TryRecvError::Empty)) {
            let (at, seen) = (*at, *seen);
            state.pending = None;
            // A result for a document no longer shown is dropped; the one shown starts its own.
            if at.0 == now.0 {
                state.computed = Some((at, seen));
                state.found = answer.ok().flatten().map(|s| (at.0, s));
            }
        }
    }
    // One worker at a time: another document waits until the running one returns.
    if app.assist.cases.suggest.pending.is_none() && !fresh(app, now) {
        start(app, now);
    }
    let state = &app.assist.cases.suggest;
    if state.dismissed.contains(&now.0) {
        return None;
    }
    state.found.as_ref().filter(|(document, _)| *document == now.0).map(|(_, s)| s.clone())
}

fn version(app: &WordApp) -> Version {
    (app.session.document_id(), app.session.changes().version())
}

/// The shown document's body paragraphs, counted up to [`MIN_PARAGRAPHS`]: a document with
/// fewer was too short to compare when it was last looked at.
fn paragraphs(app: &WordApp) -> usize {
    app.session.doc.para_paths(wordcraft_doc::StoryRef::Body).len().min(MIN_PARAGRAPHS)
}

/// Whether the result stands for the shown document as it is now: the same document, fewer than
/// [`RECHECK_EVERY`] changes since, and, when it was too short to compare then, no paragraph added
/// or removed since. A draft pasted into a blank document is one change, and is looked at again
/// at once rather than 64 changes later. Paragraphs are counted only for a document that was short
/// and has changed since, so a long one costs nothing here.
fn fresh(app: &WordApp, now: Version) -> bool {
    let Some(((document, changes), seen)) = app.assist.cases.suggest.computed else { return false };
    document == now.0 && now.1.saturating_sub(changes) < RECHECK_EVERY && (seen >= MIN_PARAGRAPHS || now.1 == changes || paragraphs(app) == seen)
}

/// Whether the shown document's result is in: nothing pending and a result for its version.
#[cfg(test)]
pub(crate) fn settled(app: &WordApp) -> bool {
    app.assist.cases.suggest.pending.is_none() && fresh(app, version(app))
}

/// The worker: the document's print from a clone, then the cases' prints ([`best`]). Its result
/// comes back through the channel and nowhere else.
fn start(app: &mut WordApp, at: Version) {
    let Some(dir) = app.services.settings_dir.clone() else { return };
    let Ok(today) = super::today() else { return };
    let doc = app.session.doc.clone();
    let name = app.title_stem();
    let ctx = app.ctx.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let seen = paragraphs(app);
    app.assist.cases.suggest.pending = Some((at, seen, rx));
    #[cfg(test)]
    let hold = {
        app.assist.cases.suggest.started += 1;
        Arc::clone(&app.assist.cases.suggest.hold)
    };
    off_frame("case lineage", move || {
        #[cfg(test)]
        drop(hold.read());
        let print = DraftPrint::of(&doc, &name, today);
        let _ = tx.send(best(&Shelf::at(&dir), &print));
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    });
}

/// Not this: hidden for this document in this window, and nothing written.
pub(crate) fn dismiss(app: &mut WordApp) {
    let document = app.session.document_id();
    app.assist.cases.suggest.dismissed.insert(document);
}

/// The person bound the shown document to `at`: its print joins the case, off the frame, through
/// the same typed op as every other write of theirs. A refusal is logged and loses only the print.
pub(crate) fn print_bound(app: &mut WordApp, click: Click, at: &CaseRef) {
    let Some(dir) = app.services.settings_dir.clone() else { return };
    let Ok(today) = super::today() else { return };
    let doc = app.session.doc.clone();
    let name = app.title_stem();
    let at = at.clone();
    off_frame("case draft print", move || {
        let print = DraftPrint::of(&doc, &name, today);
        if let Err(e) = Shelf::at(&dir).person(click, &at, PersonOp::AddDraft(print), today) {
            log::warn!("the document's print wasn't kept with its case: {e}");
        }
    });
}

/// On a thread, or here when none can be made (the web build has no threads). The work waits in a
/// slot the thread takes it from, since a spawn that fails drops the closure it was given.
fn off_frame<F: FnOnce() + Send + 'static>(name: &str, work: F) {
    let slot = Arc::new(Mutex::new(Some(work)));
    let theirs = Arc::clone(&slot);
    let run = move || {
        if let Some(work) = take(&theirs) {
            work();
        }
    };
    if spawn(name, run).is_err()
        && let Some(work) = take(&slot)
    {
        work();
    }
}

fn take<F>(slot: &Mutex<Option<F>>) -> Option<F> {
    slot.lock().unwrap_or_else(PoisonError::into_inner).take()
}

#[cfg(test)]
thread_local! {
    /// Test-only: no thread can be made from this one, as on the web.
    static NO_THREADS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn spawn(name: &str, run: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    #[cfg(test)]
    if NO_THREADS.get() {
        return Err(std::io::Error::other("this test makes no threads"));
    }
    std::thread::Builder::new().name(name.into()).spawn(run).map(drop)
}

/// The card in the pane header, when there is a suggestion.
pub(crate) fn card(app: &mut WordApp, ui: &mut Ui, t: &Tokens) {
    let Some(s) = refresh(app) else { return };
    egui::Frame::NONE.stroke(egui::Stroke::new(1.5, t.accent)).corner_radius(6).inner_margin(6).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let title = if s.title.is_empty() { tl!("Untitled case").to_string() } else { s.title.clone() };
        let line = fmt(
            tl!("{percent}% of this document's paragraphs match a draft of {title} ({client})."),
            &[("percent", &s.percent.to_string()), ("title", &title), ("client", &s.client)],
        );
        ui.add(egui::Label::new(RichText::new(line).font(crate::theme::semibold(12.0)).color(t.text)).wrap());
        ui.add(
            egui::Label::new(
                RichText::new(tl!("Two contracts from one template overlap too, so the number informs you and decides nothing."))
                    .small()
                    .color(t.text_dim),
            )
            .wrap(),
        );
        ui.horizontal_wrapped(|ui| {
            if ui.button(tl!("Use it")).on_hover_text(tl!("Bind this document to that case.")).clicked() {
                super::run(app, "assist.memory.case.bind", json!({"wall": s.at.wall.as_str(), "case": s.at.case.as_str()}));
            }
            if ui.button(tl!("Not this")).clicked() {
                dismiss(app);
            }
            if ui.button(tl!("New case")).clicked() {
                app.assist.view = crate::assist::PaneView::Details;
            }
        });
    });
}

/// The best match for `print` among the prints of the [`MAX_CASES`] most recently changed cases,
/// at or above [`THRESHOLD`]: the worker's work. Case files are read once each, as the shelf reads
/// them: a link is refused, a damaged file skipped. Only the best one's client name is read.
fn best(shelf: &Shelf, print: &DraftPrint) -> Option<Suggestion> {
    if print.len() < MIN_PARAGRAPHS {
        return None;
    }
    let mut best: Option<(f32, CaseRef, String)> = None;
    for (at, case) in shelf.newest_cases(MAX_CASES, MAX_BYTES) {
        let Some(share) = case.drafts.iter().map(|d| print.overlap(d)).reduce(f32::max) else { continue };
        if share >= THRESHOLD && best.as_ref().is_none_or(|(b, ..)| share > *b) {
            best = Some((share, at, case.title.for_person().to_string()));
        }
    }
    let (share, at, title) = best?;
    let client = shelf.wall_name(&at.wall).for_person().to_string();
    Some(Suggestion { at, title, client, percent: (share * 100.0).round().clamp(0.0, 100.0) as u8 })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wordcraft_assist::cases::Shelf;
    use wordcraft_doc::Document;

    use super::super::tests::{Cases, TODAY, cases, click, frames_until, harness};
    use super::*;
    use crate::assist::cases::bound_for;
    use crate::assist::memory::tests::{Folder, control_click, window};

    fn clauses(n: usize) -> Vec<String> {
        (0..n)
            .map(|i| format!("Clause {i}. Kestrel Freight KK shall perform the services described in schedule {i} with reasonable skill and care."))
            .collect()
    }

    fn document(paragraphs: &[String]) -> Document {
        Document::from_text(&paragraphs.join("\n"))
    }

    /// A window on `paragraphs`, in `folder`.
    fn window_on(folder: &Folder, paragraphs: &[String]) -> WordApp {
        let mut a = window(folder);
        a.session.set_document(document(paragraphs));
        a
    }

    /// Bind a window on `paragraphs` to `c.here` as the person and wait for the print to be in
    /// the case's file.
    fn bind_first(folder: &Folder, c: &Cases, paragraphs: &[String]) {
        let mut a = window_on(folder, paragraphs);
        a.run("assist.memory.case.bind", json!({"wall": c.here.wall.as_str(), "case": c.here.case.as_str()})).unwrap();
        assert_eq!(bound_for(&a), Some(c.here.clone()));
        let shelf = Shelf::at(&folder.0);
        frames_until(&mut a, "the print never reached the case", |_| shelf.open(&c.here).unwrap().drafts.len() == 1);
        let case = shelf.open(&c.here).unwrap();
        assert_eq!(case.drafts[0].len(), paragraphs.len());
        assert_eq!(case.drafts[0].name.for_person(), a.title_stem());
    }

    /// Frames until the worker's result for the shown document is in; what it is.
    fn suggestion(a: &mut WordApp) -> Option<Suggestion> {
        frames_until(a, "the suggestion worker never answered", |a| {
            refresh(a);
            settled(a)
        });
        refresh(a)
    }

    /// A blank document is too short to compare when the pane first shows it; a later draft
    /// pasted into it in one change is looked at again then, not 64 changes later.
    #[test]
    fn a_draft_pasted_into_a_blank_document_is_suggested() {
        let folder = Folder::new();
        let c = cases(&folder);
        let first = clauses(30);
        bind_first(&folder, &c, &first);
        let mut b = window_on(&folder, &[String::new()]);
        assert_eq!(suggestion(&mut b), None, "a blank document matches nothing");
        b.execute("text.insert", json!({"paragraphs": true, "text": first.join("\n")})).unwrap();
        let s = suggestion(&mut b).expect("the pasted draft is suggested");
        assert_eq!(s.at, c.here);
        assert_eq!(bound_for(&b), None, "a suggestion binds nothing");
    }

    #[test]
    fn a_later_draft_of_a_bound_cases_document_is_suggested_and_an_unrelated_one_is_not() {
        let folder = Folder::new();
        let c = cases(&folder);
        let first = clauses(30);
        bind_first(&folder, &c, &first);
        let mut later = first.clone();
        for i in [3usize, 7, 11, 19, 25] {
            later[i] = format!("Clause {i}. Kestrel Freight KK shall perform the services in schedule {i} with the care of a prudent operator.");
        }
        later.truncate(27);
        later.extend((30..34).map(|i| format!("Clause {i}. Bluefin Systems Ltd may audit the records once a year on thirty days' notice.")));
        let mut b = window_on(&folder, &later);
        let s = suggestion(&mut b).expect("the later draft is suggested");
        assert_eq!(s.at, c.here);
        assert_eq!(s.percent, 71, "22 of 31 paragraphs");
        assert_eq!(s.title, "Acme MSA");
        assert_eq!(s.client, "Client 1");
        assert_eq!(bound_for(&b), None, "a suggestion binds nothing");
        let unrelated: Vec<String> =
            (0..30).map(|i| format!("Section {i}. The tenant shall keep the premises at unit {i} in good repair and insured.")).collect();
        let mut u = window_on(&folder, &unrelated);
        assert_eq!(suggestion(&mut u), None);
        assert_eq!(Shelf::at(&folder.0).open(&c.here).unwrap().drafts.len(), 1, "a suggestion writes nothing");
    }

    #[test]
    fn two_contracts_from_one_template_are_suggested_with_their_percentage_and_not_this_hides_it() {
        let folder = Folder::new();
        let c = cases(&folder);
        let template = clauses(12);
        let mut first = template.clone();
        first.extend((0..8).map(|i| format!("Schedule A item {i}. Kestrel Freight KK pays 1,200,000 yen a month for the Osaka route.")));
        bind_first(&folder, &c, &first);
        let mut second = template;
        second.extend((0..8).map(|i| format!("Schedule B item {i}. Bluefin Systems Ltd pays 900 pounds a month for the Leeds route.")));
        let mut b = window_on(&folder, &second);
        let s = suggestion(&mut b).expect("the template's share is suggested");
        assert_eq!((s.at.clone(), s.percent), (c.here.clone(), 60));
        assert_eq!(bound_for(&b), None, "never bound without a click");
        let file = folder.0.join("assistant-cases").join(c.here.wall.as_str()).join(c.here.case.as_str()).join("case.json");
        let before = std::fs::read(&file).unwrap();
        dismiss(&mut b);
        assert_eq!(refresh(&mut b), None);
        assert!(settled(&b), "Not this starts no worker");
        for _ in 0..3 {
            crate::assist::tick(&mut b);
        }
        assert_eq!(refresh(&mut b), None, "hidden for this document");
        assert_eq!(std::fs::read(&file).unwrap(), before, "Not this writes nothing");
        // Another document in the same window is suggested again, and an unrelated one shown
        // next gets no card, not the last document's, while its worker runs or after.
        b.session.set_document(document(&second));
        assert!(suggestion(&mut b).is_some());
        let unrelated: Vec<String> =
            (0..30).map(|i| format!("Section {i}. The tenant shall keep the premises at unit {i} in good repair and insured.")).collect();
        b.session.set_document(document(&unrelated));
        assert_eq!(refresh(&mut b), None, "the last document's card while this one's worker runs");
        assert_eq!(suggestion(&mut b), None);
    }

    #[test]
    fn a_worker_compares_only_the_most_recently_changed_cases_up_to_the_cap() {
        let folder = Folder::new();
        let c = cases(&folder);
        let first = clauses(30);
        bind_first(&folder, &c, &first);
        let shelf = Shelf::at(&folder.0);
        let wall = shelf.new_wall(click(), "Client 3", TODAY, 10).unwrap();
        for n in 0..MAX_CASES as u64 {
            shelf.new_case(click(), &wall, &format!("Matter {n}"), TODAY, 100 + n).unwrap();
        }
        let changed = |when: std::time::SystemTime| {
            let file = folder.0.join("assistant-cases").join(c.here.wall.as_str()).join(c.here.case.as_str()).join("case.json");
            std::fs::File::options().write(true).open(file).unwrap().set_modified(when).unwrap();
        };
        changed(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000));
        let mut oldest = window_on(&folder, &first);
        assert_eq!(suggestion(&mut oldest), None, "more than the cap changed since the matching case");
        changed(std::time::SystemTime::now() + std::time::Duration::from_secs(3600));
        let mut newest = window_on(&folder, &first);
        assert_eq!(suggestion(&mut newest).map(|s| s.at), Some(c.here.clone()), "the matching case changed last");
    }

    #[test]
    fn switching_documents_while_a_worker_runs_starts_no_second_one_and_its_late_result_is_not_shown() {
        let folder = Folder::new();
        let c = cases(&folder);
        let first = clauses(30);
        bind_first(&folder, &c, &first);
        let mut b = window_on(&folder, &first);
        let hold = Arc::clone(&b.assist.cases.suggest.hold);
        let held = hold.write().unwrap();
        assert_eq!(refresh(&mut b), None);
        for place in ["premises", "warehouse", "garage"] {
            let unrelated: Vec<String> =
                (0..30).map(|i| format!("Section {i}. The tenant shall keep the {place} at unit {i} in good repair and insured.")).collect();
            b.session.set_document(document(&unrelated));
            assert_eq!(refresh(&mut b), None);
        }
        assert_eq!(b.assist.cases.suggest.started, 1, "the first document's worker is still running");
        drop(held);
        frames_until(&mut b, "the shown document's worker never answered", |a| {
            assert_eq!(refresh(a), None, "the first document's result, shown on another");
            settled(a)
        });
        assert_eq!(b.assist.cases.suggest.started, 2, "the shown document's worker started after the first returned");
        b.session.set_document(document(&first));
        assert_eq!(suggestion(&mut b).map(|s| s.percent), Some(100), "the dropped result is taken again");
    }

    #[test]
    fn where_no_thread_can_be_made_the_work_runs_on_the_frame() {
        let folder = Folder::new();
        let c = cases(&folder);
        let first = clauses(30);
        NO_THREADS.set(true);
        let mut a = window_on(&folder, &first);
        a.run("assist.memory.case.bind", json!({"wall": c.here.wall.as_str(), "case": c.here.case.as_str()})).unwrap();
        assert_eq!(Shelf::at(&folder.0).open(&c.here).unwrap().drafts.len(), 1, "the print was kept with no frame waited");
        let mut b = window_on(&folder, &first);
        refresh(&mut b);
        assert_eq!(refresh(&mut b).map(|s| s.at), Some(c.here.clone()), "the suggestion came with no frame waited");
        NO_THREADS.set(false);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_control_channel_click_on_use_it_binds_nothing_and_the_persons_click_binds() {
        use egui_kittest::kittest::Queryable;
        let folder = Folder::new();
        let c = cases(&folder);
        let first = clauses(30);
        bind_first(&folder, &c, &first);
        let mut b = window_on(&folder, &first);
        b.session.view.assistant_pane = true;
        assert_eq!(suggestion(&mut b).map(|s| s.percent), Some(100));
        let mut h = harness(b);
        assert!(h.query_by_label_contains("100%").is_some(), "the card shows the percentage");
        control_click(&mut h, "Use it");
        assert_eq!(bound_for(h.state()), None, "the control channel's Use it binds nothing");
        assert!(h.query_by_label("Use it").is_some(), "the card is still there");
        h.get_by_label("Use it").click();
        h.run_steps(3);
        assert_eq!(bound_for(h.state()), Some(c.here.clone()), "the person's Use it binds");
        assert!(h.query_by_label("Use it").is_none(), "bound: no card");
        let shelf = Shelf::at(&folder.0);
        frames_until(h.state_mut(), "the second print never reached the case", |_| shelf.open(&c.here).unwrap().drafts.len() == 2);
    }
}

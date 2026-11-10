//! The lesson card (#107, decision 2): the one place text leaves a client. It shows the exact text,
//! flags any word that names a party of the case, lists every fact that would travel with a tick
//! to drop each, says the lesson goes to every client's cases, and stores nothing until Keep,
//! which runs `assist.memory.lesson.keep`, the person's command. A model's `lesson` marker and
//! the page's **Make a lesson** open the same card; the list of kept lessons is here too.

use egui::{RichText, Ui};
use serde_json::json;
use wordcraft_assist::ProposalAnswer;
use wordcraft_assist::cases::record::MAX_TEXT;
use wordcraft_assist::cases::{Applies, Case, CaseRef, Day, KeptLesson, LessonProposal, Quoted, names_in};

use super::page::{KINDS, PAPERS, SIDES, STANCES, THEIRS, word};
use super::run;
use crate::WordApp;
use crate::i18n::fmt;
use crate::options::note;
use crate::theme::{Tokens, semibold};

/// One card's state while it waits: the text as edited, the facts left ticked, and whether the
/// person confirmed a party's name may go.
pub(crate) struct LessonCard {
    pub(crate) text: String,
    applies: Applies,
    /// The case's parties when the card opened, for the name check.
    parties: Vec<Quoted>,
    from: Option<CaseRef>,
    named_ok: bool,
    editing: bool,
}

impl LessonCard {
    /// A card for `proposal` from `case`: the facts as of the first note it names, else today.
    pub(crate) fn new(proposal: &LessonProposal, at: Option<&CaseRef>, case: Option<&Case>, today: Day) -> LessonCard {
        let on = case.and_then(|c| c.notes.iter().find(|n| proposal.notes.contains(&n.id)).map(|n| n.at)).unwrap_or(today);
        LessonCard {
            text: proposal.text.for_person().to_string(),
            applies: case.map(|c| c.applies(on)).unwrap_or_default(),
            parties: case.map(|c| c.facts.parties.clone()).unwrap_or_default(),
            from: at.cloned(),
            named_ok: false,
            editing: false,
        }
    }

    fn names(&self) -> Vec<String> {
        names_in(&self.text, &self.parties)
    }
}

/// What the person did on the card this frame.
#[derive(PartialEq, Eq)]
pub(crate) enum Answer {
    Kept,
    NotThis,
}

/// The facts of `applies` as the card and the list name them, one line each.
pub(crate) fn fact_lines(applies: &Applies) -> Vec<String> {
    let mut lines = Vec::new();
    let mut say = |name: &str, v: Option<String>| {
        if let Some(v) = v {
            lines.push(format!("{name}: {v}"));
        }
    };
    say(tl!("Contract"), applies.kind.map(|v| word(&KINDS, v).to_string()));
    say(tl!("Our side"), applies.side.map(|v| word(&SIDES, v).to_string()));
    say(tl!("Paper"), applies.paper.map(|v| word(&PAPERS, v).to_string()));
    say(tl!("Counterparty"), applies.theirs.map(|v| word(&THEIRS, v).to_string()));
    say(tl!("Law"), applies.law.map(String::from));
    say(tl!("Balance then"), applies.stance.map(|v| word(&STANCES, v).to_string()));
    say(tl!("Year"), applies.year.map(|y| y.to_string()));
    lines
}

/// The card: the text, the name check, the facts with a tick each, and Keep, Edit, Not this.
/// Keep runs the command and says whether it was stored.
pub(crate) fn card(app: &mut WordApp, ui: &mut Ui, t: &Tokens, c: &mut LessonCard) -> Option<Answer> {
    let mut answer = None;
    egui::Frame::NONE.stroke(egui::Stroke::new(1.5, t.accent)).corner_radius(6).inner_margin(6).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(tl!("Keep this as a lesson?")).small().color(t.text_dim));
        if c.editing {
            ui.add(egui::TextEdit::multiline(&mut c.text).char_limit(MAX_TEXT).desired_rows(2).desired_width(f32::INFINITY));
        } else {
            ui.add(egui::Label::new(RichText::new(&c.text).font(semibold(12.0)).color(t.text)).wrap());
        }
        let names = c.names();
        if !names.is_empty() {
            let says = fmt(tl!("This names a party of this case: {names}. Reword it, or keep it with the name."), &[("names", &names.join(", "))]);
            ui.add(egui::Label::new(RichText::new(says).small().color(t.orange)).wrap());
            ui.checkbox(&mut c.named_ok, tl!("Keep it with the name"));
        }
        ui.label(RichText::new(tl!("These facts go with it, so a similar deal finds it. Untick one to leave it out.")).small().color(t.text_dim));
        facts_ticks(ui, &mut c.applies);
        ui.label(RichText::new(tl!("This goes to every client's cases. Nothing else of this case does.")).small().color(t.text_dim));
        let can = !c.text.trim().is_empty() && (names.is_empty() || c.named_ok);
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(can, egui::Button::new(tl!("Keep"))).clicked() {
                let params = json!({"text": c.text.trim(), "applies": c.applies, "from": c.from.as_ref().map(|at| at.case.as_str())});
                if run(app, "assist.memory.lesson.keep", params) {
                    answer = Some(Answer::Kept);
                }
            }
            if ui.button(tl!("Edit")).clicked() {
                c.editing = !c.editing;
            }
            if ui.button(tl!("Not this")).clicked() {
                answer = Some(Answer::NotThis);
            }
        });
    });
    answer
}

/// A tick per fact that is set, named as [`fact_lines`] names it: unticked, it is dropped from
/// what travels.
fn facts_ticks(ui: &mut Ui, applies: &mut Applies) {
    fn tick<T: Copy>(ui: &mut Ui, slot: &mut Option<T>, name: &str, shown: impl Fn(T) -> String) {
        if let Some(v) = *slot {
            let mut on = true;
            ui.checkbox(&mut on, format!("{name}: {}", shown(v)));
            *slot = on.then_some(v);
        }
    }
    tick(ui, &mut applies.kind, tl!("Contract"), |v| word(&KINDS, v).to_string());
    tick(ui, &mut applies.side, tl!("Our side"), |v| word(&SIDES, v).to_string());
    tick(ui, &mut applies.paper, tl!("Paper"), |v| word(&PAPERS, v).to_string());
    tick(ui, &mut applies.theirs, tl!("Counterparty"), |v| word(&THEIRS, v).to_string());
    tick(ui, &mut applies.law, tl!("Law"), String::from);
    tick(ui, &mut applies.stance, tl!("Balance then"), |v| word(&STANCES, v).to_string());
    tick(ui, &mut applies.year, tl!("Year"), |y| y.to_string());
}

/// A card under an answer ([`wordcraft_assist::Entry::CaseCard`] with [`wordcraft_assist::cases::Card::Lesson`]):
/// built from the conversation's case the first time it shows, answered through the conversation.
/// Not this runs `assist.memory.case.cardDismiss`, so the control channel's click leaves it waiting.
pub(crate) fn transcript_card(app: &mut WordApp, ui: &mut Ui, t: &Tokens, id: u64, proposal: &LessonProposal, answer: Option<ProposalAnswer>) {
    if let Some(answer) = answer {
        app.assist.cases.lesson_cards.remove(&id);
        let said = match answer {
            ProposalAnswer::Kept => tl!("Kept as a lesson for every client's cases"),
            ProposalAnswer::Dismissed => tl!("Not kept"),
        };
        egui::Frame::NONE.stroke(egui::Stroke::new(1.0, t.border_strong)).corner_radius(6).inner_margin(6).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(said).small().color(t.text_dim));
            ui.add(egui::Label::new(RichText::new(proposal.text.for_person()).font(semibold(12.0)).color(t.text_dim)).wrap());
        });
        return;
    }
    if !super::rows_act(app) {
        return;
    }
    // The conversation's own case, as a marker's note is routed, never the document's current
    // binding; and a cached card is reused only while it is that case's, since card ids restart
    // with each conversation and a card from an earlier one could carry another case's names
    // and facts.
    let at = app.assist.conversation.as_ref().and_then(|c| c.case.clone());
    let mut c = match app.assist.cases.lesson_cards.remove(&id).filter(|c| c.from == at) {
        Some(c) => c,
        None => {
            let (Ok(today), Some(dir)) = (super::today(), app.services.settings_dir.clone()) else { return };
            let case = at.as_ref().and_then(|at| wordcraft_assist::cases::Shelf::at(&dir).open(at).ok());
            LessonCard::new(proposal, at.as_ref(), case.as_ref(), today)
        }
    };
    match card(app, ui, t, &mut c) {
        Some(Answer::Kept) => {
            if let Some(a) = app.conversation_mut() {
                a.answer_case_card(id, ProposalAnswer::Kept);
            }
        }
        Some(Answer::NotThis) => {
            run(app, "assist.memory.case.cardDismiss", json!({"card": id}));
            app.assist.cases.lesson_cards.insert(id, c);
        }
        None => {
            app.assist.cases.lesson_cards.insert(id, c);
        }
    }
}

/// The page's card, opened by **Make a lesson** on a confirmed note's row: the same card, closed
/// by Keep or Not this, which here closes a form and stores nothing.
pub(crate) fn page_card(app: &mut WordApp, ui: &mut Ui, t: &Tokens) {
    let Some(mut c) = app.assist.cases.drafts.lesson.take() else { return };
    if card(app, ui, t, &mut c).is_none() {
        app.assist.cases.drafts.lesson = Some(c);
    }
}

/// Details › Case, and File › Options › Assistant with no document: every lesson kept, with Edit,
/// Off or On, and Remove, and the facts each travels with.
pub(crate) fn list(app: &mut WordApp, ui: &mut Ui, t: &Tokens) {
    ui.add_space(4.0);
    ui.label(RichText::new(tl!("Lessons")).font(semibold(11.5)));
    note(ui, "Lessons you kept go to every client's cases, as this text and these facts only.");
    let lessons = super::lessons(app);
    if lessons.damaged {
        ui.label(
            RichText::new(tl!("The lessons file can't be read whole; your next change moves it aside as assistant-lessons.json.damaged."))
                .small()
                .color(t.orange),
        );
    }
    if lessons.lessons.is_empty() {
        ui.label(RichText::new(tl!("No lessons yet.")).small().color(t.text_dim));
        return;
    }
    for l in &lessons.lessons {
        ui.push_id(("lesson", l.id.to_string()), |ui| row(app, ui, t, l));
    }
}

fn row(app: &mut WordApp, ui: &mut Ui, t: &Tokens, l: &KeptLesson) {
    let id = l.id.to_string();
    if let Some((editing, mut text)) = app.assist.cases.drafts.lesson_edit.take()
        && editing == l.id
    {
        ui.add(egui::TextEdit::multiline(&mut text).char_limit(MAX_TEXT).desired_rows(2).desired_width(f32::INFINITY));
        let mut open = true;
        ui.horizontal(|ui| {
            if ui.add_enabled(!text.trim().is_empty(), egui::Button::new(tl!("Save")).small()).clicked() {
                open = !run(app, "assist.memory.lesson.edit", json!({"id": id, "text": text.trim()}));
            }
            if ui.small_button(tl!("Cancel")).clicked() {
                open = false;
            }
        });
        if open {
            app.assist.cases.drafts.lesson_edit = Some((editing, text));
        }
        return;
    }
    let color = if l.off { t.text_dim } else { t.text };
    ui.add(egui::Label::new(RichText::new(l.text.for_person()).small().color(color)).wrap());
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(&id).small().color(t.text_dim));
        let facts = fact_lines(&l.applies);
        if !facts.is_empty() {
            ui.label(RichText::new(facts.join(", ")).small().color(t.text_dim));
        }
        if l.off {
            ui.label(RichText::new(tl!("off")).small().color(t.orange));
        }
        if ui.small_button(tl!("Edit")).clicked() {
            app.assist.cases.drafts.lesson_edit = Some((l.id, l.text.for_person().to_string()));
        }
        if ui.small_button(if l.off { tl!("On") } else { tl!("Off") }).clicked() {
            run(app, "assist.memory.lesson.off", json!({"id": id, "value": !l.off}));
        }
        if ui.small_button(tl!("Remove")).clicked() {
            run(app, "assist.memory.lesson.remove", json!({"id": id}));
        }
    });
}

//! Details › Case: the shown document's case, or the cases to bind it to. Every change goes through
//! an `assist.memory.case.*` command by `app.run`, so a click the control channel made changes
//! nothing. Interface text is translated; a case's own text (titles, names, notes) is the person's
//! and shown as it is.

use egui::{RichText, Ui};
use serde_json::{Value, json};
use wordcraft_assist::cases::record::{MAX_FACTORS, MAX_SHORT, MAX_TEXT};
use wordcraft_assist::cases::{
    BalanceDraft, Body, Case, CaseRef, ContractKind, CounterpartyKind, FactorKind, Facts, Kind, Lang, Listing, Note, NoteDraft, OurSide, Paper, Plan,
    Quoted, Side, SizeBand, Stage, Stance, Standing, Topic, WallId,
};

use crate::WordApp;
use crate::i18n::fmt;
use crate::options::note;
use crate::theme::{Tokens, semibold};

/// What the page is in the middle of. Nothing here is stored until Save or Create.
#[derive(Default)]
pub(crate) struct Drafts {
    pub(crate) new_case: NewCase,
    /// The case the forms below were opened on. A save into any other case is refused, so text
    /// typed for one client never lands in another's file.
    pub(crate) on: Option<CaseRef>,
    /// The names and the law being edited.
    facts: Option<FactsText>,
    reassess: Option<Reassess>,
    /// The plan being edited: its goal, and its steps one per line.
    plan: Option<(String, String)>,
    /// The note being written or edited.
    note: Option<NoteForm>,
    /// Delete case was clicked: the page asks.
    deleting: bool,
}

impl Drafts {
    fn any_form(&self) -> bool {
        self.facts.is_some() || self.reassess.is_some() || self.plan.is_some() || self.note.is_some() || self.deleting
    }

    /// Every form of the bound case closes, unsaved. New case stays: it belongs to no case.
    pub(crate) fn close_forms(&mut self) {
        self.on = None;
        self.facts = None;
        self.reassess = None;
        self.plan = None;
        self.note = None;
        self.deleting = false;
    }
}

#[derive(Default)]
pub(crate) struct NewCase {
    pub(crate) title: String,
    /// `None`: a new client, named in `client`.
    pub(crate) wall: Option<WallId>,
    pub(crate) client: String,
}

struct FactsText {
    parties: String,
    law: String,
}

struct Reassess {
    stance: Stance,
    factors: Vec<(FactorKind, Side, String)>,
}

/// A note's fields, every kind's at once, so switching the kind keeps what was typed.
struct NoteForm {
    id: Option<String>,
    kind: Kind,
    main: String,
    short: String,
    how: String,
    favors: Side,
    until: String,
    topic: Topic,
    landed: bool,
}

impl NoteForm {
    fn empty() -> NoteForm {
        NoteForm {
            id: None,
            kind: Kind::Pattern,
            main: String::new(),
            short: String::new(),
            how: String::new(),
            favors: Side::Us,
            until: String::new(),
            topic: Topic::Liability,
            landed: false,
        }
    }

    fn of(note: &Note) -> NoteForm {
        let mut form = NoteForm::from_body(&note.body, note.how.as_ref());
        form.id = Some(note.id.to_string());
        form
    }

    /// A new note's form, filled from `body` and `how`.
    fn from_body(body: &Body, how: Option<&Quoted>) -> NoteForm {
        let mut form = NoteForm::empty();
        form.kind = body.kind();
        form.how = how.map(|q| q.for_person().to_string()).unwrap_or_default();
        let mut texts = body.texts().map(|q| q.for_person().to_string());
        form.main = texts.next().unwrap_or_default();
        form.short = texts.next().unwrap_or_default();
        match body {
            Body::Leverage { favors, until, .. } => {
                form.favors = *favors;
                form.until = until.map(String::from).unwrap_or_default();
            }
            Body::Position { topic, moved, landed, .. } => {
                form.topic = *topic;
                form.favors = *moved;
                form.landed = *landed;
            }
            _ => {}
        }
        form
    }

    /// The note as `assist.memory.case.note` takes it. The record parses it again on the way in.
    fn to_json(&self) -> Value {
        let short = (!self.short.trim().is_empty()).then(|| self.short.trim());
        let note = match self.kind {
            Kind::Pattern => json!({"kind": "pattern", "rule": self.main.trim(), "traps": short}),
            Kind::Lesson => json!({"kind": "lesson", "rule": self.main.trim(), "traps": short}),
            Kind::Pitfall => json!({"kind": "pitfall", "trap": self.main.trim(), "guard": short}),
            Kind::Leverage => {
                let until = (!self.until.trim().is_empty()).then(|| self.until.trim());
                json!({"kind": "leverage", "what": self.main.trim(), "favors": self.favors, "until": until})
            }
            Kind::Position => {
                json!({"kind": "position", "topic": self.topic, "moved": self.favors, "terms": self.main.trim(), "traded_for": short, "landed": self.landed})
            }
        };
        let how = (!self.how.trim().is_empty()).then(|| self.how.trim());
        json!({"id": self.id, "note": note, "how": how})
    }
}

pub(super) const KINDS: [(ContractKind, &str); 12] = [
    (ContractKind::Nda, "NDA"),
    (ContractKind::Msa, "Master services agreement"),
    (ContractKind::Saas, "SaaS"),
    (ContractKind::Licence, "Licence"),
    (ContractKind::Services, "Services"),
    (ContractKind::Supply, "Supply"),
    (ContractKind::Distribution, "Distribution"),
    (ContractKind::Employment, "Employment"),
    (ContractKind::Lease, "Lease"),
    (ContractKind::Loan, "Loan"),
    (ContractKind::Acquisition, "Acquisition"),
    (ContractKind::Other, "Other"),
];

pub(super) const SIDES: [(OurSide, &str); 13] = [
    (OurSide::Vendor, "Vendor"),
    (OurSide::Customer, "Customer"),
    (OurSide::Licensor, "Licensor"),
    (OurSide::Licensee, "Licensee"),
    (OurSide::Employer, "Employer"),
    (OurSide::Employee, "Employee"),
    (OurSide::Lender, "Lender"),
    (OurSide::Borrower, "Borrower"),
    (OurSide::Buyer, "Buyer"),
    (OurSide::Seller, "Seller"),
    (OurSide::Landlord, "Landlord"),
    (OurSide::Tenant, "Tenant"),
    (OurSide::Other, "Other"),
];

pub(super) const PAPERS: [(Paper, &str); 3] = [(Paper::Ours, "Ours"), (Paper::Theirs, "Theirs"), (Paper::ThirdParty, "A third party's")];

pub(super) const THEIRS: [(CounterpartyKind, &str); 6] = [
    (CounterpartyKind::Startup, "Startup"),
    (CounterpartyKind::Sme, "Small or medium business"),
    (CounterpartyKind::Enterprise, "Enterprise"),
    (CounterpartyKind::Public, "Public body"),
    (CounterpartyKind::Individual, "Individual"),
    (CounterpartyKind::Other, "Other"),
];

pub(super) const SIZES: [(SizeBand, &str); 4] =
    [(SizeBand::Small, "Small"), (SizeBand::Medium, "Medium"), (SizeBand::Large, "Large"), (SizeBand::Huge, "Huge")];

pub(super) const LANGS: [(Lang, &str); 3] = [(Lang::En, "English"), (Lang::Ja, "Japanese"), (Lang::Other, "Other")];

const STAGES: [(Stage, &str); 3] = [(Stage::Negotiating, "Negotiating"), (Stage::Signed, "Signed"), (Stage::Abandoned, "Abandoned")];

const STANCES: [(Stance, &str); 3] = [(Stance::Stronger, "Stronger"), (Stance::Even, "Even"), (Stance::Weaker, "Weaker")];

const FACTORS: [(FactorKind, &str); 7] = [
    (FactorKind::Dependence, "Dependence"),
    (FactorKind::Alternatives, "Alternatives"),
    (FactorKind::Deadline, "Deadline"),
    (FactorKind::SoleSource, "Sole source"),
    (FactorKind::RepeatRelationship, "Repeat relationship"),
    (FactorKind::Pen, "Holds the pen"),
    (FactorKind::Other, "Other"),
];

const FAVORS: [(Side, &str); 3] = [(Side::Us, "Us"), (Side::Them, "Them"), (Side::Neither, "Neither")];

const NOTE_KINDS: [(Kind, &str); 5] =
    [(Kind::Pattern, "Pattern"), (Kind::Pitfall, "Pitfall"), (Kind::Lesson, "Lesson"), (Kind::Leverage, "Leverage"), (Kind::Position, "Position")];

const TOPICS: [(Topic, &str); 11] = [
    (Topic::Liability, "Liability"),
    (Topic::Indemnity, "Indemnity"),
    (Topic::Ip, "IP"),
    (Topic::Confidentiality, "Confidentiality"),
    (Topic::Termination, "Termination"),
    (Topic::DataProtection, "Data protection"),
    (Topic::GoverningLaw, "Governing law"),
    (Topic::Assignment, "Assignment"),
    (Topic::Warranties, "Warranties"),
    (Topic::Sla, "SLA"),
    (Topic::Payment, "Payment"),
];

pub(super) fn word<T: PartialEq + Copy>(table: &[(T, &'static str)], value: T) -> &'static str {
    table.iter().find(|(v, _)| *v == value).map_or("", |(_, w)| tl!(w))
}

/// A picker over `table`, with a row for none when `optional`: what it was changed to, if it was.
fn picker<T: PartialEq + Copy>(
    ui: &mut Ui,
    salt: &str,
    label: &str,
    table: &[(T, &'static str)],
    current: Option<T>,
    optional: bool,
) -> Option<Option<T>> {
    let mut chosen = None;
    ui.horizontal(|ui| {
        let name = ui.label(RichText::new(tl!(label)).small());
        let shown = current.map_or_else(|| tl!("Not set").to_string(), |v| word(table, v).to_string());
        let combo = craft_ui::ComboBox::from_id_salt(salt).selected_text(shown).width(170.0).show_ui(ui, |ui| {
            if optional && ui.selectable_label(current.is_none(), tl!("Not set")).clicked() {
                chosen = Some(None);
            }
            for (value, w) in table {
                if ui.selectable_label(current == Some(*value), tl!(w)).clicked() {
                    chosen = Some(Some(*value));
                }
            }
        });
        combo.response.labelled_by(name.id);
    });
    chosen
}

/// A one-line box labelled for a screen reader.
fn line(ui: &mut Ui, label: &str, text: &mut String, limit: usize) {
    ui.horizontal(|ui| {
        let name = ui.label(RichText::new(tl!(label)).small());
        ui.add(egui::TextEdit::singleline(text).char_limit(limit).desired_width(f32::INFINITY)).labelled_by(name.id);
    });
}

fn lines(ui: &mut Ui, label: &str, text: &mut String, limit: usize) {
    let name = ui.label(RichText::new(tl!(label)).small());
    ui.add(egui::TextEdit::multiline(text).char_limit(limit).desired_rows(2).desired_width(f32::INFINITY)).labelled_by(name.id);
}

/// Open a form on the bound case, from a row or a card under an answer: Details shows it.
fn open_form(app: &mut WordApp, fill: impl FnOnce(&mut Drafts)) {
    let on = super::bound_for(app);
    let drafts = &mut app.assist.cases.drafts;
    drafts.close_forms();
    fill(drafts);
    drafts.on = on;
    app.assist.view = crate::assist::PaneView::Details;
}

/// Edit note `n` of the bound case.
pub(super) fn edit_note(app: &mut WordApp, n: &Note) {
    let form = NoteForm::of(n);
    open_form(app, |d| d.note = Some(form));
}

/// Write a note from a card's draft, as a new one.
pub(super) fn draft_note(app: &mut WordApp, draft: &NoteDraft) {
    let form = NoteForm::from_body(&draft.body, draft.how.as_ref());
    open_form(app, |d| d.note = Some(form));
}

/// Edit a plan from a card.
pub(super) fn draft_plan(app: &mut WordApp, plan: &Plan) {
    let goal = plan.goal.for_person().to_string();
    let next = plan.next.iter().map(|s| s.for_person()).collect::<Vec<_>>().join("\n");
    open_form(app, |d| d.plan = Some((goal, next)));
}

/// Reassess from a card's balance.
pub(super) fn draft_balance(app: &mut WordApp, draft: &BalanceDraft) {
    let factors = draft.factors.iter().map(|f| (f.kind, f.favors, f.why.as_ref().map(|w| w.for_person().to_string()).unwrap_or_default())).collect();
    open_form(app, |d| d.reassess = Some(Reassess { stance: draft.stance, factors }));
}

fn run(app: &mut WordApp, id: &str, params: Value) -> bool {
    match app.run(id, params) {
        Ok(_) => true,
        Err(e) => {
            app.assist.notice = Some(e);
            false
        }
    }
}

/// A form's write, refused when the form was opened on a case other than the one now bound.
/// `true` when the form is done, saved or refused.
fn save(app: &mut WordApp, id: &str, params: Value) -> bool {
    let on = &app.assist.cases.drafts.on;
    if on.is_some() && *on != super::bound_for(app) {
        app.assist.notice = Some(tl!("This was written for another case, so it wasn't saved.").to_string());
        return true;
    }
    run(app, id, params)
}

/// The section. Only with a settings folder, which the caller checks.
pub(crate) fn section(app: &mut WordApp, ui: &mut Ui, t: &Tokens) {
    match super::open_case(app, false) {
        Some((at, case)) => bound(app, ui, t, &at, &case),
        None => unbound(app, ui, t),
    }
}

/// No case: the cases to bind to, by client, and New case.
fn unbound(app: &mut WordApp, ui: &mut Ui, t: &Tokens) {
    ui.label(RichText::new(tl!("No case")).font(semibold(12.0)));
    note(
        ui,
        "Bind this document to a case and each turn recalls the case's notes and the same client's confirmed ones. Nothing is recalled until you do.",
    );
    let listing = super::listing(app);
    match &listing {
        None => {
            ui.label(RichText::new(tl!("Reading your cases…")).small().color(t.text_dim));
        }
        Some(listing) if listing.walls.is_empty() => {
            ui.label(RichText::new(tl!("No cases yet.")).small().color(t.text_dim));
        }
        Some(listing) => case_list(app, ui, t, listing),
    }
    ui.add_space(4.0);
    new_case(app, ui, listing.as_ref());
}

fn case_list(app: &mut WordApp, ui: &mut Ui, t: &Tokens, listing: &Listing) {
    for wall in &listing.walls {
        ui.push_id(("wall", wall.id.as_str()), |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(wall.name.for_person()).small().color(t.text_dim));
                if wall.cases.is_empty() && ui.small_button(tl!("Delete client")).clicked() {
                    run(app, "assist.memory.wall.delete", json!({"wall": wall.id.as_str()}));
                }
            });
            for case in &wall.cases {
                let title = if case.title.for_person().is_empty() { tl!("Untitled case").to_string() } else { case.title.for_person().to_string() };
                let hover = fmt(tl!("Use {title} for this document"), &[("title", &title)]);
                if ui.push_id(case.id.as_str(), |ui| ui.small_button(&title).on_hover_text(hover).clicked()).inner {
                    run(app, "assist.memory.case.bind", json!({"wall": wall.id.as_str(), "case": case.id.as_str()}));
                }
                if case.damaged {
                    ui.label(RichText::new(tl!("Its file can't be read whole; your next change moves it aside.")).small().color(t.orange));
                }
            }
        });
    }
}

fn new_case(app: &mut WordApp, ui: &mut Ui, listing: Option<&Listing>) {
    ui.label(RichText::new(tl!("New case")).font(semibold(12.0)));
    let mut draft = std::mem::take(&mut app.assist.cases.drafts.new_case);
    line(ui, "Title", &mut draft.title, MAX_TEXT);
    ui.horizontal(|ui| {
        let name = ui.label(RichText::new(tl!("Client")).small());
        let walls = listing.map(|l| l.walls.as_slice()).unwrap_or_default();
        let shown = draft
            .wall
            .as_ref()
            .and_then(|id| walls.iter().find(|w| w.id == *id))
            .map_or_else(|| tl!("New client").to_string(), |w| w.name.for_person().to_string());
        let combo = craft_ui::ComboBox::from_id_salt("case_client").selected_text(shown).width(170.0).show_ui(ui, |ui| {
            if ui.selectable_label(draft.wall.is_none(), tl!("New client")).clicked() {
                draft.wall = None;
            }
            for wall in walls {
                if ui.selectable_label(draft.wall.as_ref() == Some(&wall.id), wall.name.for_person()).clicked() {
                    draft.wall = Some(wall.id.clone());
                }
            }
        });
        combo.response.labelled_by(name.id);
    });
    if draft.wall.is_none() {
        if draft.client.is_empty() {
            draft.client = super::MY_CASES.into();
        }
        line(ui, "Client name", &mut draft.client, MAX_TEXT);
    }
    let can = !draft.title.trim().is_empty();
    if ui.add_enabled(can, egui::Button::new(tl!("Create"))).clicked() {
        let params = match &draft.wall {
            Some(wall) => json!({"title": draft.title, "wall": wall.as_str()}),
            None => json!({"title": draft.title, "client": draft.client}),
        };
        if run(app, "assist.memory.case.new", params) {
            draft = NewCase::default();
        }
    }
    app.assist.cases.drafts.new_case = draft;
}

/// The bound case: its title and client, Unbind, then facts, balance, plan, notes and Delete.
fn bound(app: &mut WordApp, ui: &mut Ui, t: &Tokens, at: &CaseRef, case: &Case) {
    let client = super::listing(app).and_then(|l| l.walls.into_iter().find(|w| w.id == at.wall)).map(|w| w.name.for_person().to_string());
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(case.title.for_person()).font(semibold(12.0)));
        if let Some(client) = client {
            ui.label(RichText::new(client).small().color(t.text_dim));
        }
        if ui.small_button(tl!("Unbind")).on_hover_text(tl!("This document stops using the case; its conversation ends.")).clicked() {
            run(app, "assist.memory.case.unbind", json!({}));
        }
    });
    if case.damaged {
        ui.label(RichText::new(tl!("Its file can't be read whole; your next change moves it aside as case.json.damaged.")).small().color(t.orange));
    }
    ui.add_space(4.0);
    heading(ui, "Facts");
    facts(app, ui, &case.facts);
    heading(ui, "Balance");
    balance(app, ui, t, case);
    heading(ui, "Plan");
    plan(app, ui, t, case);
    heading(ui, "Notes");
    notes(app, ui, t, case);
    ui.add_space(6.0);
    delete(app, ui, t, case);
    let drafts = &mut app.assist.cases.drafts;
    if !drafts.any_form() {
        drafts.on = None;
    } else if drafts.on.is_none() {
        drafts.on = Some(at.clone());
    }
}

fn heading(ui: &mut Ui, title: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(tl!(title)).font(semibold(11.5)));
}

/// The pickers write as they change; the names and the law have Edit, Save and Cancel.
fn facts(app: &mut WordApp, ui: &mut Ui, current: &Facts) {
    let mut f = current.clone();
    let mut changed = false;
    if let Some(v) = picker(ui, "case_kind", "Contract", &KINDS, f.kind, true) {
        f.kind = v;
        changed = true;
    }
    if let Some(v) = picker(ui, "case_side", "Our side", &SIDES, f.side, true) {
        f.side = v;
        changed = true;
    }
    if let Some(v) = picker(ui, "case_paper", "Whose paper", &PAPERS, f.paper, true) {
        f.paper = v;
        changed = true;
    }
    if let Some(v) = picker(ui, "case_theirs", "Counterparty", &THEIRS, f.theirs, true) {
        f.theirs = v;
        changed = true;
    }
    if let Some(v) = picker(ui, "case_size", "Size", &SIZES, f.size, true) {
        f.size = v;
        changed = true;
    }
    if let Some(v) = picker(ui, "case_language", "Language", &LANGS, f.language, true) {
        f.language = v;
        changed = true;
    }
    if let Some(Some(v)) = picker(ui, "case_stage", "Stage", &STAGES, Some(f.stage), false) {
        f.stage = v;
        changed = true;
    }
    if changed {
        run(app, "assist.memory.case.facts", json!({"facts": f}));
    }
    facts_text(app, ui, current);
}

/// Governing law and the parties' names, as text.
fn facts_text(app: &mut WordApp, ui: &mut Ui, current: &Facts) {
    let law = current.law.map(String::from).unwrap_or_default();
    let parties: Vec<&str> = current.facts_parties();
    let Some(mut text) = app.assist.cases.drafts.facts.take() else {
        ui.horizontal_wrapped(|ui| {
            let law_shown = if law.is_empty() { tl!("Not set").to_string() } else { law.clone() };
            ui.label(RichText::new(fmt(tl!("Governing law: {law}"), &[("law", &law_shown)])).small());
            let names = if parties.is_empty() { tl!("none").to_string() } else { parties.join(", ") };
            ui.label(RichText::new(fmt(tl!("Parties: {names}"), &[("names", &names)])).small());
            if ui.small_button(tl!("Edit")).clicked() {
                app.assist.cases.drafts.facts = Some(FactsText { parties: parties.join("\n"), law });
            }
        });
        return;
    };
    line(ui, "Governing law (JP, US-NY)", &mut text.law, 6);
    lines(ui, "Parties, one per line", &mut text.parties, MAX_SHORT.saturating_mul(16));
    let mut open = true;
    ui.horizontal(|ui| {
        if ui.small_button(tl!("Save")).clicked() {
            let mut f = current.clone();
            let law = text.law.trim();
            f.law = if law.is_empty() { None } else { wordcraft_assist::cases::Law::parse(law) };
            if !law.is_empty() && f.law.is_none() {
                app.assist.notice = Some(tl!("Governing law is a country code such as JP, or a country and region such as US-NY.").to_string());
            } else {
                let names: Vec<&str> = text.parties.lines().map(str::trim).filter(|n| !n.is_empty()).collect();
                open = !save(app, "assist.memory.case.facts", json!({"facts": f.with_parties(&names)}));
            }
        }
        if ui.small_button(tl!("Cancel")).clicked() {
            open = false;
        }
    });
    if open {
        app.assist.cases.drafts.facts = Some(text);
    }
}

/// Ways to read and rewrite the parties without reaching into [`Quoted`].
trait FactsExt {
    fn facts_parties(&self) -> Vec<&str>;
    fn with_parties(&self, names: &[&str]) -> Value;
}

impl FactsExt for Facts {
    fn facts_parties(&self) -> Vec<&str> {
        self.parties.iter().map(|p| p.for_person()).collect()
    }

    fn with_parties(&self, names: &[&str]) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Some(o) = v.as_object_mut() {
            o.insert("parties".into(), json!(names));
        }
        v
    }
}

fn standing_words(standing: Standing) -> String {
    match standing {
        Standing::Confirmed { on } => fmt(tl!("{date}, yours"), &[("date", &String::from(on))]),
        Standing::Unconfirmed => tl!("unconfirmed").to_string(),
    }
}

/// The current stance with its date, Reassess, and the history.
fn balance(app: &mut WordApp, ui: &mut Ui, t: &Tokens, case: &Case) {
    match case.balance.last() {
        None => ui.label(RichText::new(tl!("Not assessed yet.")).small().color(t.text_dim)),
        Some(b) => {
            let says = fmt(tl!("{stance} ({standing})"), &[("stance", word(&STANCES, b.stance)), ("standing", &standing_words(b.standing))]);
            ui.label(RichText::new(says).small())
        }
    };
    if let Some(mut draft) = app.assist.cases.drafts.reassess.take() {
        if reassess(app, ui, &mut draft) {
            app.assist.cases.drafts.reassess = Some(draft);
        }
    } else if ui.small_button(tl!("Reassess")).clicked() {
        app.assist.cases.drafts.reassess = Some(Reassess { stance: case.stance_now().map_or(Stance::Even, |(s, _)| s), factors: Vec::new() });
    }
    if case.balance.len() > 1 {
        egui::CollapsingHeader::new(RichText::new(tl!("History")).small()).id_salt("case_balance_history").show(ui, |ui| {
            for b in case.balance.iter().rev() {
                let factors: Vec<String> = b.factors.iter().map(factor_words).collect();
                let line = fmt(
                    tl!("{date}: {stance}. {factors}"),
                    &[("date", &String::from(b.on)), ("stance", word(&STANCES, b.stance)), ("factors", &factors.join("; "))],
                );
                ui.add(egui::Label::new(RichText::new(line).small().color(t.text_dim)).wrap());
            }
        });
    }
    if let Some(b) = case.balance.last() {
        for f in &b.factors {
            ui.add(egui::Label::new(RichText::new(factor_words(f)).small().color(t.text_dim)).wrap());
        }
    }
}

fn factor_words(f: &wordcraft_assist::cases::Factor) -> String {
    let why = f.why.as_ref().map(|w| format!(": {}", w.for_person())).unwrap_or_default();
    fmt(tl!("{kind}, favours {side}{why}"), &[("kind", word(&FACTORS, f.kind)), ("side", word(&FAVORS, f.favors)), ("why", &why)])
}

/// The Reassess form: whether it stays open.
fn reassess(app: &mut WordApp, ui: &mut Ui, draft: &mut Reassess) -> bool {
    ui.horizontal(|ui| {
        for (stance, w) in STANCES {
            if ui.selectable_label(draft.stance == stance, tl!(w)).clicked() {
                draft.stance = stance;
            }
        }
    });
    let mut remove = None;
    for (n, (kind, favors, why)) in draft.factors.iter_mut().enumerate() {
        ui.push_id(("factor", n), |ui| {
            if let Some(Some(k)) = picker(ui, "factor_kind", "Factor", &FACTORS, Some(*kind), false) {
                *kind = k;
            }
            if let Some(Some(s)) = picker(ui, "factor_favors", "Favours", &FAVORS, Some(*favors), false) {
                *favors = s;
            }
            line(ui, "Why", why, MAX_SHORT);
            if ui.small_button(tl!("Remove this factor")).clicked() {
                remove = Some(n);
            }
        });
    }
    if let Some(n) = remove
        && n < draft.factors.len()
    {
        draft.factors.remove(n);
    }
    let mut open = true;
    ui.horizontal(|ui| {
        if draft.factors.len() < MAX_FACTORS && ui.small_button(tl!("Add a factor")).clicked() {
            draft.factors.push((FactorKind::Dependence, Side::Them, String::new()));
        }
        if ui.small_button(tl!("Save")).clicked() {
            let factors: Vec<Value> = draft
                .factors
                .iter()
                .map(|(k, s, why)| json!({"kind": k, "favors": s, "why": (!why.trim().is_empty()).then(|| why.trim())}))
                .collect();
            open = !save(app, "assist.memory.case.balance", json!({"stance": draft.stance, "factors": factors}));
        }
        if ui.small_button(tl!("Cancel")).clicked() {
            open = false;
        }
    });
    open
}

/// The plan: goal and next steps, with Edit, Save, Cancel and Remove.
fn plan(app: &mut WordApp, ui: &mut Ui, t: &Tokens, case: &Case) {
    if let Some((mut goal, mut steps)) = app.assist.cases.drafts.plan.take() {
        lines(ui, "Goal", &mut goal, MAX_TEXT);
        lines(ui, "Next steps, one per line", &mut steps, MAX_TEXT.saturating_mul(16));
        let mut open = true;
        ui.horizontal(|ui| {
            if ui.add_enabled(!goal.trim().is_empty(), egui::Button::new(tl!("Save")).small()).clicked() {
                let next: Vec<&str> = steps.lines().map(str::trim).filter(|s| !s.is_empty()).collect();
                open = !save(app, "assist.memory.case.plan", json!({"goal": goal.trim(), "next": next}));
            }
            if ui.small_button(tl!("Cancel")).clicked() {
                open = false;
            }
        });
        if open {
            app.assist.cases.drafts.plan = Some((goal, steps));
        }
        return;
    }
    match &case.plan {
        None => {
            ui.label(RichText::new(tl!("No plan yet.")).small().color(t.text_dim));
        }
        Some(p) => {
            ui.add(egui::Label::new(RichText::new(p.goal.for_person()).small()).wrap());
            for step in &p.next {
                ui.add(egui::Label::new(RichText::new(format!("• {}", step.for_person())).small()).wrap());
            }
            ui.label(RichText::new(standing_words(p.standing)).small().color(t.text_dim));
        }
    }
    ui.horizontal(|ui| {
        if ui.small_button(tl!("Edit plan")).clicked() {
            let (goal, steps) = case
                .plan
                .as_ref()
                .map(|p| (p.goal.for_person().to_string(), p.next.iter().map(|s| s.for_person()).collect::<Vec<_>>().join("\n")))
                .unwrap_or_default();
            app.assist.cases.drafts.plan = Some((goal, steps));
        }
        if case.plan.is_some() && ui.small_button(tl!("Remove plan")).clicked() {
            run(app, "assist.memory.case.plan", json!({"clear": true}));
        }
    });
}

/// Notes grouped by kind, each with Edit and Remove, and Confirm on an unconfirmed one; then Add
/// note, or the form.
fn notes(app: &mut WordApp, ui: &mut Ui, t: &Tokens, case: &Case) {
    if case.notes.is_empty() {
        ui.label(RichText::new(tl!("No notes yet.")).small().color(t.text_dim));
    }
    for (kind, w) in NOTE_KINDS {
        let of_kind: Vec<&Note> = case.notes.iter().filter(|n| n.kind() == kind).collect();
        if of_kind.is_empty() {
            continue;
        }
        ui.label(RichText::new(tl!(w)).small().color(t.text_dim));
        for n in of_kind {
            ui.push_id(("note", n.id.to_string()), |ui| note_row(app, ui, t, n));
        }
    }
    ui.add_space(2.0);
    if let Some(mut form) = app.assist.cases.drafts.note.take() {
        if note_form(app, ui, &mut form) {
            app.assist.cases.drafts.note = Some(form);
        }
    } else if ui.small_button(tl!("Add note")).clicked() {
        app.assist.cases.drafts.note = Some(NoteForm::empty());
    }
}

fn note_row(app: &mut WordApp, ui: &mut Ui, t: &Tokens, n: &Note) {
    let confirmed = matches!(n.standing, Standing::Confirmed { .. });
    let color = if confirmed { t.text } else { t.text_dim };
    ui.add(egui::Label::new(RichText::new(note_words(n)).small().color(color)).wrap());
    let id = n.id.to_string();
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(fmt(tl!("{id}, {date}"), &[("id", &id), ("date", &String::from(n.at))])).small().color(t.text_dim));
        if !confirmed {
            ui.label(RichText::new(tl!("unconfirmed")).small().color(t.orange));
            if ui.small_button(tl!("Confirm")).on_hover_text(tl!("This client's other cases can use it.")).clicked() {
                run(app, "assist.memory.case.noteConfirm", json!({"id": id}));
            }
        }
        if ui.small_button(tl!("Edit")).clicked() {
            app.assist.cases.drafts.note = Some(NoteForm::of(n));
        }
        if ui.small_button(tl!("Remove")).clicked() {
            run(app, "assist.memory.case.noteRemove", json!({"id": id}));
        }
    });
}

/// A note in one line of the person's words, with the typed parts named. The quiet row under an
/// answer shows a note with this too, so Confirm there approves what the page would show.
pub(super) fn note_words(n: &Note) -> String {
    let mut texts = n.body.texts().map(|q| q.for_person());
    let main = texts.next().unwrap_or_default().to_string();
    let more = texts.next().map(|s| format!(" ({s})")).unwrap_or_default();
    let typed = match &n.body {
        Body::Leverage { favors, until, .. } => {
            let until = until.map(|d| fmt(tl!(" until {date}"), &[("date", &String::from(d))])).unwrap_or_default();
            fmt(tl!(" favours {side}{until}"), &[("side", word(&FAVORS, *favors)), ("until", &until)])
        }
        Body::Position { topic, moved, landed, .. } => fmt(
            tl!(" {topic}, {side} moved, {landed}"),
            &[("topic", word(&TOPICS, *topic)), ("side", word(&FAVORS, *moved)), ("landed", if *landed { tl!("landed") } else { tl!("open") })],
        ),
        _ => String::new(),
    };
    let how = n.how.as_ref().map(|h| fmt(tl!(" How: {how}"), &[("how", h.for_person())])).unwrap_or_default();
    format!("{main}{more}{typed}{how}")
}

/// The note form: whether it stays open.
fn note_form(app: &mut WordApp, ui: &mut Ui, form: &mut NoteForm) -> bool {
    if form.id.is_none()
        && let Some(Some(k)) = picker(ui, "note_kind", "Kind", &NOTE_KINDS, Some(form.kind), false)
    {
        form.kind = k;
    }
    let (main, short) = match form.kind {
        Kind::Pattern | Kind::Lesson => ("Rule", Some("Traps")),
        Kind::Pitfall => ("Trap", Some("Guard")),
        Kind::Leverage => ("What", None),
        Kind::Position => ("Terms, verbatim", Some("Traded for")),
    };
    lines(ui, main, &mut form.main, MAX_TEXT);
    if let Some(short) = short {
        line(ui, short, &mut form.short, MAX_SHORT);
    }
    match form.kind {
        Kind::Leverage => {
            if let Some(Some(s)) = picker(ui, "note_favors", "Favours", &FAVORS, Some(form.favors), false) {
                form.favors = s;
            }
            line(ui, "Until (YYYY-MM-DD)", &mut form.until, 10);
        }
        Kind::Position => {
            if let Some(Some(topic)) = picker(ui, "note_topic", "Topic", &TOPICS, Some(form.topic), false) {
                form.topic = topic;
            }
            if let Some(Some(s)) = picker(ui, "note_moved", "Who moved", &FAVORS, Some(form.favors), false) {
                form.favors = s;
            }
            ui.checkbox(&mut form.landed, tl!("Landed"));
        }
        _ => {}
    }
    line(ui, "How you know", &mut form.how, MAX_SHORT);
    let mut open = true;
    ui.horizontal(|ui| {
        if ui.add_enabled(!form.main.trim().is_empty(), egui::Button::new(tl!("Save")).small()).clicked() {
            let id = if form.id.is_some() { "assist.memory.case.noteEdit" } else { "assist.memory.case.note" };
            open = !save(app, id, form.to_json());
        }
        if ui.small_button(tl!("Cancel")).clicked() {
            open = false;
        }
    });
    open
}

/// Delete case asks in place, and names what goes and what WordCraft can't reach.
fn delete(app: &mut WordApp, ui: &mut Ui, t: &Tokens, case: &Case) {
    if !app.assist.cases.drafts.deleting {
        if ui.small_button(tl!("Delete case…")).clicked() {
            app.assist.cases.drafts.deleting = true;
        }
        return;
    }
    egui::Frame::NONE.stroke(egui::Stroke::new(1.5, t.red)).corner_radius(6).inner_margin(6).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let count = case.notes.len().to_string();
        let says = fmt(tl!("Delete this case and its {count} notes? It can't be undone."), &[("count", &count)]);
        ui.add(egui::Label::new(RichText::new(says).small().color(t.text)).wrap());
        note(
            ui,
            "WordCraft can't reach what a turn already sent to a model provider, nor an agent's own record of a conversation in its own folder, nor a backup of your settings folder.",
        );
        ui.horizontal(|ui| {
            if ui.small_button(tl!("Delete")).clicked() {
                save(app, "assist.memory.case.delete", json!({}));
                app.assist.cases.drafts.deleting = false;
            }
            if ui.small_button(tl!("Cancel")).clicked() {
                app.assist.cases.drafts.deleting = false;
            }
        });
    });
}

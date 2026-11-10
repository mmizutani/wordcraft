//! The Assistant pane's Details: what the conversation is doing and what runs it, section by
//! section, all read from its transcript as it goes ([`wordcraft_assist::Details`]), the same
//! value `assist.details` answers. Each section folds. A row with nothing to say doesn't show.

use egui::{RichText, Ui};
use serde_json::json;
use wordcraft_assist::answer::opens;
use wordcraft_assist::{Citation, DetailStep, Details, OutputKind, OutputRow, OutputState, Sent, Source, SourceKind, StepState, Usage};
use wordcraft_engine::room::Role;

use super::assistant::{Mark, wrapped};
use super::{Tokens, WordApp, semibold};
use crate::assist::role_label;
use crate::file_dialogs::{AfterPick, FileDialogRequest};
use crate::i18n::fmt;

/// Details, in `height` points.
pub(super) fn details(app: &mut WordApp, ui: &mut Ui, t: &Tokens, height: f32) {
    let details = crate::assist::details_for(app);
    let area = egui::ScrollArea::vertical().id_salt("assistant_details").max_height(height).auto_shrink([false, false]);
    area.show(ui, |ui| {
        ui.set_width(ui.available_width());
        section(ui, "Steps", |ui| steps(ui, t, &details));
        section(ui, "Sources", |ui| sources(app, ui, t, &details));
        section(ui, "Output", |ui| output(app, ui, t, &details));
        section(ui, "Properties", |ui| properties(app, ui, t, &details));
        if crate::assist::desktop(app) {
            // A chip's click comes here.
            if std::mem::take(&mut app.assist.memory.show_list) {
                ui.scroll_to_cursor(Some(egui::Align::TOP));
            }
            section(ui, "Memory", |ui| memory(app, ui, t, &details));
            section(ui, "Recalled", |ui| crate::assist::cases::section(ui, t, &details));
            if app.services.settings_dir.is_some() {
                section(ui, "Case", |ui| crate::assist::cases::page::section(app, ui, t));
            }
        }
    });
}

/// A section that folds, open at first.
fn section(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    egui::CollapsingHeader::new(RichText::new(tl!(title)).font(semibold(12.5)))
        .id_salt(("assistant_details", title))
        .default_open(true)
        .show(ui, add);
}

/// The steps of each turn, under what the person asked: oldest first, as the conversation goes.
fn steps(ui: &mut Ui, t: &Tokens, details: &Details) {
    if details.turns.iter().all(|turn| turn.steps.is_empty()) {
        ui.label(RichText::new(tl!("No steps yet.")).small().color(t.text_dim));
        return;
    }
    for turn in details.turns.iter().filter(|turn| !turn.steps.is_empty()) {
        if let Some(asked) = &turn.asked {
            wrapped(ui, RichText::new(asked).small().color(t.text_dim));
        }
        for step in &turn.steps {
            row(ui, t, step);
        }
        ui.add_space(4.0);
    }
}

/// What each turn gave the model and what it read, newest last.
fn sources(app: &mut WordApp, ui: &mut Ui, t: &Tokens, details: &Details) {
    attached(app, ui, t);
    for (i, turn) in details.turns.iter().enumerate() {
        if turn.context.is_none() && turn.sources.is_empty() && turn.citations.is_empty() && turn.unfound == 0 {
            continue;
        }
        if let Some(asked) = &turn.asked {
            wrapped(ui, RichText::new(asked).small().color(t.text_dim));
        }
        if let Some(sent) = &turn.context {
            context_row(ui, t, sent);
        }
        for (n, source) in turn.sources.iter().enumerate() {
            ui.push_id((i, n), |ui| source_row(app, ui, t, source));
        }
        if turn.more_sources > 0 {
            let more = turn.more_sources.to_string();
            ui.label(RichText::new(fmt(tl!("and {count} more"), &[("count", &more)])).small().color(t.text_dim));
        }
        for (n, citation) in turn.citations.iter().enumerate() {
            ui.push_id((i, "cite", n), |ui| citation_row(app, ui, t, citation));
        }
        if turn.unfound > 0 {
            let count = turn.unfound.to_string();
            let says = if turn.unfound == 1 {
                tl!("One citation couldn't be found.").to_string()
            } else {
                fmt(tl!("{count} citations couldn't be found."), &[("count", &count)])
            };
            wrapped(ui, RichText::new(says).small().color(t.orange));
        }
        ui.add_space(4.0);
    }
}

/// The sources attached for the conversation, each with its name, its size and whether it was cut,
/// and the way to add one: the person's alone, as a program's click does nothing.
fn attached(app: &mut WordApp, ui: &mut Ui, t: &Tokens) {
    let rows: Vec<(String, String, bool)> = crate::assist::sources(app).iter().map(|s| (s.name.clone(), size(s.size), s.cut)).collect();
    for (n, (name, size, cut)) in rows.iter().enumerate() {
        ui.push_id(("source", n), |ui| {
            ui.scope(|ui| {
                ui.spacing_mut().interact_size.y = 0.0;
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(name).small().color(t.text));
                    ui.label(RichText::new(size).small().color(t.text_dim));
                    if *cut {
                        ui.label(RichText::new(tl!("cut to fit")).small().color(t.orange));
                    }
                    if ui.small_button(tl!("Remove")).on_hover_text(fmt(tl!("Stop giving {name} to the assistant"), &[("name", name)])).clicked() {
                        let _ = crate::assist::remove_source(app, n);
                    }
                });
            });
        });
    }
    let add = ui
        .small_button(tl!("Add a source…"))
        .on_hover_text(tl!("Give the assistant a file or another open document to refer to. Its text goes to the assistant's provider."));
    if add.clicked() {
        if app.input_from_control(ui.ctx()) {
            app.assist.notice = Some(tl!("Only you can add a source: click it yourself.").to_string());
        } else {
            app.assist.add_open = !app.assist.add_open;
        }
    }
    if !app.assist.add_open {
        return;
    }
    let active = app.session.document_id();
    let documents: Vec<_> = app.session.documents().into_iter().filter(|d| d.id != active).collect();
    if ui.small_button(tl!("A file…")).clicked() && !app.input_from_control(ui.ctx()) {
        app.assist.add_open = false;
        let _ = app.ask_file(FileDialogRequest::Open { purpose: "document".into() }, AfterPick::AssistantSource { document: active });
    }
    for document in documents {
        if ui.small_button(&document.title).clicked() && !app.input_from_control(ui.ctx()) {
            app.assist.add_open = false;
            if let Err(e) = app.run("assist.knowledge", json!({"document": document.id})) {
                app.assist.notice = Some(e.to_string());
            }
        }
    }
}

/// `bytes` for a person to read.
fn size(bytes: usize) -> String {
    if bytes < 1024 { format!("{bytes} B") } else { format!("{:.1} KiB", bytes as f64 / 1024.0) }
}

/// What an answer cites: the document's words select, and a source's say which source has them.
fn citation_row(app: &mut WordApp, ui: &mut Ui, t: &Tokens, citation: &Citation) {
    let words = format!("“{}”", citation.words);
    match &citation.source {
        None => {
            if ui.link(RichText::new(words).small()).on_hover_text(tl!("Select these words")).clicked() {
                super::answer::select_cited(app, &citation.words);
            }
        }
        Some(source) => wrapped(ui, RichText::new(fmt(tl!("{words} from {source}"), &[("words", &words), ("source", source)])).small().color(t.text)),
    }
}

/// What a turn gave the model, in one row, and a note when the selection was cut.
fn context_row(ui: &mut Ui, t: &Tokens, sent: &Sent) {
    let kib = |bytes: u32| format!("{:.1}", f64::from(bytes) / 1024.0);
    let selection = sent.selection_bytes > 0 || sent.selection_cut;
    let mut parts = vec![if selection { tl!("the selection").to_string() } else { tl!("where the caret is").to_string() }];
    let around = sent.before.saturating_add(sent.after);
    if around > 0 {
        parts.push(fmt(tl!("{count} paragraphs around it"), &[("count", &around.to_string())]));
    }
    if sent.headings > 0 {
        parts.push(tl!("the outline").to_string());
    }
    let line = fmt(
        tl!("Given: {parts}. {used} of {cap} KiB."),
        &[("parts", &parts.join(", ")), ("used", &kib(sent.bytes)), ("cap", &format!("{:.0}", f64::from(sent.cap) / 1024.0))],
    );
    wrapped(ui, RichText::new(line).small().color(t.text));
    if sent.selection_cut {
        wrapped(ui, RichText::new(tl!("The selection was cut to fit.")).small().color(t.orange));
    }
    if sent.feedback > 0 {
        let line = fmt(tl!("Given: what you did with its changes, {size}"), &[("size", &size(sent.feedback as usize))]);
        wrapped(ui, RichText::new(line).small().color(t.text));
    }
    for source in &sent.knowledge {
        let line = fmt(tl!("Given: {name}, {size}"), &[("name", &source.name), ("size", &size(source.bytes as usize))]);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(line).small().color(t.text));
            if source.cut {
                ui.label(RichText::new(tl!("cut to fit")).small().color(t.orange));
            }
        });
    }
}

/// One thing a turn read. A paragraph of the document goes to its place, a file says its path,
/// and an address opens only on the person's click.
fn source_row(app: &mut WordApp, ui: &mut Ui, t: &Tokens, source: &Source) {
    let words = RichText::new(&source.title).small();
    match (source.kind, source.target.as_deref()) {
        (SourceKind::Document, Some(block)) => {
            if ui.link(words).on_hover_text(tl!("Select the paragraph it read")).clicked() {
                let range = json!({"anchor": {"block": block, "off": 0}, "focus": {"block": block, "off": u32::MAX}});
                if app.run("select.range", range).is_err() {
                    app.assist.notice = Some(tl!("That paragraph isn't in the document any more.").to_string());
                }
            }
        }
        (SourceKind::Web, Some(address)) if opens(address) => super::answer::web_link(app, ui, words.color(ui.visuals().hyperlink_color), address),
        _ => wrapped(ui, words.color(t.text)),
    }
}

/// What each turn changed and commented on, newest first.
fn output(app: &mut WordApp, ui: &mut Ui, t: &Tokens, details: &Details) {
    let turns: Vec<_> = details.turns.iter().enumerate().rev().filter(|(_, turn)| !turn.output.is_empty()).collect();
    if turns.is_empty() {
        ui.label(RichText::new(tl!("No changes or comments yet.")).small().color(t.text_dim));
    }
    for (i, turn) in turns {
        if let Some(asked) = &turn.asked {
            wrapped(ui, RichText::new(asked).small().color(t.text_dim));
        }
        for (n, row) in turn.output.iter().enumerate().rev() {
            ui.push_id((i, n), |ui| output_row(app, ui, t, row));
        }
        ui.add_space(4.0);
    }
    if details.more_output > 0 {
        let more = details.more_output.to_string();
        ui.label(RichText::new(fmt(tl!("and {count} more changes: see the Review tab"), &[("count", &more)])).small().color(t.text_dim));
    }
}

/// A change or a comment: its kind and words, then how it ended, or Show, Accept and Reject.
fn output_row(app: &mut WordApp, ui: &mut Ui, t: &Tokens, row: &OutputRow) {
    let kind = match row.kind {
        OutputKind::Insert => tl!("Added"),
        OutputKind::Delete => tl!("Removed"),
        OutputKind::Format => tl!("Formatted"),
        OutputKind::Move => tl!("Moved"),
        OutputKind::Comment => tl!("Comment"),
    };
    let state = match row.state {
        OutputState::Open => None,
        OutputState::Accepted => Some(tl!("Accepted")),
        OutputState::Rejected => Some(tl!("Rejected")),
        OutputState::Resolved => Some(tl!("Resolved")),
        OutputState::Deleted => Some(tl!("Deleted")),
    };
    ui.scope(|ui| {
        // Rows of text, not of controls: a wrapped row takes its height from this as it starts.
        ui.spacing_mut().interact_size.y = 0.0;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(kind).small().color(t.text_dim));
            ui.add(egui::Label::new(RichText::new(&row.text).small().color(t.text)).wrap());
            if let Some(state) = state {
                ui.label(RichText::new(state).small().color(t.text_dim));
            }
        });
    });
    let open = row.state == OutputState::Open;
    let there = open || (row.kind == OutputKind::Comment && row.state == OutputState::Resolved);
    let Some(range) = row.range().filter(|_| there) else { return };
    let name = app.conversation().map(|a| a.name().to_string()).unwrap_or_default();
    ui.horizontal_wrapped(|ui| {
        if ui.small_button(tl!("Show")).clicked() {
            show(app, &range, row.kind != OutputKind::Comment);
        }
        if open && row.kind != OutputKind::Comment {
            let can = crate::widgets::enabled(app, "review.accept");
            for (label, id) in [("Accept", "review.accept"), ("Reject", "review.reject")] {
                let button = ui.add_enabled(can, egui::Button::new(RichText::new(tl!(label)).small()));
                // The assistant's change alone, though someone else's overlaps it.
                if button.clicked() && select(app, &range) {
                    let _ = app.run(id, row.resolve_params(&name));
                }
            }
        }
        if row.quotes {
            ui.label(RichText::new(tl!("This quotes your case notes")).small().color(t.orange));
        }
    });
}

/// Select what a row names; it says so when that has gone.
fn select(app: &mut WordApp, range: &serde_json::Value) -> bool {
    let selected = app.run("select.range", range.clone()).is_ok();
    if !selected {
        app.assist.notice = Some(tl!("That isn't in the document any more.").to_string());
    }
    selected
}

/// Select what a row names, with Review open for a change.
fn show(app: &mut WordApp, range: &serde_json::Value, review: bool) {
    if select(app, range) && review {
        let _ = app.run("ui.tab", json!({"tab": "Review"}));
    }
}

/// What each turn sent of what the person kept, newest first, and the list itself.
fn memory(app: &mut WordApp, ui: &mut Ui, t: &Tokens, details: &Details) {
    let turns =
        details.turns.iter().rev().filter_map(|turn| Some((turn, turn.context.as_ref().filter(|s| !s.memory.is_empty() || s.memory_left_out > 0)?)));
    for (n, (turn, sent)) in turns.enumerate() {
        if let Some(asked) = &turn.asked {
            wrapped(ui, RichText::new(asked).small().color(t.text_dim));
        }
        ui.push_id(("memory turn", n), |ui| crate::assist::memory::turn(ui, t, sent, &turn.used, n == 0));
        ui.add_space(4.0);
    }
    crate::assist::memory::page(app, ui, t);
}

fn row(ui: &mut Ui, t: &Tokens, step: &DetailStep) {
    ui.horizontal_top(|ui| {
        let mark = match step.state {
            StepState::Waiting => Mark::Waiting,
            StepState::Running => Mark::Running,
            StepState::Done => Mark::Done,
            StepState::Failed => Mark::Failed,
        };
        mark.paint(ui, t);
        let color = if step.state == StepState::Done { t.text_dim } else { t.text };
        wrapped(ui, RichText::new(&step.title).color(color));
    });
}

/// What runs the conversation.
fn properties(app: &mut WordApp, ui: &mut Ui, t: &Tokens, details: &Details) {
    if let Some(p) = &details.properties {
        property(ui, t, tl!("Assistant"), details.agent.as_deref().unwrap_or(&p.backend));
        if !p.document.is_empty() {
            property(ui, t, tl!("Document"), &p.document);
        }
        if let Some(role) = Role::named(&p.role) {
            property(ui, t, tl!("Role"), tl!(role_label(role)));
        }
        property(ui, t, tl!("Edits"), if p.tracked { tl!("Tracked, in its name") } else { tl!("Not tracked") });
    }
    if let Some(model) = &details.model {
        property(ui, t, tl!("Model"), model);
    }
    if let Some(effort) = details.effort.as_deref().map(crate::assist::effort_word) {
        let shown = if details.effort_asked { fmt(tl!("{effort} (requested)"), &[("effort", effort)]) } else { effort.to_string() };
        property(ui, t, tl!("Effort"), &shown);
    }
    if let Some(usage) = &details.usage {
        self::usage(ui, t, usage);
    }
    // ChatGPT's plan has no price per request: what the turn cost is on the plan's own page.
    #[cfg(not(target_arch = "wasm32"))]
    if app.conversation().is_some_and(|a| a.info().id == wordcraft_assist::chatgpt::ID) {
        ui.hyperlink_to(RichText::new(tl!("See your plan's usage")).small(), wordcraft_assist::chatgpt::USAGE_PAGE);
    }
    #[cfg(target_arch = "wasm32")]
    let _ = app;
}

fn usage(ui: &mut Ui, t: &Tokens, usage: &Usage) {
    let count = |n: Option<u64>| n.map(|n| n.to_string());
    if let (Some(input), Some(output)) = (count(usage.input_tokens), count(usage.output_tokens)) {
        property(ui, t, tl!("Tokens"), &fmt(tl!("{input} in, {output} out"), &[("input", &input), ("output", &output)]));
    }
    if let Some(reasoning) = count(usage.reasoning_tokens) {
        property(ui, t, tl!("Reasoning tokens"), &reasoning);
    }
    match (count(usage.context_used), count(usage.context_size)) {
        (Some(used), Some(size)) => property(ui, t, tl!("Context"), &fmt(tl!("{used} of {size} tokens"), &[("used", &used), ("size", &size)])),
        (Some(used), None) => property(ui, t, tl!("Context"), &fmt(tl!("{used} tokens"), &[("used", &used)])),
        _ => {}
    }
    if let Some(cost) = &usage.cost {
        property(ui, t, tl!("Cost so far"), cost);
    }
}

/// A row of Properties, its value named by its label for a screen reader.
fn property(ui: &mut Ui, t: &Tokens, name: &str, value: &str) {
    ui.scope(|ui| {
        // Rows of text, not of controls: a wrapped row takes its height from this as it starts.
        ui.spacing_mut().interact_size.y = 0.0;
        ui.horizontal_wrapped(|ui| {
            let label = ui.label(RichText::new(name).small().color(t.text_dim));
            ui.add(egui::Label::new(RichText::new(value).small().color(t.text)).wrap()).labelled_by(label.id);
        });
    });
}

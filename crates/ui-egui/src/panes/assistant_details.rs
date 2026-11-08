//! The Assistant pane's Details: what the conversation is doing and what runs it, section by
//! section, all read from its transcript as it goes ([`wordcraft_assist::Details`]), the same
//! value `assist.details` answers. Each section folds. A row with nothing to say doesn't show.

use egui::{RichText, Ui};
use serde_json::json;
use wordcraft_assist::answer::opens;
use wordcraft_assist::{DetailStep, Details, OutputKind, OutputRow, OutputState, Sent, Source, SourceKind, StepState, Usage};
use wordcraft_engine::room::Role;

use super::assistant::{Mark, wrapped};
use super::{Tokens, WordApp, semibold};
use crate::assist::role_label;
use crate::i18n::fmt;

/// Details, in `height` points.
pub(super) fn details(app: &mut WordApp, ui: &mut Ui, t: &Tokens, height: f32) {
    let details = app.conversation().map(wordcraft_assist::Assistant::details).unwrap_or_default();
    let area = egui::ScrollArea::vertical().id_salt("assistant_details").max_height(height).auto_shrink([false, false]);
    area.show(ui, |ui| {
        ui.set_width(ui.available_width());
        section(ui, "Steps", |ui| steps(ui, t, &details));
        section(ui, "Sources", |ui| sources(app, ui, t, &details));
        section(ui, "Output", |ui| output(app, ui, t, &details));
        section(ui, "Properties", |ui| properties(app, ui, t, &details));
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
    for (i, turn) in details.turns.iter().enumerate() {
        if turn.context.is_none() && turn.sources.is_empty() {
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
        ui.add_space(4.0);
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
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(kind).small().color(t.text_dim));
        ui.add(egui::Label::new(RichText::new(&row.text).small().color(t.text)).wrap());
    });
    let state = match row.state {
        OutputState::Open => None,
        OutputState::Accepted => Some(tl!("Accepted")),
        OutputState::Rejected => Some(tl!("Rejected")),
        OutputState::Resolved => Some(tl!("Resolved")),
        OutputState::Deleted => Some(tl!("Deleted")),
    };
    ui.horizontal_wrapped(|ui| {
        if let Some(state) = state {
            ui.label(RichText::new(state).small().color(t.text_dim));
        }
        let open = row.state == OutputState::Open;
        let there = open || (row.kind == OutputKind::Comment && row.state == OutputState::Resolved);
        if let Some(range) = row.range().filter(|_| there) {
            if ui.small_button(tl!("Show")).clicked() && app.run("select.range", range.clone()).is_ok() {
                let tab = if row.kind == OutputKind::Comment { None } else { Some(json!({"tab": "Review"})) };
                if let Some(tab) = tab {
                    let _ = app.run("ui.tab", tab);
                }
            }
            if open && row.kind != OutputKind::Comment {
                let can = crate::widgets::enabled(app, "review.accept");
                for (label, id) in [("Accept", "review.accept"), ("Reject", "review.reject")] {
                    if ui.add_enabled(can, egui::Button::new(RichText::new(tl!(label)).small())).clicked()
                        && app.run("select.range", range.clone()).is_ok()
                    {
                        let _ = app.run(id, json!({}));
                    }
                }
            }
        }
    });
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

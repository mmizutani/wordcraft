//! The Assistant pane's Details: what the conversation is doing and what runs it, section by
//! section, all read from its transcript as it goes ([`wordcraft_assist::Details`]), the same
//! value `assist.details` answers. Each section folds. A row with nothing to say doesn't show.

use egui::{RichText, Ui};
use wordcraft_assist::{DetailStep, Details, StepState, Usage};
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
    ui.horizontal_wrapped(|ui| {
        // Rows of text, not of controls.
        ui.spacing_mut().interact_size.y = 0.0;
        let label = ui.label(RichText::new(name).small().color(t.text_dim));
        ui.add(egui::Label::new(RichText::new(value).small().color(t.text)).wrap()).labelled_by(label.id);
    });
}

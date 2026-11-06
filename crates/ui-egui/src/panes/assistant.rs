//! The Assistant pane (#15, #17): a conversation with the built-in assistant.
//!
//! At the top, the assistant new conversations use (whether it is ready, and if not what to do)
//! and who it is in the document: its name and role, which the person can change for the
//! conversation. Then the conversation: the prompts, the answers, thoughts folded away, a row per
//! tool call with how it went, the plan, permission requests with their options while they wait,
//! errors, and why a turn stopped. At the bottom, Accept all and Reject all of this assistant's
//! changes, and the prompt box with Send and Stop. Everything it does goes through the `assist.*`
//! and `review.*` commands.

use egui::{Color32, RichText, Stroke, Ui, vec2};
use serde_json::json;
use wordcraft_assist::{Answer, Entry, PlanStatus, Readiness, ToolStatus};
use wordcraft_engine::AssistantPrefs;
use wordcraft_engine::room::Role;

use super::{Tokens, WordApp, regular, semibold};
use crate::assist::{chosen, role_label, role_note};
use crate::i18n::fmt;

/// Rows the prompt box shows.
const PROMPT_ROWS: usize = 3;
/// The height of the buttons and the prompt box under the conversation before they measured.
const BOTTOM: f32 = 110.0;
/// The least height the conversation gets, however short the pane.
const MIN_TRANSCRIPT: f32 = 60.0;

pub(super) fn assistant(app: &mut WordApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    picker(app, ui, &t);
    ui.add_space(4.0);
    who(app, ui, &t);
    if app.has_document() && app.shared_room().is_some() && !app.shows_shared_document() {
        ui.label(
            RichText::new(tl!("Another document is shared. Switching ends its conversation and disconnects its participants."))
                .small()
                .color(t.orange),
        );
        if ui.button(tl!("Use this document")).clicked() {
            let _ = app.run("assist.useDocument", json!({}));
        }
    }
    ui.separator();
    // The conversation takes the height that the buttons and the prompt box under it leave, as
    // they measured on the last frame.
    let bottom_id = ui.scope_id().with("assistant_bottom");
    let bottom = ui.data(|d| d.get_temp::<f32>(bottom_id)).unwrap_or(BOTTOM);
    let height = (ui.available_height() - bottom - 3.0 * ui.spacing().item_spacing.y - 6.0).max(MIN_TRANSCRIPT);
    transcript(app, ui, &t, height);
    ui.separator();
    let bottom = ui
        .vertical(|ui| {
            review(app, ui);
            ui.add_space(4.0);
            prompt(app, ui, &t);
        })
        .response
        .rect
        .height();
    ui.data_mut(|d| d.insert_temp(bottom_id, bottom));
}

/// The assistant new conversations use, from those offered, and what it needs before it can.
fn picker(app: &mut WordApp, ui: &mut Ui, t: &Tokens) {
    let offers = app.assist.offers.clone();
    let current = chosen(app).cloned();
    if offers.is_empty() {
        ui.label(RichText::new(tl!("No assistant is available yet.")).font(semibold(12.5)));
        ui.label(RichText::new(tl!("This version of WordCraft offers no assistant to sign in to or set up.")).small().color(t.text_dim));
        return;
    }
    ui.horizontal(|ui| {
        let label = ui.label(tl!("Assistant:"));
        let shown = current.as_ref().map(|o| o.label.clone()).unwrap_or_default();
        let combo =
            craft_ui::ComboBox::from_id_salt("assistant_backend").selected_text(shown).width(ui.available_width().max(80.0)).show_ui(ui, |ui| {
                for o in &offers {
                    let ready = o.readiness == Readiness::Ready;
                    let text = if ready {
                        o.label.clone()
                    } else {
                        fmt(tl!("{assistant} ({state})"), &[("assistant", &o.label), ("state", tl!(readiness_word(&o.readiness)))])
                    };
                    if ui.selectable_label(current.as_ref().is_some_and(|c| c.id == o.id), text).clicked() {
                        let _ = app.run("assist.backend", json!({"id": o.id}));
                    }
                }
            });
        combo.response.labelled_by(label.id);
    });
    if let Some(o) = &current {
        let text = match &o.readiness {
            Readiness::Ready => return,
            Readiness::SignIn { how } => fmt(tl!("Sign in: {how}"), &[("how", how)]),
            Readiness::Install { how } => fmt(tl!("Install: {how}"), &[("how", how)]),
            Readiness::Unavailable { why } => fmt(tl!("Unavailable: {why}"), &[("why", why)]),
        };
        wrapped(ui, RichText::new(text).small().color(t.orange));
    }
}

fn readiness_word(r: &Readiness) -> &'static str {
    match r {
        Readiness::Ready => "ready",
        Readiness::SignIn { .. } => "sign in",
        Readiness::Install { .. } => "install",
        Readiness::Unavailable { .. } => "unavailable",
    }
}

/// Who the assistant is in the document, and the role for this conversation.
fn who(app: &mut WordApp, ui: &mut Ui, t: &Tokens) {
    let live = app.conversation().filter(|a| !a.has_ended()).map(|a| (a.name().to_string(), a.role()));
    let setting = app.session.prefs.assistant.default_role();
    let role = app.assist.role.or(live.as_ref().map(|(_, r)| *r)).unwrap_or(setting);
    if let Some((name, now)) = &live {
        let line = fmt(tl!("{name} · {role}"), &[("name", name), ("role", tl!(role_label(*now)))]);
        ui.label(RichText::new(line).font(semibold(13.0)));
    }
    ui.horizontal(|ui| {
        let label = ui.label(tl!("Role:"));
        let combo = craft_ui::ComboBox::from_id_salt("assistant_role").selected_text(tl!(role_label(role))).width(140.0).show_ui(ui, |ui| {
            for r in AssistantPrefs::ROLES {
                if ui.selectable_label(r == role, tl!(role_label(r))).clicked() {
                    app.assist.role = Some(r);
                }
            }
        });
        combo.response.labelled_by(label.id);
    });
    let color = if role == Role::Editor { t.orange } else { t.text_dim };
    ui.label(RichText::new(tl!(role_note(role))).small().color(color));
    if let Some((_, now)) = &live
        && *now != role
    {
        ui.label(RichText::new(tl!("Your next prompt starts a new conversation with this role.")).small().color(t.text_dim));
    }
}

/// The conversation so far.
fn transcript(app: &mut WordApp, ui: &mut Ui, t: &Tokens, height: f32) {
    let entries: Vec<Entry> = app.conversation().map(|a| a.transcript().entries().cloned().collect()).unwrap_or_default();
    let waiting = app.conversation().and_then(|a| a.pending_permission()).map(|r| r.id);
    let busy = app.conversation().is_some_and(|a| a.is_busy());
    let ended = app.conversation().is_some_and(|a| a.has_ended());
    let area = egui::ScrollArea::vertical().id_salt("assistant_transcript").max_height(height).auto_shrink([false, false]).stick_to_bottom(true);
    area.show(ui, |ui| {
        ui.set_width(ui.available_width());
        if entries.is_empty() {
            ui.label(RichText::new(tl!("Ask the assistant to draft, rewrite or check your writing. It works in the document like a collaborator: with the Suggester role, every edit is a tracked change for you to accept or reject.")).color(t.text_dim));
        }
        for (i, e) in entries.iter().enumerate() {
            entry(app, ui, t, i, e, waiting);
            ui.add_space(6.0);
        }
        if busy {
            ui.label(RichText::new(tl!("Working…")).small().color(t.text_dim));
        } else if ended && !entries.is_empty() {
            ui.label(RichText::new(tl!("This conversation has ended. Your next prompt starts a new one.")).small().color(t.text_dim));
        }
        if let Some(n) = app.assist.notice.clone() {
            ui.label(RichText::new(n).color(t.red));
        }
    });
}

fn wrapped(ui: &mut Ui, text: RichText) {
    ui.add(egui::Label::new(text).wrap());
}

fn entry(app: &mut WordApp, ui: &mut Ui, t: &Tokens, i: usize, e: &Entry, waiting: Option<u64>) {
    match e {
        Entry::Asked { text } => {
            egui::Frame::NONE.fill(t.checked).corner_radius(6).inner_margin(6).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(tl!("You")).font(semibold(11.5)).color(t.text_dim));
                wrapped(ui, RichText::new(text).color(t.text));
            });
        }
        Entry::Answer { text } => wrapped(ui, RichText::new(text).color(t.text)),
        Entry::Thought { text } => {
            egui::CollapsingHeader::new(RichText::new(tl!("Thinking")).small().color(t.text_dim))
                .id_salt(("assistant_thought", i))
                .default_open(false)
                .show(ui, |ui| wrapped(ui, RichText::new(text).small().color(t.text_dim)));
        }
        Entry::Tool(call) => {
            ui.horizontal_top(|ui| {
                let mark = match call.status {
                    ToolStatus::Pending => Mark::Waiting,
                    ToolStatus::InProgress => Mark::Running,
                    ToolStatus::Completed => Mark::Done,
                    ToolStatus::Failed => Mark::Failed,
                };
                mark.paint(ui, t);
                ui.vertical(|ui| {
                    wrapped(ui, RichText::new(&call.title).font(regular(12.0)).color(t.text));
                    // Why a call failed, or what it said, unless that is data for the model (the
                    // start of a JSON result says nothing in a compact log).
                    let failed = call.status == ToolStatus::Failed;
                    if !call.detail.is_empty() && (failed || !call.detail.trim_start().starts_with(['{', '['])) {
                        wrapped(ui, RichText::new(&call.detail).small().color(if failed { t.red } else { t.text_dim }));
                    }
                });
            });
        }
        Entry::Plan { entries } => {
            ui.label(RichText::new(tl!("Plan")).font(semibold(12.0)));
            for p in entries {
                ui.horizontal_top(|ui| {
                    let mark = match p.status {
                        PlanStatus::Pending => Mark::Waiting,
                        PlanStatus::InProgress => Mark::Running,
                        PlanStatus::Completed => Mark::Done,
                    };
                    mark.paint(ui, t);
                    wrapped(ui, RichText::new(&p.content).color(t.text));
                });
            }
        }
        Entry::Permission { request, answer } => {
            egui::Frame::NONE.stroke(Stroke::new(1.0, t.border_strong)).corner_radius(6).inner_margin(6).show(ui, |ui| {
                ui.set_width(ui.available_width());
                wrapped(ui, RichText::new(&request.title).font(semibold(12.0)).color(t.text));
                match answer {
                    None if waiting == Some(request.id) => {
                        ui.horizontal_wrapped(|ui| {
                            for o in &request.options {
                                if ui.button(&o.name).clicked()
                                    && let Some(a) = app.conversation_mut()
                                {
                                    a.answer(request.id, Answer::Selected { option: o.id.clone() });
                                }
                            }
                        });
                    }
                    None => wrapped(ui, RichText::new(tl!("No answer")).small().color(t.text_dim)),
                    Some(Answer::Selected { option }) => {
                        let name = request.options.iter().find(|o| &o.id == option).map_or(option.as_str(), |o| o.name.as_str());
                        wrapped(ui, RichText::new(fmt(tl!("Answered: {option}"), &[("option", name)])).small().color(t.text_dim));
                    }
                    Some(Answer::Cancelled) => wrapped(ui, RichText::new(tl!("No answer")).small().color(t.text_dim)),
                }
            });
        }
        Entry::Model { name } => {
            ui.label(RichText::new(fmt(tl!("Model: {name}"), &[("name", name)])).small().color(t.text_dim));
        }
        Entry::Stopped { stop } => {
            if let Some(says) = stop.says() {
                ui.label(RichText::new(tl!(says)).small().italics().color(t.text_dim));
            }
        }
        // A failed turn says why; the turn itself ends with its own event.
        Entry::Error { message } => wrapped(ui, RichText::new(message).color(t.red)),
    }
}

/// How a tool call or a plan entry stands, drawn: an open ring (waiting), a dot (running), a
/// tick (done) or a cross (failed).
#[derive(Clone, Copy)]
enum Mark {
    Waiting,
    Running,
    Done,
    Failed,
}

impl Mark {
    fn paint(self, ui: &mut Ui, t: &Tokens) {
        let (r, _) = ui.allocate_exact_size(vec2(14.0, 16.0), egui::Sense::hover());
        let c = r.center();
        let p = ui.painter();
        let line = |color: Color32| Stroke::new(1.5, color);
        match self {
            Mark::Waiting => {
                p.circle_stroke(c, 4.0, line(t.text_dim));
            }
            Mark::Running => {
                p.circle_filled(c, 3.5, t.accent);
            }
            Mark::Done => {
                p.line_segment([c + vec2(-4.0, 0.0), c + vec2(-1.0, 3.0)], line(t.green));
                p.line_segment([c + vec2(-1.0, 3.0), c + vec2(4.5, -3.5)], line(t.green));
            }
            Mark::Failed => {
                p.line_segment([c + vec2(-3.5, -3.5), c + vec2(3.5, 3.5)], line(t.red));
                p.line_segment([c + vec2(-3.5, 3.5), c + vec2(3.5, -3.5)], line(t.red));
            }
        }
    }
}

/// How many tracked changes `name` has in the document, counted again only when it changed.
fn changes_by(app: &mut WordApp, name: &str) -> usize {
    let key = (app.session.document_id(), app.session.rev(), name.to_string());
    if let Some((k, n)) = &app.assist.changes_by
        && *k == key
    {
        return *n;
    }
    let n = wordcraft_engine::cmd::review::changes_by(&app.session, name);
    app.assist.changes_by = Some((key, n));
    n
}

/// Accept or reject every change the assistant made, and the way to the Review tab for the rest.
fn review(app: &mut WordApp, ui: &mut Ui) {
    let Some(name) = app.conversation().map(|a| a.name().to_string()) else { return };
    let n = changes_by(app, &name);
    ui.horizontal(|ui| {
        let accept = ui
            .add_enabled(n > 0, egui::Button::new(tl!("Accept all")))
            .on_hover_text(fmt(tl!("Accept every change {name} made"), &[("name", &name)]));
        if accept.clicked() {
            let _ = app.run("review.acceptAll", json!({"author": name}));
        }
        let reject = ui
            .add_enabled(n > 0, egui::Button::new(tl!("Reject all")))
            .on_hover_text(fmt(tl!("Reject every change {name} made"), &[("name", &name)]));
        if reject.clicked() {
            let _ = app.run("review.rejectAll", json!({"author": name}));
        }
    });
    // On a line of its own: the pane is narrow, and a link cut in two reads badly.
    if ui.link(tl!("Review changes one by one")).clicked() {
        let _ = app.run("ui.tab", json!({"tab": "Review"}));
    }
}

/// The prompt box, with Send (Enter) and Stop; Shift+Enter starts a new line.
fn prompt(app: &mut WordApp, ui: &mut Ui, t: &Tokens) {
    let busy = app.conversation().is_some_and(|a| a.is_busy());
    let hint = match app.conversation().filter(|a| !a.has_ended()) {
        Some(a) => fmt(tl!("Ask {name}…"), &[("name", a.name())]),
        None => tl!("Ask the assistant…").to_string(),
    };
    let id = app.window_id("assistant_prompt");
    let mut text = std::mem::take(&mut app.assist.prompt);
    let r = ui.add(
        egui::TextEdit::multiline(&mut text)
            .id(id)
            .hint_text(hint)
            .desired_rows(PROMPT_ROWS)
            .desired_width(f32::INFINITY)
            .return_key(Some(egui::KeyboardShortcut::new(egui::Modifiers::SHIFT, egui::Key::Enter))),
    );
    app.assist.prompt = text;
    if app.assist.focus_prompt > 0 {
        app.assist.focus_prompt -= 1;
        r.request_focus();
        // The page doesn't take the keyboard back (a click on the ribbon hands it there).
        app.canvas.want_focus = false;
    }
    let enter = r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.is_none());
    let can_send = !busy && !app.assist.prompt.trim().is_empty() && crate::widgets::enabled(app, "assist.ask");
    ui.horizontal(|ui| {
        let send = ui.add_enabled(can_send, egui::Button::new(tl!("Send")));
        let send = match app.session.why_disabled("assist.ask") {
            Some(why) if !busy => send.on_disabled_hover_text(tl!(why)),
            _ => send,
        };
        if (send.clicked() || (enter && can_send)) && can_send {
            let prompt = app.assist.prompt.clone();
            let _ = app.run("assist.ask", json!({"prompt": prompt}));
            app.assist.focus_prompt = crate::assist::FOCUS_FRAMES;
        }
        if ui.add_enabled(busy, egui::Button::new(tl!("Stop"))).clicked() {
            let _ = app.run("assist.cancel", json!({}));
        }
        if !busy && let Some(a) = app.conversation().filter(|a| !a.has_ended() && !a.tracked() && a.role() >= Role::Editor) {
            ui.label(RichText::new(fmt(tl!("{name}'s edits aren't tracked"), &[("name", a.name())])).small().color(t.orange));
        }
    });
}

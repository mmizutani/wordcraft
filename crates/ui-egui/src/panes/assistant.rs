//! The Assistant pane (#15, #17): a conversation with the built-in assistant.
//!
//! At the top, the assistant new conversations use (whether it is ready, and if not what to do:
//! for ChatGPT, Continue with ChatGPT, #19) and who it is in the document: its name and role, which the person can change for the
//! conversation. Then the conversation: the prompts, the answers, thoughts folded away, a row per
//! tool call folded to how it went (open: what it said, and the way to the change it made, #23),
//! the plan as a checklist, permission requests with their options while they wait, errors, and
//! why a turn stopped, said plainly. At the bottom, Accept all and Reject all of this assistant's
//! changes, the writing actions (#18), and the prompt box with Send and Stop; Draft turns the box
//! into what to write at the caret. Everything it does goes through the `assist.*` and `review.*`
//! commands, but for the answer to a permission request: that is the person's click alone, which
//! no script can give for them.

use egui::{Color32, RichText, Stroke, Ui, vec2};
use serde_json::json;
use wordcraft_assist::{Answer, Entry, PermissionRequest, PlanStatus, Readiness, StopReason, ToolCall, ToolStatus};
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
    // The web app runs no assistant (#26): there is no role to choose for one.
    if crate::assist::desktop(app) || !app.assist.offers.is_empty() {
        who(app, ui, &t);
    }
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
            actions(app, ui);
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
        if crate::assist::desktop(app) {
            ui.label(RichText::new(tl!("No assistant is available yet.")).font(semibold(12.5)));
            ui.label(RichText::new(tl!("This version of WordCraft offers no assistant to sign in to or set up.")).small().color(t.text_dim));
        } else {
            // The web app (#26): its backends would sign in or start agents.
            ui.label(RichText::new(tl!("The assistant needs the desktop app")).font(semibold(12.5)));
            wrapped(
                ui,
                RichText::new(tl!(
                    "Signing in to ChatGPT and starting an agent such as Claude Code or Codex take WordCraft's desktop app: a web page can't keep a sign-in safe or start a program."
                ))
                .small()
                .color(t.text_dim),
            );
        }
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
            // A backend's sign-in hint already says what to do ("Sign in with …"), with the
            // button for it right under it for ChatGPT (#19): no "Sign in:" before it.
            Readiness::SignIn { how } => how.clone(),
            Readiness::Install { how } => fmt(tl!("Install: {how}"), &[("how", how)]),
            Readiness::Unavailable { why } => fmt(tl!("Unavailable: {why}"), &[("why", why)]),
        };
        wrapped(ui, RichText::new(text).small().color(t.orange));
        // ChatGPT signs in from here too, without a trip to File › Options › Account (#19).
        #[cfg(not(target_arch = "wasm32"))]
        if o.id == wordcraft_assist::chatgpt::ID
            && matches!(o.readiness, Readiness::SignIn { .. })
            && crate::assist::desktop(app)
            && app.services.settings_dir.is_some()
        {
            crate::assist::chatgpt::controls(app, ui, true);
        }
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
        Entry::Tool(call) => tool_row(app, ui, t, i, call),
        Entry::Plan { entries } => {
            // A checklist: what is done, under way and to come.
            let done = entries.iter().filter(|p| p.status == PlanStatus::Completed).count();
            ui.horizontal(|ui| {
                ui.label(RichText::new(tl!("Plan")).font(semibold(12.0)));
                let count = fmt(tl!("{done} of {all} done"), &[("done", &done.to_string()), ("all", &entries.len().to_string())]);
                ui.label(RichText::new(count).small().color(t.text_dim));
            });
            for p in entries {
                ui.horizontal_top(|ui| {
                    let mark = match p.status {
                        PlanStatus::Pending => Mark::Waiting,
                        PlanStatus::InProgress => Mark::Running,
                        PlanStatus::Completed => Mark::Done,
                    };
                    mark.paint(ui, t);
                    let color = if p.status == PlanStatus::Completed { t.text_dim } else { t.text };
                    wrapped(ui, RichText::new(&p.content).color(color));
                });
            }
        }
        Entry::Permission { request, answer } => permission(app, ui, t, request, answer.as_ref(), waiting == Some(request.id)),
        Entry::Model { name } => {
            ui.label(RichText::new(fmt(tl!("Model: {name}"), &[("name", name)])).small().color(t.text_dim));
        }
        Entry::Stopped { stop } => {
            if let Some(says) = stop.says() {
                // Stopped by the person reads as a note; any other stop is news to them.
                let color = if *stop == StopReason::Cancelled { t.text_dim } else { t.orange };
                wrapped(ui, RichText::new(tl!(says)).small().italics().color(color));
            }
        }
        // A failed turn says why; the turn itself ends with its own event.
        Entry::Error { message } => wrapped(ui, RichText::new(message).color(t.red)),
    }
}

/// A tool call's row: how it stands and its title, folded; open, its kind, how it went, what it
/// said, and for a call of WordCraft's tools that changed the document, the way to that change.
fn tool_row(app: &mut WordApp, ui: &mut Ui, t: &Tokens, i: usize, call: &ToolCall) {
    let (mark, status) = match call.status {
        ToolStatus::Pending => (Mark::Waiting, "Waiting"),
        ToolStatus::InProgress => (Mark::Running, "Running"),
        ToolStatus::Completed => (Mark::Done, "Done"),
        ToolStatus::Failed => (Mark::Failed, "Failed"),
    };
    let failed = call.status == ToolStatus::Failed;
    let id = ui.make_persistent_id(("assistant_tool", i, call.id.as_str()));
    let mut toggled = false;
    let row = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
    let mut header = row.show_header(ui, |ui| {
        mark.paint(ui, t);
        // The title opens the row too, as the arrow does.
        let title = egui::Label::new(RichText::new(&call.title).font(regular(12.0)).color(t.text)).truncate().sense(egui::Sense::click());
        toggled = ui.add(title).on_hover_text(&call.title).clicked();
    });
    if toggled {
        header.toggle();
    }
    header.body(|ui| {
        let kind = match call.kind.as_str() {
            "read" => tl!("Read"),
            "edit" => tl!("Edit"),
            "delete" => tl!("Delete"),
            "move" => tl!("Move"),
            "search" => tl!("Search"),
            "execute" => tl!("Run"),
            "think" => tl!("Think"),
            "fetch" => tl!("Fetch"),
            _ => tl!("Other"),
        };
        let line = fmt(tl!("{kind} · {status}"), &[("kind", kind), ("status", tl!(status))]);
        ui.label(RichText::new(line).small().color(if failed { t.red } else { t.text_dim }));
        // Why a call failed, or what it said, unless that is data for the model (the start of a
        // JSON result says nothing here).
        if !call.detail.is_empty() && (failed || !call.detail.trim_start().starts_with(['{', '['])) {
            wrapped(ui, RichText::new(&call.detail).small().color(if failed { t.red } else { t.text_dim }));
        }
        if let Some(changes) = call.changes
            && ui.link(RichText::new(tl!("Show the change")).small()).on_hover_text(tl!("Select what this call changed, with Review open")).clicked()
        {
            let _ = app.run("assist.showChange", json!({"version": changes.last, "from": changes.first}));
        }
    });
}

/// A permission request: what the agent asks, and while the turn waits, its answers as buttons
/// (an agent's long names cut, whole on hover); once answered, the answer.
fn permission(app: &mut WordApp, ui: &mut Ui, t: &Tokens, request: &PermissionRequest, answer: Option<&Answer>, waiting: bool) {
    let stroke = if waiting { Stroke::new(1.5, t.accent) } else { Stroke::new(1.0, t.border_strong) };
    egui::Frame::NONE.stroke(stroke).corner_radius(6).inner_margin(6).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let name = app.conversation().map(|a| a.name().to_string()).unwrap_or_default();
        ui.label(RichText::new(fmt(tl!("{name} asks permission"), &[("name", &name)])).small().color(t.text_dim));
        wrapped(ui, RichText::new(&request.title).font(semibold(12.0)).color(t.text));
        match answer {
            None if waiting && request.options.is_empty() => {
                wrapped(ui, RichText::new(tl!("It offers no answer to choose: Stop ends the turn.")).small().color(t.orange));
            }
            None if waiting => {
                ui.horizontal_wrapped(|ui| {
                    for o in &request.options {
                        let b = ui.add(egui::Button::new(&o.name).truncate()).on_hover_text(&o.name);
                        if b.clicked() {
                            // Only the person answers: a click or key the control channel made
                            // (MCP's `click` and `key`) leaves the question waiting.
                            if app.input_from_control(ui.ctx()) {
                                app.assist.notice = Some(tl!("Only you can answer the assistant: click it yourself.").to_string());
                            } else if let Some(a) = app.conversation_mut() {
                                a.answer(request.id, Answer::Selected { option: o.id.clone() });
                            }
                        }
                    }
                });
            }
            None | Some(Answer::Cancelled) => wrapped(ui, RichText::new(tl!("No answer")).small().color(t.text_dim)),
            Some(Answer::Selected { option }) => {
                let chosen = request.options.iter().find(|o| &o.id == option).map_or(option.as_str(), |o| o.name.as_str());
                wrapped(ui, RichText::new(fmt(tl!("Answered: {option}"), &[("option", chosen)])).small().color(t.text_dim));
            }
        }
    });
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

/// The writing actions (#18) on the selection: Write a Draft writes what the prompt box says at
/// the caret (or turns the box to it, while it is empty).
fn actions(app: &mut WordApp, ui: &mut Ui) {
    ui.horizontal_wrapped(|ui| {
        let draft = action_button(ui, app, "Write a Draft", "assist.draft");
        if draft.clicked() {
            let about = app.assist.prompt.trim().to_string();
            if about.is_empty() {
                crate::assist::start_draft(app);
            } else {
                send(app, "assist.draft", json!({"about": about}));
            }
        }
        for (label, id) in [
            ("Rewrite", "assist.rewrite"),
            ("Make Shorter", "assist.shorten"),
            ("Make Longer", "assist.expand"),
            ("Proofread", "assist.proofread"),
            ("Summarize", "assist.summarize"),
        ] {
            if action_button(ui, app, label, id).clicked() {
                let _ = app.run(id, json!({}));
            }
        }
    });
}

/// A small button for writing action `id`, greyed out with the reason in its tooltip.
fn action_button(ui: &mut Ui, app: &WordApp, label: &str, id: &str) -> egui::Response {
    let r = ui.add_enabled(crate::widgets::enabled(app, id), egui::Button::new(RichText::new(tl!(label)).small()));
    match app.session.why_disabled(id) {
        Some(why) => r.on_disabled_hover_text(tl!(why)),
        None => r,
    }
}

/// Send what the prompt box says with `id`; once it is asked, the box is emptied for the next.
fn send(app: &mut WordApp, id: &str, params: serde_json::Value) {
    if app.run(id, params).is_ok() {
        app.assist.prompt.clear();
        app.assist.draft = false;
    }
    app.assist.focus_prompt = crate::assist::FOCUS_FRAMES;
}

/// The prompt box, with Send (Enter) and Stop; Shift+Enter starts a new line. While drafting it
/// says what to write at the caret, and Write (Enter) drafts it.
fn prompt(app: &mut WordApp, ui: &mut Ui, t: &Tokens) {
    let busy = app.conversation().is_some_and(|a| a.is_busy());
    let drafting = app.assist.draft;
    let hint = match app.conversation().filter(|a| !a.has_ended()) {
        _ if drafting => tl!("Say what to write at the caret…").to_string(),
        Some(a) => fmt(tl!("Ask {name}…"), &[("name", a.name())]),
        None => tl!("Ask the assistant…").to_string(),
    };
    if drafting {
        ui.horizontal(|ui| {
            ui.label(RichText::new(tl!("Drafting at the caret")).small().color(t.text_dim));
            if ui.link(RichText::new(tl!("Ask instead")).small()).clicked() {
                app.assist.draft = false;
                app.assist.focus_prompt = crate::assist::FOCUS_FRAMES;
            }
        });
    }
    let id = app.window_id("assistant_prompt");
    let mut text = std::mem::take(&mut app.assist.prompt);
    let r = super::comments::capped_field(
        ui,
        id.with("scroll"),
        None,
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
    let id = if drafting { "assist.draft" } else { "assist.ask" };
    let can_send = !busy && !app.assist.prompt.trim().is_empty() && crate::widgets::enabled(app, id);
    ui.horizontal(|ui| {
        let send_button = ui.add_enabled(can_send, egui::Button::new(if drafting { tl!("Write") } else { tl!("Send") }));
        let send_button = match app.session.why_disabled(id) {
            Some(why) if !busy => send_button.on_disabled_hover_text(tl!(why)),
            _ => send_button,
        };
        if (send_button.clicked() || (enter && can_send)) && can_send {
            let text = app.assist.prompt.clone();
            let params = if drafting { json!({"about": text}) } else { json!({"prompt": text}) };
            send(app, id, params);
        }
        if ui.add_enabled(busy, egui::Button::new(tl!("Stop"))).clicked() {
            let _ = app.run("assist.cancel", json!({}));
        }
        if !busy && let Some(a) = app.conversation().filter(|a| !a.has_ended() && !a.tracked() && a.role() >= Role::Editor) {
            ui.label(RichText::new(fmt(tl!("{name}'s edits aren't tracked"), &[("name", a.name())])).small().color(t.orange));
        }
    });
}

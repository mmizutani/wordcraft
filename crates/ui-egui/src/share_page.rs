//! File › Share (#33): share the document shown with people and agents, invite them with a role
//! (and, for an agent, which files it may use), see who is in it, change a role and take an
//! invite back. Every control runs a command (`room.share`, `room.invite`, `room.role`,
//! `room.revoke`, `room.relay`, `room.stop`, [`crate::share::room_method`]), the ones the control
//! channel and MCP run, so a program can do whatever the page does; the page only reads the
//! room's state.

use egui::{RichText, Ui, vec2};
use serde_json::{Value, json};
use wordcraft_engine::agent_files::AgentFiles;
use wordcraft_engine::room::Role;

use crate::WordApp;
use crate::theme::{Tokens, medium, semibold};

/// What the Share page's invite form holds between frames. Not saved anywhere.
#[derive(Debug)]
pub(crate) struct Form {
    /// The name being typed for the next invite.
    pub(crate) name: String,
    /// The role the next invite gives.
    pub(crate) role: Role,
    /// Which files the next invite gives its agent: `None` follows File › Options › Agents.
    pub(crate) files: Option<AgentFiles>,
    /// The invite just made: its participant's name and key. The key shows only until the
    /// person is done with it, leaves the page or sharing stops: the room never shows it again.
    pub(crate) shown: Option<(String, String)>,
    /// Why the last command the page ran failed, as it said.
    pub(crate) error: String,
    /// Whether the next invite is for an agent on another computer (`remote`).
    pub(crate) remote: bool,
    /// The URL the invite just made reaches the document at, for an agent on another computer
    /// (the relay's, #76), shown with its key.
    pub(crate) shown_url: Option<String>,
    /// The relay's address and token being typed (`room.relay`).
    pub(crate) relay: String,
    pub(crate) token: String,
}

impl Default for Form {
    fn default() -> Self {
        Form {
            name: String::new(),
            role: Role::Suggester,
            files: None,
            shown: None,
            error: String::new(),
            remote: false,
            shown_url: None,
            relay: String::new(),
            token: String::new(),
        }
    }
}

/// A role's name on the page.
fn role_label(role: Role) -> &'static str {
    crate::assist::role_label(role)
}

/// What a role lets a participant do, plainly.
fn role_note(role: Role) -> &'static str {
    match role {
        Role::Viewer => "Reads the document; changes nothing.",
        Role::Commenter => "Reads and adds comments; doesn't change the text.",
        Role::Suggester => "Edits as tracked changes in their name, for you to accept or reject.",
        Role::Editor => "Edits directly, and accepts and rejects tracked changes.",
        Role::Owner => "Edits directly, and may also change the app's settings in their copy.",
    }
}

/// Which files a participant's commands can use, on the page.
fn files_label(files: AgentFiles) -> &'static str {
    match files {
        AgentFiles::None => "No files",
        AgentFiles::Folders => "Agents' folders",
        AgentFiles::All => "Any file",
    }
}

/// The invite form's file choices: what the setting gives, or fewer.
const FILE_CHOICES: [(Option<AgentFiles>, &str); 4] = [
    (None, "As File › Options › Agents allows"),
    (Some(AgentFiles::None), "No files"),
    (Some(AgentFiles::Folders), "Only the agents' folders"),
    (Some(AgentFiles::All), "Any file the setting allows"),
];

/// The line an MCP client runs to join with `key` through the room's port at `addr`.
pub(crate) fn mcp_line(key: &str, addr: std::net::SocketAddr) -> String {
    format!("WORDCRAFT_CONTROL_KEY={key} wordcraft-cli mcp --connect {addr}")
}

/// Who is in the shared document and who is invited, one row each, from what `room.participants`
/// answers (`state`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Row {
    pub(crate) name: String,
    pub(crate) role: Role,
    /// Which files their commands can use now; `None` for the window, whose commands are the
    /// person's.
    pub(crate) files: Option<AgentFiles>,
    /// How many copies of theirs are in the document now.
    pub(crate) copies: usize,
    /// Whether this is the window's own participant: the person.
    pub(crate) you: bool,
    /// Whether they were invited with a key: their role can change and their invite be taken back.
    pub(crate) invited: bool,
}

/// The rows of the participants' list: the window first, then the others in the order they
/// joined (each name once, however many copies they have), then the invites no one joined with.
pub(crate) fn rows(state: &Value) -> Vec<Row> {
    let role_of = |v: &Value| v.get("role").and_then(Value::as_str).and_then(Role::named);
    let files_of = |v: &Value| v.get("files").and_then(Value::as_str).and_then(AgentFiles::named);
    let name_of = |v: &Value| v.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
    let setting = state.get("agentFiles").and_then(Value::as_str).and_then(AgentFiles::named).unwrap_or_default();
    let invited: Vec<&Value> = state.get("invited").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
    let is_invited = |name: &str| invited.iter().any(|i| name_of(i).to_lowercase() == name.to_lowercase());
    let you = state.get("you");
    let you_id = you.and_then(|y| y.get("id")).cloned();
    let mut rows: Vec<Row> = Vec::new();
    if let Some(y) = you {
        rows.push(Row { name: name_of(y), role: role_of(y).unwrap_or(Role::Owner), files: None, copies: 1, you: true, invited: false });
    }
    for p in state.get("participants").and_then(Value::as_array).into_iter().flatten() {
        if you_id.is_some() && p.get("id") == you_id.as_ref() {
            continue;
        }
        let name = name_of(p);
        if let Some(row) = rows.iter_mut().find(|r| !r.you && r.name.to_lowercase() == name.to_lowercase()) {
            row.copies = row.copies.saturating_add(1);
            continue;
        }
        let invited = is_invited(&name);
        rows.push(Row { role: role_of(p).unwrap_or(Role::Viewer), files: files_of(p), copies: 1, you: false, invited, name });
    }
    for i in invited {
        let name = name_of(i);
        if rows.iter().any(|r| r.name.to_lowercase() == name.to_lowercase()) {
            continue;
        }
        let files = Some(files_of(i).map_or(setting, |f| f.at_most(setting)));
        rows.push(Row { role: role_of(i).unwrap_or(Role::Viewer), files, copies: 0, you: false, invited: true, name });
    }
    rows
}

/// Run command `id` for the page, keeping what it said when it failed.
fn run(app: &mut WordApp, id: &str, params: Value) -> Option<Value> {
    match app.run(id, params) {
        Ok(v) => {
            app.share_form.error.clear();
            Some(v)
        }
        Err(e) => {
            app.share_form.error = tl!(&e).to_string();
            None
        }
    }
}

fn heading(ui: &mut Ui, s: &str) {
    ui.label(RichText::new(tl!(s)).font(semibold(26.0)));
    ui.add_space(16.0);
}

fn section(ui: &mut Ui, s: &str) {
    ui.add_space(14.0);
    ui.label(RichText::new(tl!(s)).font(semibold(15.0)));
    ui.add_space(4.0);
}

fn note(ui: &mut Ui, s: &str) {
    ui.label(RichText::new(s).small().weak());
}

/// Draws File › Share.
pub(crate) fn page(app: &mut WordApp, ui: &mut Ui) {
    heading(ui, "Share");
    if !crate::assist::desktop(app) {
        // Nobody reaches the web app's room: people and agents join through the desktop app.
        ui.label(tl!("People and agents join a shared document through the desktop app. Here, export a copy to send."));
        ui.add_space(8.0);
        export_button(app, ui);
        return;
    }
    ui.label(tl!(
        "Work on this document with people and AI agents at the same time. Each works on a copy of their own, and everyone's changes reach everyone as they are made."
    ));
    let state = app.share_state();
    if state.get("shared").and_then(Value::as_bool) != Some(true) {
        app.share_form.shown = None;
        not_shared(app, ui);
    } else if app.shows_shared_document() {
        shared(app, ui, &state);
    } else {
        ui.add_space(12.0);
        ui.label(tl!("Another open document is shared. Stop sharing it to share this one."));
        ui.add_space(6.0);
        stop_button(app, ui);
    }
    error_line(app, ui);
    relay_section(app, ui);
    section(ui, "Send a copy");
    note(ui, tl!("A PDF or Word document to send by email or keep, without sharing the document itself."));
    ui.add_space(4.0);
    export_button(app, ui);
}

fn error_line(app: &WordApp, ui: &mut Ui) {
    if !app.share_form.error.is_empty() {
        ui.add_space(6.0);
        ui.label(RichText::new(&app.share_form.error).color(ui.visuals().error_fg_color));
    }
}

fn export_button(app: &mut WordApp, ui: &mut Ui) {
    if ui.button(RichText::new(tl!("Export a copy…")).font(medium(13.5))).clicked() {
        let _ = app.run("ui.backstage", json!({"value": true, "page": "export"}));
    }
}

fn stop_button(app: &mut WordApp, ui: &mut Ui) {
    if ui
        .button(RichText::new(tl!("Stop sharing")).font(medium(13.5)))
        .on_hover_text(tl!("Everyone else's copy stops at once. You keep the document with every change made so far."))
        .clicked()
    {
        app.share_form.shown = None;
        run(app, "room.stop", json!({}));
    }
}

fn not_shared(app: &mut WordApp, ui: &mut Ui) {
    ui.add_space(14.0);
    let author = app.session.author.trim();
    let name = if author.is_empty() { "Owner" } else { author };
    let joins = tl!("You join as {name}, the document's owner. Then invite others, each with a role.").replace("{name}", name);
    ui.label(joins);
    ui.add_space(8.0);
    if ui.button(RichText::new(tl!("Share this document")).font(medium(14.0))).clicked() {
        run(app, "room.share", json!({}));
    }
}

fn shared(app: &mut WordApp, ui: &mut Ui, state: &Value) {
    let t = Tokens::get(ui.ctx());
    ui.add_space(14.0);
    let version = state.get("version").and_then(Value::as_u64).unwrap_or(0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(tl!("Shared")).font(semibold(14.0)).color(t.accent_text));
        ui.label(crate::i18n::tn(version, "{n} change so far", "{n} changes so far").replace("{n}", &version.to_string()));
    });
    ui.add_space(6.0);
    stop_button(app, ui);
    invite_form(app, ui);
    if app.share_form.shown.is_some() {
        shown_key(app, ui);
    }
    participants(app, ui, state);
}

/// Invite: a name, a role, and for an agent which files it may use; Invite runs `room.invite`
/// and shows the key it gives.
fn invite_form(app: &mut WordApp, ui: &mut Ui) {
    section(ui, "Invite someone");
    note(
        ui,
        tl!(
            "A person's program or an AI agent joins with the key an invite gives. A name stays theirs in this document: their comments and tracked changes carry it."
        ),
    );
    ui.add_space(6.0);
    let mut invite = false;
    crate::rtl::Table::new("share_invite").columns(2).top().spacing(vec2(12.0, 8.0)).show(ui, |t| {
        t.row(|r| {
            let label = r.cell(|ui| ui.label(tl!("Name")));
            r.cell(|ui| {
                let field = ui
                    .add(egui::TextEdit::singleline(&mut app.share_form.name).desired_width(260.0).hint_text(tl!("Fact checker")))
                    .labelled_by(label.id);
                if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    invite = true;
                }
            });
        });
        t.row(|r| {
            let label = r.cell(|ui| ui.label(tl!("Role")));
            r.cell(|ui| {
                crate::rtl::vertical(ui, |ui| {
                    let role = app.share_form.role;
                    let combo =
                        craft_ui::ComboBox::from_id_salt("share_invite_role").selected_text(tl!(role_label(role))).width(260.0).show_ui(ui, |ui| {
                            for r in Role::ALL {
                                if ui.selectable_label(r == role, tl!(role_label(r))).on_hover_text(tl!(role_note(r))).clicked() {
                                    app.share_form.role = r;
                                }
                            }
                        });
                    combo.response.labelled_by(label.id);
                    ui.label(RichText::new(tl!(role_note(app.share_form.role))).small().weak());
                })
            });
        });
        t.row(|r| {
            let label = r.cell(|ui| ui.label(tl!("Files")));
            r.cell(|ui| {
                let files = app.share_form.files;
                let shown = FILE_CHOICES.iter().find(|(f, _)| *f == files).map_or("", |(_, l)| l);
                let combo = craft_ui::ComboBox::from_id_salt("share_invite_files").selected_text(tl!(shown)).width(260.0).show_ui(ui, |ui| {
                    for (f, l) in FILE_CHOICES {
                        if ui.selectable_label(f == files, tl!(l)).clicked() {
                            app.share_form.files = f;
                        }
                    }
                });
                combo
                    .response
                    .labelled_by(label.id)
                    .on_hover_text(tl!("For an agent: which files its commands may read and write. Never more than File › Options › Agents allows."));
            });
        });
        t.row(|r| {
            r.cell(|_| {});
            r.cell(|ui| {
                ui.checkbox(&mut app.share_form.remote, tl!("On another computer")).on_hover_text(tl!(
                    "For an agent elsewhere, such as one in a cloud sandbox: it reaches the document through the relay, uses no files unless you choose some, and its key stops working after a day."
                ));
            });
        });
    });
    ui.add_space(6.0);
    let named = !app.share_form.name.trim().is_empty();
    if ui.add_enabled(named, egui::Button::new(RichText::new(tl!("Invite")).font(medium(13.5)))).clicked() {
        invite = true;
    }
    if invite && named {
        let f = &app.share_form;
        let params = json!({"name": f.name.trim(), "role": f.role, "files": f.files.map(AgentFiles::name), "remote": f.remote});
        app.invite_on_share_page(params);
    }
}

impl WordApp {
    /// What File › Share's Invite does with `params` (`room.invite`'s): the key it gives shows on
    /// the page, and the form is ready for the next. The invite's answer, `None` when it failed
    /// (the page says why). Public for headless screenshots (`ui_shot`).
    #[doc(hidden)]
    pub fn invite_on_share_page(&mut self, params: Value) -> Option<Value> {
        let v = run(self, "room.invite", params)?;
        let key = v.get("key").and_then(Value::as_str).unwrap_or_default().to_string();
        let name = v.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
        self.share_form.shown = Some((name, key));
        self.share_form.shown_url = v.get("url").and_then(Value::as_str).map(str::to_string);
        self.share_form.name.clear();
        Some(v)
    }
}

/// The key the last invite gave, to copy now: it isn't shown again.
fn shown_key(app: &mut WordApp, ui: &mut Ui) {
    let Some((name, key)) = app.share_form.shown.clone() else { return };
    let t = Tokens::get(ui.ctx());
    let addr = app.lobby.addr();
    ui.add_space(10.0);
    egui::Frame::NONE.fill(t.panel).stroke(egui::Stroke::new(1.0, t.border)).corner_radius(6.0).inner_margin(12).show(ui, |ui| {
        ui.label(RichText::new(tl!("Key for {name}").replace("{name}", &name)).font(semibold(14.0)));
        note(ui, tl!("Copy it now and give it to them: it isn't shown again. Anyone with it joins as them, until you remove their access."));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(RichText::new(&key).monospace()).selectable(true));
            if ui.button(tl!("Copy key")).clicked() {
                ui.ctx().copy_text(key.clone());
            }
        });
        ui.add_space(6.0);
        match (app.share_form.shown_url.clone(), addr) {
            (Some(url), _) => {
                let config = serde_json::to_string_pretty(&crate::relay::mcp_config(&url, &key)).unwrap_or_default();
                note(ui, tl!("For an MCP client on another computer, such as an AI agent's:"));
                ui.add(egui::Label::new(RichText::new(&config).monospace()).selectable(true));
                if ui.button(tl!("Copy configuration")).clicked() {
                    ui.ctx().copy_text(config.clone());
                }
            }
            (None, Some(addr)) => {
                let line = mcp_line(&key, addr);
                note(ui, tl!("For an MCP client, such as an AI agent's:"));
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(RichText::new(&line).monospace()).selectable(true));
                    if ui.button(tl!("Copy command")).clicked() {
                        ui.ctx().copy_text(line.clone());
                    }
                });
            }
            (None, None) => note(ui, tl!("Agents join through the app's room port, which isn't open: they can't join right now.")),
        }
        ui.add_space(6.0);
        if ui.button(tl!("Done")).clicked() {
            app.share_form.shown = None;
        }
    });
}

/// How the relay's connection is, in words.
fn relay_status(state: &Value) -> String {
    let status = state.get("status").and_then(Value::as_str).unwrap_or("off");
    let words = match status {
        "waiting" => tl!("Connects when you share a document."),
        "connecting" => tl!("Connecting…"),
        "connected" => tl!("Connected: agents on other computers reach the shared document."),
        "retrying" => tl!("Can't reach the relay; trying again. Agents are told the document is offline."),
        "refused" => tl!("The relay refused this computer."),
        "failed" => tl!("Not connected."),
        _ => tl!("Not connected."),
    };
    match state.get("why").and_then(Value::as_str) {
        Some(why) => format!("{words} {why}"),
        None => words.to_string(),
    }
}

/// Agents on other computers: the relay they reach the shared document through (`room.relay`).
fn relay_section(app: &mut WordApp, ui: &mut Ui) {
    section(ui, "Agents on other computers");
    let state = app.relay_method(&json!({})).unwrap_or(Value::Null);
    let relay = state.get("relay").and_then(Value::as_str).map(str::to_string);
    let Some(relay) = relay else {
        note(
            ui,
            tl!(
                "An AI agent on another computer, such as one in a cloud sandbox, reaches a shared document through a relay this app connects to. Nothing on this computer is opened to the internet. Enter the relay's address and the token from whoever runs it."
            ),
        );
        ui.add_space(6.0);
        let mut connect = false;
        crate::rtl::Table::new("share_relay").columns(2).spacing(vec2(12.0, 8.0)).show(ui, |t| {
            t.row(|r| {
                let label = r.cell(|ui| ui.label(tl!("Relay")));
                r.cell(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut app.share_form.relay).desired_width(320.0).hint_text(tl!("https://relay.example.org")))
                        .labelled_by(label.id);
                });
            });
            t.row(|r| {
                let label = r.cell(|ui| ui.label(tl!("Token")));
                r.cell(|ui| {
                    let field =
                        ui.add(egui::TextEdit::singleline(&mut app.share_form.token).password(true).desired_width(320.0)).labelled_by(label.id);
                    if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        connect = true;
                    }
                });
            });
        });
        ui.add_space(6.0);
        let ready = !app.share_form.relay.trim().is_empty() && !app.share_form.token.trim().is_empty();
        if ui.add_enabled(ready, egui::Button::new(RichText::new(tl!("Use this relay")).font(medium(13.5)))).clicked() {
            connect = true;
        }
        if connect && ready {
            let params = json!({"relay": app.share_form.relay.trim(), "token": app.share_form.token.trim()});
            if run(app, "room.relay", params).is_some() {
                app.share_form.relay.clear();
                app.share_form.token.clear();
            }
        }
        return;
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(&relay).monospace());
        ui.label(relay_status(&state));
    });
    if let Some(url) = state.get("url").and_then(Value::as_str) {
        ui.add_space(4.0);
        note(ui, tl!("Agents invited on another computer use this address with their key:"));
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(RichText::new(url).monospace()).selectable(true));
            if ui.button(tl!("Copy address")).clicked() {
                ui.ctx().copy_text(url.to_string());
            }
        });
    }
    ui.add_space(6.0);
    if ui
        .button(RichText::new(tl!("Stop using this relay")).font(medium(13.5)))
        .on_hover_text(tl!("Agents on other computers can't reach the document any more. The relay's address and token are forgotten."))
        .clicked()
    {
        run(app, "room.relay", json!({"off": true}));
    }
}

/// Who is in the document: their role (an invited participant's can change), which files their
/// commands can use, whether they're connected, and Remove access for an invite.
fn participants(app: &mut WordApp, ui: &mut Ui, state: &Value) {
    section(ui, "People and agents");
    let rows = rows(state);
    // The assistant's role is the Assistant pane's to choose (`room.role` refuses it).
    let assistant: Vec<bool> = rows.iter().map(|r| app.assistant_named(&r.name).is_some()).collect();
    let mut action: Option<(&str, Value)> = None;
    crate::rtl::Table::new("share_participants").columns(5).spacing(vec2(18.0, 8.0)).striped(true).show(ui, |t| {
        t.row(|r| {
            for h in ["Name", "Role", "Files", "Status", ""] {
                r.cell(|ui| ui.label(RichText::new(if h.is_empty() { "" } else { tl!(h) }).strong()));
            }
        });
        for (i, row) in rows.iter().enumerate() {
            t.row(|r| {
                r.cell(|ui| {
                    if row.you {
                        ui.label(tl!("{name} (you)").replace("{name}", &row.name));
                    } else {
                        ui.label(&row.name);
                    }
                });
                r.cell(|ui| {
                    if assistant.get(i).copied().unwrap_or(false) {
                        ui.label(tl!(role_label(row.role))).on_hover_text(tl!("The assistant's role is chosen in the Assistant pane."));
                    } else if row.invited {
                        let combo = craft_ui::ComboBox::from_id_salt(("share_role", i))
                            .selected_text(tl!(role_label(row.role)))
                            .width(130.0)
                            .show_ui(ui, |ui| {
                                for role in Role::ALL {
                                    if ui.selectable_label(role == row.role, tl!(role_label(role))).on_hover_text(tl!(role_note(role))).clicked()
                                        && role != row.role
                                    {
                                        action = Some(("room.role", json!({"name": row.name, "role": role})));
                                    }
                                }
                            });
                        let name = tl!("Role of {name}").replace("{name}", &row.name);
                        combo.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, &name));
                    } else {
                        ui.label(tl!(role_label(row.role)));
                    }
                });
                r.cell(|ui| ui.label(row.files.map_or("—", |f| tl!(files_label(f)))));
                r.cell(|ui| {
                    let status = match (row.you, row.copies) {
                        (true, _) => tl!("Here").to_string(),
                        (false, 0) => tl!("Invited, not joined yet").to_string(),
                        (false, 1) => tl!("Connected").to_string(),
                        (false, n) => tl!("Connected ({n} copies)").replace("{n}", &n.to_string()),
                    };
                    ui.label(status);
                });
                r.cell(|ui| {
                    if row.invited {
                        let remove = ui
                            .button(tl!("Remove access"))
                            .on_hover_text(tl!("Takes their key back: their copies stop at once, and the key no longer joins."));
                        if remove.clicked() {
                            action = Some(("room.revoke", json!({"name": row.name})));
                        }
                    } else {
                        ui.label("");
                    }
                });
            });
        }
    });
    if let Some((id, params)) = action {
        run(app, id, params);
    }
}

#[cfg(test)]
mod tests;

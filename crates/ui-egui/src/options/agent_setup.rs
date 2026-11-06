//! File › Options › Assistant › Agents (#24): the ACP agents WordCraft knows (Claude Code, Codex,
//! Pi) and the person's own, each with the command, arguments and environment it starts with.
//! They are saved through the assistant's library in the agents' settings file
//! ([`AgentSettings`]: `assistant-agents.json` in the settings folder, which on Unix only its
//! owner may read), and each says whether it can start, or what to install first: WordCraft
//! never installs an agent.
//!
//! Which program WordCraft starts, and the keys an agent signs in with, are the person's alone:
//! no command reads or writes them, so neither a script nor an agent in the document can change
//! them or read them back. The variables' values are masked (egui's password fields, which
//! never show, copy or expose them as text) and never logged, and nothing here derives `Debug`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use egui::{RichText, Ui, vec2};
use wordcraft_assist::acp::presets::{self, AgentPrefs, AgentSettings, CUSTOM_ID, CUSTOM_KEY, PRESETS, Preset};
use wordcraft_assist::acp::{ADDR_VAR, KEY_VAR};
use wordcraft_assist::{Offer, Readiness};

use super::{note, section};
use crate::WordApp;
use crate::i18n::fmt;
use crate::theme::{Tokens, semibold};

/// Longest name typed for the custom agent, in characters.
const MAX_NAME: usize = 100;

/// Longest command, in characters: at most 4 bytes each, well within the 32 KiB the settings keep.
const MAX_COMMAND: usize = 4096;

/// Longest arguments field, in characters (32 KiB at most).
const MAX_ARGS: usize = 8192;

/// Longest variable name, in characters (the settings keep names of up to 256 bytes).
const MAX_VAR_NAME: usize = 64;

/// Longest variable value, in characters (32 KiB at most).
const MAX_VALUE: usize = 8192;

/// Most arguments, and most variables, an agent's settings keep.
const MOST: usize = 64;

/// The agents while File › Options › Assistant shows them: as saved, as edited, and whether
/// each can start. No `Debug`: the variables may hold API keys.
pub(crate) struct AgentSetup {
    /// The settings folder they're saved in.
    dir: PathBuf,
    /// As the file holds them.
    saved: AgentSettings,
    /// As edited, in the picker's order: the presets, then the custom agent.
    drafts: Vec<Draft>,
    /// What the drafts would save, as the file reads it back, or what to fix first: worked out
    /// when they change.
    check: Result<AgentSettings, String>,
    /// Whether each agent can start, as last asked: when the page opens, after a save, and on
    /// Check again. Asking reads the file and looks programs up on `PATH`, so not every frame.
    pub(crate) offers: Vec<Offer>,
    /// What the last save did: its message, and whether it failed.
    notice: Option<(String, bool)>,
    /// The pass the page was last drawn in: drawn again after a gap, it reads the file again.
    pass: u64,
}

/// One agent as the page edits it. No `Debug`: its variables may hold API keys.
#[derive(Clone, PartialEq)]
struct Draft {
    /// Its key in the settings (`claude-code`, `codex`, `pi`, `custom`).
    key: &'static str,
    /// Its id, as `assist.backend {id}` and its offer name it.
    id: &'static str,
    /// The preset it is, with its own adapter; `None` for the custom agent.
    preset: Option<Preset>,
    /// The custom agent's name (a preset keeps its own: what a file gives one is kept, unused).
    name: String,
    command: String,
    /// The arguments, one per line.
    args: String,
    env: Vec<Var>,
}

/// An environment variable as the page edits it.
#[derive(Clone, Default, PartialEq)]
struct Var {
    name: String,
    /// Masked on the page.
    value: String,
}

impl AgentSetup {
    fn open(dir: PathBuf, offers: Vec<Offer>) -> AgentSetup {
        let saved = AgentSettings::load(&dir);
        AgentSetup { drafts: drafts(&saved), check: Ok(saved.clone()), saved, dir, offers, notice: None, pass: 0 }
    }

    /// Whether there are edits not saved.
    fn dirty(&self) -> bool {
        !matches!(&self.check, Ok(s) if *s == self.saved)
    }

    /// The drafts changed: work out what they would save.
    fn edited(&mut self) {
        self.check = settings_from(&self.saved, &self.drafts);
        self.notice = None;
    }

    /// Back to what is saved.
    fn discard(&mut self) {
        self.drafts = drafts(&self.saved);
        self.check = Ok(self.saved.clone());
    }
}

/// The agents as `saved` sets them up, in the picker's order.
fn drafts(saved: &AgentSettings) -> Vec<Draft> {
    let presets = PRESETS.iter().map(|p| draft(p.key, p.id, Some(*p), saved));
    presets.chain([draft(CUSTOM_KEY, CUSTOM_ID, None, saved)]).collect()
}

fn draft(key: &'static str, id: &'static str, preset: Option<Preset>, saved: &AgentSettings) -> Draft {
    let prefs = saved.agent(key);
    let env = prefs.env.into_iter().map(|(name, value)| Var { name, value }).collect();
    Draft { key, id, preset, name: prefs.name, command: prefs.command, args: prefs.args.join("\n"), env }
}

/// The agent's name, for what to fix.
fn draft_label(d: &Draft) -> String {
    match &d.preset {
        Some(p) => p.label.to_string(),
        None if d.name.trim().is_empty() => tl!("Custom agent").to_string(),
        None => d.name.trim().to_string(),
    }
}

/// What `drafts` save over `saved` (an agent the page doesn't show is kept), as the settings
/// file reads it back; or what to fix first.
fn settings_from(saved: &AgentSettings, drafts: &[Draft]) -> Result<AgentSettings, String> {
    let mut settings = saved.clone();
    for d in drafts {
        let prefs = prefs_of(d)?;
        if prefs == AgentPrefs::default() {
            settings.agents.remove(d.key);
        } else {
            settings.agents.insert(d.key.to_string(), prefs);
        }
    }
    // As the file reads it back, so the page shows what it holds (a name loses its control
    // characters, say).
    let value = serde_json::to_value(&settings).map_err(|e| e.to_string())?;
    Ok(AgentSettings::from_value(&value))
}

/// One agent's settings from its draft: the command and each argument trimmed, blank lines and
/// blank variables left out, and NUL, which no program, argument or variable can hold, taken out.
fn prefs_of(d: &Draft) -> Result<AgentPrefs, String> {
    let agent = draft_label(d);
    let clean = |s: &str| s.replace('\0', "");
    let args: Vec<String> = d.args.lines().map(|l| clean(l.trim())).filter(|l| !l.is_empty()).collect();
    let most = MOST.to_string();
    if args.len() > MOST {
        return Err(fmt(tl!("{agent}: at most {n} arguments"), &[("agent", &agent), ("n", &most)]));
    }
    let mut env = BTreeMap::new();
    for v in &d.env {
        let name = clean(v.name.trim());
        if name.is_empty() && v.value.is_empty() {
            continue;
        }
        if name.is_empty() {
            return Err(fmt(tl!("{agent}: a variable has a value but no name"), &[("agent", &agent)]));
        }
        if name.contains('=') {
            return Err(fmt(tl!("{agent}: {name}: a variable's name can't hold ="), &[("agent", &agent), ("name", &name)]));
        }
        if env.insert(name.clone(), clean(&v.value)).is_some() {
            return Err(fmt(tl!("{agent}: {name} is there twice"), &[("agent", &agent), ("name", &name)]));
        }
    }
    if env.len() > MOST {
        return Err(fmt(tl!("{agent}: at most {n} variables"), &[("agent", &agent), ("n", &most)]));
    }
    Ok(AgentPrefs { name: clean(d.name.trim()), command: clean(d.command.trim()), args, env })
}

/// File › Options › Assistant › Agents: each agent, whether it can start, and its settings, with
/// Save. Without a settings folder (the web, tests) there is nothing to save them in.
pub(crate) fn show(app: &mut WordApp, ui: &mut Ui) {
    section(ui, "Agents");
    note(
        ui,
        "An agent is a program on your computer that WordCraft starts for a conversation in the Assistant pane, with the command, arguments and environment set here. WordCraft never installs one: each says what it needs.",
    );
    let Some(dir) = app.services.settings_dir.clone() else {
        note(ui, "WordCraft has no settings folder here, so agents can't be set up.");
        return;
    };
    let pass = ui.ctx().cumulative_pass_nr();
    let mut setup = match app.assist.setup.take() {
        Some(mut s) if s.dir == dir => {
            if s.pass.saturating_add(1) < pass {
                // Shown again: the file may have changed (kept while there are edits), and so may
                // what is installed.
                if !s.dirty() {
                    s.saved = AgentSettings::load(&s.dir);
                    s.discard();
                }
                s.offers = presets::offers(&crate::assist::host(app));
            }
            s
        }
        _ => AgentSetup::open(dir, presets::offers(&crate::assist::host(app))),
    };
    setup.pass = pass;
    let t = Tokens::get(ui.ctx());
    let mut edited = false;
    for i in 0..setup.drafts.len() {
        edited |= agent(ui, &t, &mut setup, i);
    }
    if edited {
        setup.edited();
    }
    ui.add_space(4.0);
    let (mut save, mut discard, mut check) = (false, false, false);
    ui.horizontal(|ui| {
        save = ui.add_enabled(setup.dirty() && setup.check.is_ok(), egui::Button::new(tl!("Save"))).clicked();
        discard = ui.add_enabled(setup.dirty(), egui::Button::new(tl!("Discard changes"))).clicked();
        check = ui.button(tl!("Check again")).on_hover_text(tl!("Look for the agents' programs again, after installing one")).clicked();
    });
    if let Err(why) = &setup.check {
        ui.label(RichText::new(fmt(tl!("Can't save: {why}"), &[("why", why)])).small().color(t.orange));
    }
    if let Some((text, failed)) = &setup.notice {
        ui.label(RichText::new(text).small().color(if *failed { t.red } else { t.text_dim }));
    }
    note(ui, "An agent starts with what's saved here the next time it starts; one already running keeps what it started with.");
    if discard {
        setup.discard();
        setup.notice = None;
    }
    if save {
        save_setup(app, &mut setup);
    }
    if check {
        setup.offers = presets::offers(&crate::assist::host(app));
        crate::assist::refresh_offers(app);
    }
    app.assist.setup = Some(setup);
}

/// Save the drafts through the library, then ask again whether each agent can start (the pane's
/// picker too).
fn save_setup(app: &mut WordApp, setup: &mut AgentSetup) {
    let Ok(settings) = setup.check.clone() else { return };
    match settings.save(&setup.dir) {
        Ok(()) => {
            setup.saved = settings;
            setup.discard();
            setup.notice = Some((tl!("Saved.").to_string(), false));
            setup.offers = presets::offers(&crate::assist::host(app));
            crate::assist::refresh_offers(app);
        }
        Err(e) => {
            // The error names the file, never what it holds.
            log::warn!("couldn't save the agents' settings: {e}");
            setup.notice = Some((fmt(tl!("Couldn't save the agents' settings: {error}"), &[("error", &e.to_string())]), true));
        }
    }
}

/// One agent: a row saying whether it can start, which opens to its settings. Whether its
/// settings changed.
fn agent(ui: &mut Ui, t: &Tokens, setup: &mut AgentSetup, i: usize) -> bool {
    let offer = setup.drafts.get(i).and_then(|d| setup.offers.iter().find(|o| o.id == d.id)).cloned();
    let Some(d) = setup.drafts.get_mut(i) else { return false };
    let readiness = offer.as_ref().map(|o| o.readiness.clone());
    let shown = offer.map_or_else(|| draft_label(d), |o| o.label);
    let (word, ready) = match &readiness {
        Some(Readiness::Ready) => ("Ready", true),
        Some(Readiness::Install { .. }) => ("Not found", false),
        Some(Readiness::SignIn { .. }) => ("Sign-in needed", false),
        _ => ("Unavailable", false),
    };
    let id = ui.make_persistent_id(("options_agent", d.key));
    let mut toggled = false;
    let row = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
    let mut header = row.show_header(ui, |ui| {
        // The name opens the row too, as the arrow does.
        let text = fmt(tl!("{agent} · {state}"), &[("agent", &shown), ("state", tl!(word))]);
        let label = egui::Label::new(RichText::new(text).font(semibold(13.0)).color(if ready { t.text } else { t.text_dim }));
        toggled = ui.add(label.truncate().sense(egui::Sense::click())).clicked();
    });
    if toggled {
        header.toggle();
    }
    let mut edited = false;
    header.body(|ui| {
        let needs = match &readiness {
            Some(Readiness::Install { how }) => Some(fmt(tl!("Install: {how}"), &[("how", how)])),
            Some(Readiness::SignIn { how }) => Some(fmt(tl!("Sign in: {how}"), &[("how", how)])),
            Some(Readiness::Unavailable { why }) => Some(fmt(tl!("Unavailable: {why}"), &[("why", why)])),
            _ => None,
        };
        if let Some(needs) = needs {
            ui.add(egui::Label::new(RichText::new(needs).small().color(t.orange)).wrap());
        }
        edited = fields(ui, t, d);
    });
    edited
}

/// An agent's command, arguments and environment (and the custom agent's name). Whether any
/// changed.
fn fields(ui: &mut Ui, t: &Tokens, d: &mut Draft) -> bool {
    let mut edited = false;
    crate::rtl::Table::new("options_agent_fields").columns(2).spacing(vec2(8.0, 6.0)).show(ui, |rows| {
        if d.preset.is_none() {
            rows.row(|r| {
                let label = r.cell(|ui| ui.label(tl!("Name:")));
                r.cell(|ui| {
                    let field = egui::TextEdit::singleline(&mut d.name).desired_width(260.0).char_limit(MAX_NAME).hint_text(tl!("Custom agent"));
                    edited |= ui.add(field).labelled_by(label.id).changed();
                });
            });
        }
        rows.row(|r| {
            let label = r.cell(|ui| ui.label(tl!("Command:")));
            r.cell(|ui| {
                let mut field = egui::TextEdit::singleline(&mut d.command).desired_width(380.0).char_limit(MAX_COMMAND);
                if let Some(p) = &d.preset {
                    field = field.hint_text(p.program);
                }
                edited |= ui.add(field).labelled_by(label.id).changed();
            });
        });
        rows.row(|r| {
            let label = r.cell(|ui| ui.label(tl!("Arguments:")));
            r.cell(|ui| {
                // A preset's adapter takes none: arguments go with a command of the person's own.
                let usable = d.preset.is_none() || !d.command.trim().is_empty();
                let field = egui::TextEdit::multiline(&mut d.args).desired_width(380.0).desired_rows(2);
                let response = ui.add_enabled(usable, field).labelled_by(label.id);
                if response.changed() {
                    // A multi-line field has no character limit of its own.
                    if let Some((cut, _)) = d.args.char_indices().nth(MAX_ARGS) {
                        d.args.truncate(cut);
                    }
                    edited = true;
                }
            });
        });
    });
    match &d.preset {
        Some(p) => {
            let text = match p.package {
                Some(package) => fmt(
                    tl!(
                        "Leave the command empty to run {program} from PATH, or {package} with npx when it isn't installed. Arguments, one per line, go with a command of your own."
                    ),
                    &[("program", p.program), ("package", package)],
                ),
                None => fmt(
                    tl!("Leave the command empty to run {program} from PATH. Arguments, one per line, go with a command of your own."),
                    &[("program", p.program)],
                ),
            };
            note_with(ui, text);
        }
        None => note(ui, "The command is an absolute path, or a program's name looked up on PATH. Arguments: one per line."),
    }
    ui.add_space(4.0);
    edited |= environment(ui, t, d);
    edited
}

/// An agent's environment variables: names, masked values, Remove, and Add variable.
fn environment(ui: &mut Ui, t: &Tokens, d: &mut Draft) -> bool {
    let mut edited = false;
    ui.label(RichText::new(tl!("Environment")).font(semibold(12.5)));
    let mut remove = None;
    if !d.env.is_empty() {
        let key = d.key;
        // Sized, not just wanting a width: a column but the last is as wide as it was.
        let height = ui.spacing().interact_size.y;
        crate::rtl::Table::new("options_agent_env").columns(3).spacing(vec2(8.0, 4.0)).show(ui, |rows| {
            let (name_label, value_label) = rows.row(|r| {
                let name = r.cell(|ui| ui.label(RichText::new(tl!("Name")).small()));
                let value = r.cell(|ui| ui.label(RichText::new(tl!("Value")).small()));
                r.cell(|_| ());
                (name.id, value.id)
            });
            for (j, v) in d.env.iter_mut().enumerate() {
                rows.row(|r| {
                    r.cell(|ui| {
                        let field = egui::TextEdit::singleline(&mut v.name).id_salt(("agent_var_name", key, j)).char_limit(MAX_VAR_NAME);
                        edited |= ui.add_sized(vec2(170.0, height), field).labelled_by(name_label).changed();
                    });
                    r.cell(|ui| {
                        let field =
                            egui::TextEdit::singleline(&mut v.value).id_salt(("agent_var_value", key, j)).password(true).char_limit(MAX_VALUE);
                        edited |= ui.add_sized(vec2(220.0, height), field).labelled_by(value_label).changed();
                    });
                    r.cell(|ui| {
                        if ui.button(tl!("Remove")).clicked() {
                            remove = Some(j);
                        }
                    });
                });
            }
        });
    }
    if let Some(j) = remove.filter(|j| *j < d.env.len()) {
        d.env.remove(j);
        edited = true;
    }
    if ui.add_enabled(d.env.len() < MOST, egui::Button::new(tl!("Add variable"))).clicked() {
        d.env.push(Var::default());
    }
    // WordCraft's own variables come from WordCraft only.
    for v in &d.env {
        let name = v.name.trim();
        if [KEY_VAR, ADDR_VAR].iter().any(|own| own.eq_ignore_ascii_case(name)) {
            let text = fmt(tl!("WordCraft sets {name} itself: what's here is never used."), &[("name", name)]);
            ui.add(egui::Label::new(RichText::new(text).small().color(t.orange)).wrap());
        }
    }
    note(ui, "Values are hidden here and never logged.");
    edited
}

/// A note made of a template filled in (already looked up).
fn note_with(ui: &mut Ui, text: String) {
    ui.add(egui::Label::new(RichText::new(text).small().weak()).wrap());
}

#[cfg(test)]
mod tests;

//! File › Options › Assistant › Agents (#24): the ACP agents WordCraft knows (Claude Code, Codex,
//! Pi) and the person's own, each with the command, arguments and environment it starts with.
//! They are saved through the assistant's library in the agents' settings file
//! ([`AgentSettings`]: `assistant-agents.json` in the settings folder, which on Unix only its
//! owner may read), and each says whether it can start, or what to install first: WordCraft
//! never installs an agent.
//!
//! Which program WordCraft starts, and the keys an agent signs in with, are the person's alone:
//! no command reads or writes them, and input the control channel makes (an agent's `click`,
//! `key` or `type`) changes nothing here: the page puts back what it changed and says why. The
//! variables' values are masked (egui's password fields, which never show, copy or expose them
//! as text) and never logged, and nothing here derives `Debug`.
//!
//! Save writes only what the person changed, over the file as it is then: an agent they didn't
//! touch, and each field they didn't change, keep what the file holds, even if it changed while
//! the page was open. It never writes what the library couldn't read back whole, and a file that
//! can't be read is shown as such, and kept aside before a save replaces it.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use egui::accesskit::Role;
use egui::{RichText, Ui, vec2};
use wordcraft_assist::acp::presets::{self, AgentPrefs, AgentSettings, CUSTOM_ID, CUSTOM_KEY, PRESETS, Preset, SET_ASIDE_SUFFIX, SETTINGS_FILE};
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

/// Most agents the settings file keeps (the library's own limit).
const MOST_AGENTS: usize = 16;

/// Most windows whose passes are remembered: more is a leak, and forgetting them only reads the
/// file again.
const MOST_WINDOWS: usize = 64;

/// The agents while File › Options › Assistant shows them: as saved, as edited, and whether
/// each can start. No `Debug`: the variables may hold API keys.
pub(crate) struct AgentSetup {
    /// The settings folder they're saved in.
    dir: PathBuf,
    /// As the file held them when the page read it: nothing set up when it is missing or can't
    /// be read.
    saved: AgentSettings,
    /// The drafts `saved` gives: a field of a draft still as it is here wasn't changed.
    original: Vec<Draft>,
    /// As edited, in the picker's order: the presets, then the custom agent.
    drafts: Vec<Draft>,
    /// What the drafts would save over `saved`, as the file reads it back, or what to fix first:
    /// worked out when they change.
    check: Result<AgentSettings, String>,
    /// Whether each agent can start, as last asked: when the page is shown, after a save, and on
    /// Check again. Asking reads the file and looks programs up on `PATH`, so not every frame.
    pub(crate) offers: Vec<Offer>,
    /// What the last save (or a refused input) did: its message, and whether it failed.
    notice: Option<(String, bool)>,
    /// Why the file can't be read, while it can't: the page says so, and Save keeps it aside
    /// before replacing it.
    damaged: Option<String>,
    /// Part of the fields' ids, new when the control channel's input is put back: the fields
    /// start afresh, with no undo history holding what it typed.
    generation: u64,
    /// The pass each window last drew the page in: egui counts passes per window.
    passes: HashMap<egui::ViewportId, u64>,
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
#[derive(Clone, PartialEq)]
struct Var {
    /// Its own id, never reused while WordCraft runs: its fields, and their undo history, are
    /// this variable's alone, whichever row it is in.
    uid: u64,
    name: String,
    /// Masked on the page.
    value: String,
}

/// A number never handed out before while WordCraft runs (wrapping after 2^64 of them).
fn fresh() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A variable with an id of its own.
fn var(name: String, value: String) -> Var {
    Var { uid: fresh(), name, value }
}

impl AgentSetup {
    fn open(dir: PathBuf, offers: Vec<Offer>) -> AgentSetup {
        let mut setup = AgentSetup {
            dir,
            saved: AgentSettings::default(),
            original: Vec::new(),
            drafts: Vec::new(),
            check: Ok(AgentSettings::default()),
            offers,
            notice: None,
            damaged: None,
            generation: fresh(),
            passes: HashMap::new(),
        };
        setup.reload();
        setup
    }

    /// Read the file again and start from it, edits dropped. A file that can't be read is
    /// nothing set up, and the page says why.
    fn reload(&mut self) {
        let (saved, damaged) = match AgentSettings::read(&self.dir) {
            Ok(saved) => (saved.unwrap_or_default(), None),
            Err(why) => (AgentSettings::default(), Some(why)),
        };
        self.damaged = damaged;
        self.start_from(saved);
    }

    /// Start from `saved`: new drafts, nothing to save.
    fn start_from(&mut self, saved: AgentSettings) {
        self.original = drafts(&saved);
        self.drafts = self.original.clone();
        self.check = Ok(saved.clone());
        self.saved = saved;
    }

    /// Whether a draft differs from what the page read (a blank variable added counts).
    fn touched(&self) -> bool {
        self.drafts != self.original
    }

    /// Whether saving would change the settings.
    fn dirty(&self) -> bool {
        !matches!(&self.check, Ok(s) if *s == self.saved)
    }

    /// The drafts changed: work out what they would save.
    fn edited(&mut self) {
        self.check = settings_over(&self.saved, &self.original, &self.drafts);
        self.notice = None;
    }
}

/// The agents as `saved` sets them up, in the picker's order.
fn drafts(saved: &AgentSettings) -> Vec<Draft> {
    let presets = PRESETS.iter().map(|p| draft(p.key, p.id, Some(*p), saved));
    presets.chain([draft(CUSTOM_KEY, CUSTOM_ID, None, saved)]).collect()
}

fn draft(key: &'static str, id: &'static str, preset: Option<Preset>, saved: &AgentSettings) -> Draft {
    let prefs = saved.agent(key);
    let env = prefs.env.into_iter().map(|(name, value)| var(name, value)).collect();
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

/// What saving `drafts` writes over `base` (the file as it is now): each field changed from
/// `original` (the drafts the page read), and everything else as `base` has it, an agent the page
/// doesn't show included; as the file reads it back. Or what to fix first, which includes
/// settings the file couldn't hold (over 1 MiB, or too many agents).
fn settings_over(base: &AgentSettings, original: &[Draft], drafts: &[Draft]) -> Result<AgentSettings, String> {
    let mut settings = base.clone();
    for d in drafts {
        let o = original.iter().find(|o| o.key == d.key);
        if o == Some(d) {
            continue;
        }
        let prefs = prefs_over(&base.agent(d.key), o, d)?;
        if prefs == AgentPrefs::default() {
            settings.agents.remove(d.key);
        } else {
            settings.agents.insert(d.key.to_string(), prefs);
        }
    }
    // As the file reads it back, so the page shows what it holds (a name loses its control
    // characters, say), and only what the library reads back whole.
    let value = serde_json::to_value(&settings).map_err(|e| e.to_string())?;
    let read = AgentSettings::from_value(&value);
    if read.agents.len() < settings.agents.len() {
        return Err(fmt(tl!("the file keeps at most {n} agents"), &[("n", &MOST_AGENTS.to_string())]));
    }
    if !read.fits() {
        return Err(tl!("the agents' settings would be larger than the 1 MiB their file may hold").to_string());
    }
    Ok(read)
}

/// One agent's settings: each field of `d` changed from `o` (what the page read; `None`: all of
/// them), cleaned up and checked, and the others as `base` has them, untouched. So a field the
/// page can't show as it is (an argument with a line break in it, say) is kept unless changed.
fn prefs_over(base: &AgentPrefs, o: Option<&Draft>, d: &Draft) -> Result<AgentPrefs, String> {
    let agent = draft_label(d);
    let changed = |same: fn(&Draft, &Draft) -> bool| o.is_none_or(|o| !same(o, d));
    let mut prefs = base.clone();
    if changed(|o, d| o.name == d.name) {
        prefs.name = clean(d.name.trim());
    }
    if changed(|o, d| o.command == d.command) {
        prefs.command = clean(d.command.trim());
    }
    if changed(|o, d| o.args == d.args) {
        prefs.args = args_of(d, &agent)?;
    }
    if changed(|o, d| o.env == d.env) {
        prefs.env = env_of(d, &agent)?;
    }
    Ok(prefs)
}

/// `s` without NUL, which no program, argument or variable can hold.
fn clean(s: &str) -> String {
    s.replace('\0', "")
}

/// The arguments, one per line: each trimmed, blank lines left out.
fn args_of(d: &Draft, agent: &str) -> Result<Vec<String>, String> {
    let args: Vec<String> = d.args.lines().map(|l| clean(l.trim())).filter(|l| !l.is_empty()).collect();
    if args.len() > MOST {
        return Err(fmt(tl!("{agent}: at most {n} arguments"), &[("agent", agent), ("n", &MOST.to_string())]));
    }
    Ok(args)
}

/// The variables: names trimmed, blank variables left out, what can't be kept refused.
fn env_of(d: &Draft, agent: &str) -> Result<BTreeMap<String, String>, String> {
    let mut env = BTreeMap::new();
    for v in &d.env {
        let name = clean(v.name.trim());
        if name.is_empty() && v.value.is_empty() {
            continue;
        }
        if name.is_empty() {
            return Err(fmt(tl!("{agent}: a variable has a value but no name"), &[("agent", agent)]));
        }
        if name.contains('=') {
            return Err(fmt(tl!("{agent}: {name}: a variable's name can't hold ="), &[("agent", agent), ("name", &name)]));
        }
        if env.insert(name.clone(), clean(&v.value)).is_some() {
            return Err(fmt(tl!("{agent}: {name} is there twice"), &[("agent", agent), ("name", &name)]));
        }
    }
    if env.len() > MOST {
        return Err(fmt(tl!("{agent}: at most {n} variables"), &[("agent", agent), ("n", &MOST.to_string())]));
    }
    Ok(env)
}

/// Whether the arguments field shows `args` as they are: one per line, each neither empty nor
/// with spaces at its ends, and none with a line break in it.
fn one_per_line(args: &[String]) -> bool {
    args.iter().all(|a| !a.is_empty() && a.trim() == a && !a.contains(['\n', '\r']))
}

/// Whether the page is shown again in the window `viewport`, drawn in its pass `pass`: it wasn't
/// drawn there in the pass before. egui counts passes per window, so each window's are kept
/// apart: a page shown in two windows isn't "shown again" every frame.
fn shown_again(passes: &mut HashMap<egui::ViewportId, u64>, viewport: egui::ViewportId, pass: u64) -> bool {
    let again = passes.get(&viewport).is_none_or(|last| last.saturating_add(1) < pass);
    if passes.len() >= MOST_WINDOWS && !passes.contains_key(&viewport) {
        passes.clear();
    }
    passes.insert(viewport, pass);
    again
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
    let (viewport, pass) = (ui.ctx().viewport_id(), ui.ctx().cumulative_pass_nr());
    let mut setup = match app.assist.setup.take() {
        Some(mut s) if s.dir == dir => {
            if shown_again(&mut s.passes, viewport, pass) {
                // Shown again: the file may have changed (kept while there are edits: Save
                // writes them over the file as it is then), and so may what is installed.
                if !s.touched() {
                    s.reload();
                }
                s.offers = presets::offers(&crate::assist::host(app));
            }
            s
        }
        _ => {
            let mut s = AgentSetup::open(dir, presets::offers(&crate::assist::host(app)));
            shown_again(&mut s.passes, viewport, pass);
            s
        }
    };
    // Only the person changes these settings: what the control channel's input changes in this
    // pass (an agent's click, key or text) is put back.
    let from_control = app.input_from_control(ui.ctx());
    let before = from_control.then(|| setup.drafts.clone());
    let t = Tokens::get(ui.ctx());
    if let Some(why) = &setup.damaged {
        let file = format!("{SETTINGS_FILE}{SET_ASIDE_SUFFIX}");
        let text = fmt(
            tl!("The agents' settings file can't be read ({why}), so nothing in it is shown. Saving keeps it as {file} and starts a new one."),
            &[("why", why), ("file", &file)],
        );
        ui.add(egui::Label::new(RichText::new(text).small().color(t.orange)).wrap());
    }
    let mut edited = false;
    for i in 0..setup.drafts.len() {
        edited |= agent(ui, &t, &mut setup, i);
    }
    ui.add_space(4.0);
    let (mut save, mut discard, mut check) = (false, false, false);
    ui.horizontal(|ui| {
        save = ui.add_enabled(setup.dirty() && setup.check.is_ok(), egui::Button::new(tl!("Save"))).clicked();
        discard = ui.add_enabled(setup.touched(), egui::Button::new(tl!("Discard changes"))).clicked();
        check = ui.button(tl!("Check again")).on_hover_text(tl!("Look for the agents' programs again, after installing one")).clicked();
    });
    if let Err(why) = &setup.check {
        ui.label(RichText::new(fmt(tl!("Can't save: {why}"), &[("why", why)])).small().color(t.orange));
    }
    if let Some((text, failed)) = &setup.notice {
        ui.add(egui::Label::new(RichText::new(text).small().color(if *failed { t.red } else { t.text_dim })).wrap());
    }
    note(ui, "An agent starts with what's saved here the next time it starts; one already running keeps what it started with.");
    if let Some(before) = before {
        if save || discard || setup.drafts != before {
            // Put back, in fields with no undo history of what was typed, and say why.
            setup.drafts = before;
            setup.generation = fresh();
            let why = tl!("Only you can change the agents' settings: type and click here yourself.");
            setup.notice = Some((why.to_string(), true));
            ui.ctx().request_repaint();
        }
        (edited, save, discard) = (false, false, false);
    }
    if edited {
        setup.edited();
    }
    if discard {
        setup.reload();
        setup.notice = None;
    }
    if save {
        save_setup(app, &mut setup);
    }
    if check {
        if !setup.touched() {
            setup.reload();
        }
        ask_again(app, &mut setup);
    }
    app.assist.setup = Some(setup);
}

/// Ask again whether each agent can start, for the page and the pane's picker: once, when the
/// picker lists the agents.
fn ask_again(app: &mut WordApp, setup: &mut AgentSetup) {
    crate::assist::refresh_offers(app);
    let listed = |o: &Offer| o.id == CUSTOM_ID || PRESETS.iter().any(|p| p.id == o.id);
    setup.offers = if app.assist.offers.iter().any(listed) {
        app.assist.offers.iter().filter(|o| listed(o)).cloned().collect()
    } else {
        presets::offers(&crate::assist::host(app))
    };
}

/// Save what the person changed over the file as it is now, through the library, then ask again
/// whether each agent can start (the pane's picker too). A file that can't be read is kept aside
/// first, once the page has said so.
fn save_setup(app: &mut WordApp, setup: &mut AgentSetup) {
    if setup.check.is_err() {
        return;
    }
    let read = AgentSettings::read(&setup.dir);
    let now = match &read {
        Ok(now) => now.clone().unwrap_or_default(),
        Err(why) if setup.damaged.is_none() => {
            // It could be read when the page read it: say so before saving over it.
            let file = format!("{SETTINGS_FILE}{SET_ASIDE_SUFFIX}");
            let text = fmt(
                tl!("Not saved: the agents' settings file can't be read now ({why}). Save again to keep it as {file} and save what's here."),
                &[("why", why), ("file", &file)],
            );
            setup.notice = Some((text, true));
            setup.damaged = Some(why.clone());
            return;
        }
        Err(_) => AgentSettings::default(),
    };
    let settings = match settings_over(&now, &setup.original, &setup.drafts) {
        Ok(settings) => settings,
        Err(why) => {
            setup.notice = Some((fmt(tl!("Not saved: {why}"), &[("why", &why)]), true));
            return;
        }
    };
    let aside = match read {
        Ok(_) => None,
        Err(_) => match AgentSettings::set_aside(&setup.dir) {
            Ok(aside) => aside,
            Err(e) => {
                log::warn!("couldn't keep the damaged agents' settings aside: {e}");
                let text = fmt(tl!("Not saved: the file that can't be read couldn't be kept aside: {error}"), &[("error", &e.to_string())]);
                setup.notice = Some((text, true));
                return;
            }
        },
    };
    match settings.save(&setup.dir) {
        Ok(()) => {
            let said = match &aside {
                Some(aside) => {
                    let file = aside.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                    fmt(tl!("Saved. The file that couldn't be read is kept as {file}."), &[("file", &file)])
                }
                None if now != setup.saved => {
                    tl!("Saved. The file had changed since this page read it: what you didn't change here is as it holds now.").to_string()
                }
                None => tl!("Saved.").to_string(),
            };
            setup.damaged = None;
            setup.start_from(settings);
            setup.notice = Some((said, false));
            ask_again(app, setup);
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
    let generation = setup.generation;
    let shows_all = setup.drafts.get(i).is_none_or(|d| setup.saved.agents.get(d.key).is_none_or(|p| one_per_line(&p.args)));
    let Some(d) = setup.drafts.get_mut(i) else { return false };
    let readiness = offer.as_ref().map(|o| o.readiness.clone());
    let shown = offer.map_or_else(|| draft_label(d), |o| o.label);
    let (word, ready) = match &readiness {
        Some(Readiness::Ready) => ("Ready", true),
        Some(Readiness::Install { .. }) => ("Not found", false),
        Some(Readiness::SignIn { .. }) => ("Sign-in needed", false),
        _ => ("Unavailable", false),
    };
    let ctx = ui.ctx().clone();
    let id = ui.make_persistent_id(("options_agent", d.key));
    let text = fmt(tl!("{agent} · {state}"), &[("agent", &shown), ("state", tl!(word))]);
    let (mut toggled, mut name) = (false, None);
    let row = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
    let mut header = row.show_header(ui, |ui| {
        // The name opens the row too, as the arrow does.
        let label = egui::Label::new(RichText::new(&text).font(semibold(13.0)).color(if ready { t.text } else { t.text_dim }));
        let response = ui.add(label.truncate().sense(egui::Sense::click()));
        toggled = response.clicked();
        name = Some(response.id);
    });
    if toggled {
        header.toggle();
    }
    let open = header.is_open();
    if let Some(name) = name {
        // For assistive technology the name is the row's disclosure, saying whether it is open.
        ctx.accesskit_node_builder(name, |n| {
            n.set_role(Role::DisclosureTriangle);
            n.clear_value();
            n.set_label(text.clone());
            n.set_expanded(open);
        });
    }
    let mut edited = false;
    let (arrow, _, _) = header.body(|ui| {
        let needs = match &readiness {
            Some(Readiness::Install { how }) => Some(fmt(tl!("Install: {how}"), &[("how", how)])),
            Some(Readiness::SignIn { how }) => Some(fmt(tl!("Sign in: {how}"), &[("how", how)])),
            Some(Readiness::Unavailable { why }) => Some(fmt(tl!("Unavailable: {why}"), &[("why", why)])),
            _ => None,
        };
        if let Some(needs) = needs {
            ui.add(egui::Label::new(RichText::new(needs).small().color(t.orange)).wrap());
        }
        edited = fields(ui, t, d, generation, shows_all);
    });
    // The arrow does what the name does: one control for assistive technology, not two.
    ctx.accesskit_node_builder(arrow.id, |n| n.set_hidden());
    edited
}

/// An agent's command, arguments and environment (and the custom agent's name), in fields of
/// this `generation`. `shows_all`: whether the arguments field shows the saved arguments as they
/// are. Whether any changed.
fn fields(ui: &mut Ui, t: &Tokens, d: &mut Draft, generation: u64, shows_all: bool) -> bool {
    let mut edited = false;
    let key = d.key;
    crate::rtl::Table::new("options_agent_fields").columns(2).spacing(vec2(8.0, 6.0)).show(ui, |rows| {
        if d.preset.is_none() {
            rows.row(|r| {
                let label = r.cell(|ui| ui.label(tl!("Name:")));
                r.cell(|ui| {
                    let field = egui::TextEdit::singleline(&mut d.name)
                        .id_salt(("agent_name", key, generation))
                        .desired_width(260.0)
                        .char_limit(MAX_NAME)
                        .hint_text(tl!("Custom agent"));
                    edited |= ui.add(field).labelled_by(label.id).changed();
                });
            });
        }
        rows.row(|r| {
            let label = r.cell(|ui| ui.label(tl!("Command:")));
            r.cell(|ui| {
                let mut field = egui::TextEdit::singleline(&mut d.command)
                    .id_salt(("agent_command", key, generation))
                    .desired_width(380.0)
                    .char_limit(MAX_COMMAND);
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
                // Capped as it is typed or pasted: egui takes only what fits, so text beyond the
                // limit is never laid out, and what is there is never cut.
                let field = egui::TextEdit::multiline(&mut d.args)
                    .id_salt(("agent_args", key, generation))
                    .desired_width(380.0)
                    .desired_rows(2)
                    .char_limit(MAX_ARGS);
                edited |= ui.add_enabled(usable, field).labelled_by(label.id).changed();
            });
        });
    });
    if !shows_all {
        let text = tl!(
            "Some of the saved arguments are empty, have spaces at their ends or hold a line break, so they can't be shown one per line: they are kept as they are unless you change the arguments here."
        );
        ui.add(egui::Label::new(RichText::new(text).small().color(t.orange)).wrap());
    }
    match &d.preset {
        Some(p) => {
            let text = match p.package {
                Some(package) => fmt(
                    tl!(
                        "Leave the command empty to run {program} from PATH (a PATH variable set below comes first), or, when it isn't installed, to download and run {package} with npx, which the Assistant pane asks you to allow each time. Arguments, one per line, go with a command of your own."
                    ),
                    &[("program", p.program), ("package", package)],
                ),
                None => fmt(
                    tl!(
                        "Leave the command empty to run {program} from PATH (a PATH variable set below comes first). Arguments, one per line, go with a command of your own."
                    ),
                    &[("program", p.program)],
                ),
            };
            note_with(ui, text);
        }
        None => note(
            ui,
            "The command is an absolute path, or a program's name looked up on PATH (a PATH variable set below comes first). Arguments: one per line.",
        ),
    }
    ui.add_space(4.0);
    edited |= environment(ui, t, d, generation);
    edited
}

/// An agent's environment variables: names, masked values, Remove, and Add variable. Each
/// variable's fields are its own (by its id, not its row), and its value field and Remove button
/// are named after it.
fn environment(ui: &mut Ui, t: &Tokens, d: &mut Draft, generation: u64) -> bool {
    let mut edited = false;
    ui.label(RichText::new(tl!("Environment")).font(semibold(12.5)));
    let mut remove = None;
    if !d.env.is_empty() {
        let key = d.key;
        let ctx = ui.ctx().clone();
        // Sized, not just wanting a width: a column but the last is as wide as it was.
        let height = ui.spacing().interact_size.y;
        crate::rtl::Table::new("options_agent_env").columns(3).spacing(vec2(8.0, 4.0)).show(ui, |rows| {
            let (name_label, value_label) = rows.row(|r| {
                let name = r.cell(|ui| ui.label(RichText::new(tl!("Name")).small()));
                let value = r.cell(|ui| ui.label(RichText::new(tl!("Value")).small()));
                r.cell(|_| ());
                (name.id, value.id)
            });
            for v in d.env.iter_mut() {
                let uid = v.uid;
                rows.row(|r| {
                    r.cell(|ui| {
                        let field =
                            egui::TextEdit::singleline(&mut v.name).id_salt(("agent_var_name", key, uid, generation)).char_limit(MAX_VAR_NAME);
                        edited |= ui.add_sized(vec2(170.0, height), field).labelled_by(name_label).changed();
                    });
                    // Named after the variable for assistive technology, once it has a name.
                    let named = v.name.trim().to_string();
                    r.cell(|ui| {
                        let field = egui::TextEdit::singleline(&mut v.value)
                            .id_salt(("agent_var_value", key, uid, generation))
                            .password(true)
                            .char_limit(MAX_VALUE);
                        let response = ui.add_sized(vec2(220.0, height), field).labelled_by(value_label);
                        edited |= response.changed();
                        if !named.is_empty() {
                            ctx.accesskit_node_builder(response.id, |n| n.set_label(fmt(tl!("{name} value"), &[("name", &named)])));
                        }
                    });
                    r.cell(|ui| {
                        let button = ui.button(tl!("Remove"));
                        if !named.is_empty() {
                            ctx.accesskit_node_builder(button.id, |n| n.set_label(fmt(tl!("Remove {name}"), &[("name", &named)])));
                        }
                        if button.clicked() {
                            remove = Some(uid);
                        }
                    });
                });
            }
        });
    }
    if let Some(uid) = remove {
        d.env.retain(|v| v.uid != uid);
        edited = true;
    }
    if ui.add_enabled(d.env.len() < MOST, egui::Button::new(tl!("Add variable"))).clicked() {
        d.env.push(var(String::new(), String::new()));
        edited = true;
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

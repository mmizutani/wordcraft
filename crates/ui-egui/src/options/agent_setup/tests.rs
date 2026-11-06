use std::path::{Path, PathBuf};

use egui_kittest::kittest::{By, NodeT, Queryable};
use serde_json::json;
use wordcraft_assist::acp::presets::{AgentPrefs, AgentSettings, SETTINGS_FILE};

use super::{AgentSetup, Draft, Var, drafts, settings_from};
use crate::{Services, WordApp};

/// A secret no node of the page may show.
const SECRET: &str = "s3cret-t0ken-4f9a";

/// A new empty settings folder for a test.
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wordcraft-agent-setup-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// File › Options › Assistant, with `dir` (if any) as the settings folder.
fn harness(dir: Option<&Path>) -> egui_kittest::Harness<'static, WordApp> {
    let services = Services { settings_dir: dir.map(Path::to_path_buf), ..Services::default() };
    let mut a = WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::new()), services);
    a.run("file.options", json!({"pane": "assistant"})).unwrap();
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1440.0, 1400.0)).build_ui_state(
        |ui, app: &mut WordApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        a,
    );
    h.run_steps(4);
    h
}

/// The page's own button called `label`: right of the list of panes, which has a Save, as
/// File's menu does.
fn button<'h>(h: &'h egui_kittest::Harness<'static, WordApp>, label: &'h str) -> egui_kittest::Node<'h> {
    let panes = h.get_by_label("General").rect().right();
    let mut found = h.query_all_by_label(label).filter(|n| n.rect().left() > panes);
    let node = found.next().unwrap_or_else(|| panic!("no {label} button"));
    assert!(found.next().is_none(), "two {label} buttons");
    node
}

/// Open the agent row that names `agent`.
fn open(h: &mut egui_kittest::Harness<'static, WordApp>, agent: &str) {
    h.get_by_label_contains(agent).click();
    h.run_steps(3);
}

/// Type `text` into the field labelled `label` (the `n`th of them).
fn type_into(h: &mut egui_kittest::Harness<'static, WordApp>, label: &str, n: usize, text: &str) {
    h.get_all_by_label(label).nth(n).unwrap_or_else(|| panic!("no field {label} #{n}")).focus();
    h.run_steps(2);
    h.get_all_by_label(label).nth(n).unwrap().type_text(text);
    h.run_steps(2);
}

/// Every node that shows `text`, in its name or its value.
fn showing(h: &egui_kittest::Harness<'static, WordApp>, text: &str) -> usize {
    h.query_all(By::new().predicate(|n| n.label().is_some_and(|l| l.contains(text)) || n.value().is_some_and(|v| v.contains(text)))).count()
}

#[test]
fn an_edit_round_trips_through_the_file_and_a_missing_command_says_what_to_install() {
    let dir = folder("round-trip");
    let mut h = harness(Some(&dir));
    // Nothing set up yet: each agent is listed, the custom one says it isn't set up.
    for agent in ["Claude Code", "Codex", "Pi", "Custom agent"] {
        assert!(h.query_by_label_contains(agent).is_some(), "{agent} is listed");
    }
    assert!(button(&h, "Save").accesskit_node().is_disabled(), "nothing to save yet");
    open(&mut h, "Custom agent");
    assert!(showing(&h, "no custom agent is set up") > 0, "the custom agent says what it needs");
    type_into(&mut h, "Name:", 0, "My Agent");
    type_into(&mut h, "Command:", 0, "/nowhere/my-agent");
    type_into(&mut h, "Arguments:", 0, "--acp");
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    h.get_by_label("Arguments:").type_text("--quiet");
    h.run_steps(2);
    button(&h, "Add variable").click();
    h.run_steps(3);
    type_into(&mut h, "Name", 0, "AGENT_TOKEN");
    type_into(&mut h, "Value", 0, SECRET);
    assert_eq!(showing(&h, SECRET), 0, "the value is masked");
    button(&h, "Save").click();
    h.run_steps(4);

    let custom = AgentPrefs {
        name: "My Agent".into(),
        command: "/nowhere/my-agent".into(),
        args: vec!["--acp".into(), "--quiet".into()],
        env: [("AGENT_TOKEN".to_string(), SECRET.to_string())].into(),
    };
    assert_eq!(AgentSettings::load(&dir).agent("custom"), custom, "saved through the library");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(dir.join(SETTINGS_FILE)).unwrap().permissions().mode() & 0o777, 0o600, "only its owner may read it");
    }
    assert!(h.query_by_label("Saved.").is_some());
    // A command that isn't there: the hint says so, and nothing is installed or started.
    assert!(h.query_by_label_contains("My Agent").is_some(), "the agent is listed under its name");
    assert!(showing(&h, "Install: ") > 0 && showing(&h, "/nowhere/my-agent` wasn't found") > 0, "the hint names the command");
    assert!(button(&h, "Save").accesskit_node().is_disabled(), "nothing more to save");
    assert_eq!(showing(&h, SECRET), 0, "the value is never shown");
    let setup = h.state().assist.setup.as_ref().unwrap();
    assert!(setup.offers.iter().any(|o| o.id == "acp:custom" && o.label == "My Agent"), "asked again after the save");

    // A fresh window shows what is saved, the value still masked; an edit to another agent
    // keeps it.
    drop(h);
    let mut h = harness(Some(&dir));
    open(&mut h, "My Agent");
    assert_eq!(h.get_by_label("Command:").value().as_deref(), Some("/nowhere/my-agent"));
    assert_eq!(h.get_by_label("Arguments:").value().as_deref(), Some("--acp\n--quiet"));
    assert_eq!(h.get_by_label("Name").value().as_deref(), Some("AGENT_TOKEN"));
    assert_eq!(showing(&h, SECRET), 0);
    // Close it, and give Codex an API key.
    open(&mut h, "My Agent");
    open(&mut h, "Codex");
    button(&h, "Add variable").click();
    h.run_steps(3);
    type_into(&mut h, "Name", 0, "OPENAI_API_KEY");
    type_into(&mut h, "Value", 0, "sk-test");
    button(&h, "Save").click();
    h.run_steps(4);
    let saved = AgentSettings::load(&dir);
    assert_eq!(saved.agent("custom"), custom);
    assert_eq!(saved.agent("codex").env.get("OPENAI_API_KEY").map(String::as_str), Some("sk-test"));
    // Removing it saves without it.
    button(&h, "Remove").click();
    h.run_steps(3);
    button(&h, "Save").click();
    h.run_steps(4);
    assert_eq!(AgentSettings::load(&dir).agent("codex"), AgentPrefs::default());
    assert_eq!(AgentSettings::load(&dir).agent("custom"), custom);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn what_cant_be_saved_says_why_and_edits_can_be_discarded() {
    let dir = folder("invalid");
    let mut h = harness(Some(&dir));
    open(&mut h, "Custom agent");
    button(&h, "Add variable").click();
    h.run_steps(3);
    type_into(&mut h, "Value", 0, SECRET);
    assert!(showing(&h, "a variable has a value but no name") > 0, "a value needs a name");
    assert!(button(&h, "Save").accesskit_node().is_disabled());
    type_into(&mut h, "Name", 0, "A=B");
    assert!(showing(&h, "can't hold =") > 0);
    assert!(button(&h, "Save").accesskit_node().is_disabled());
    assert_eq!(showing(&h, SECRET), 0, "no message shows the value");
    // WordCraft's own variables are never taken from here, and the page says so.
    h.get_by_label("Name").focus();
    h.run_steps(1);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.get_by_label("Name").type_text("wordcraft_control_key");
    h.run_steps(3);
    assert!(showing(&h, "itself: what's here is never used") > 0);
    // Discard: back to nothing set up, and nothing was written.
    button(&h, "Discard changes").click();
    h.run_steps(3);
    assert!(h.query_by_label("Name").is_none(), "the variable is gone");
    assert!(!dir.join(SETTINGS_FILE).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn without_a_settings_folder_the_page_says_so() {
    let h = harness(None);
    assert!(h.query_by_label("WordCraft has no settings folder here, so agents can't be set up.").is_some());
    assert!(h.query_by_label_contains("Claude Code").is_none());
    assert!(h.state().assist.setup.is_none());
}

#[test]
fn the_file_is_read_again_when_the_page_is_shown_again() {
    let dir = folder("reread");
    let mut h = harness(Some(&dir));
    assert!(h.query_by_label_contains("Custom agent").is_some());
    h.state_mut().run("file.options", json!({"pane": "general"})).unwrap();
    h.run_steps(3);
    let mut settings = AgentSettings::default();
    settings.agents.insert("custom".into(), AgentPrefs { name: "Hand Made".into(), command: "/nowhere/hand".into(), ..AgentPrefs::default() });
    settings.save(&dir).unwrap();
    h.state_mut().run("file.options", json!({"pane": "assistant"})).unwrap();
    h.run_steps(3);
    assert!(h.query_by_label_contains("Hand Made").is_some(), "the file as it is now");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A draft of every agent with nothing set up, as the page starts.
fn blank() -> Vec<Draft> {
    drafts(&AgentSettings::default())
}

#[test]
fn drafts_save_what_the_file_reads_back_and_hostile_ones_are_refused() {
    // Untouched drafts save what is there, an agent the page doesn't show included.
    let mut saved = AgentSettings::default();
    saved.agents.insert("future-agent".into(), AgentPrefs { command: "x".into(), ..AgentPrefs::default() });
    saved.agents.insert("pi".into(), AgentPrefs { name: "kept".into(), args: vec!["--a".into()], ..AgentPrefs::default() });
    let ds = drafts(&saved);
    assert_eq!(settings_from(&saved, &ds).ok(), Some(saved.clone()));

    // Lines trimmed, blank ones and blank variables left out, NUL taken out.
    let mut ds = blank();
    let custom = ds.iter_mut().find(|d| d.key == "custom").unwrap();
    custom.name = "  Bot\u{7}\u{202E} ".into();
    custom.command = " /opt/bot\u{0} ".into();
    custom.args = "  --one \r\n\n--two\u{0}\n   \n".into();
    custom.env = vec![Var::default(), Var { name: " K ".into(), value: "v\u{0}1".into() }, Var { name: "  ".into(), value: String::new() }];
    let s = settings_from(&AgentSettings::default(), &ds).unwrap();
    let bot = s.agent("custom");
    assert_eq!((bot.name.as_str(), bot.command.as_str()), ("Bot", "/opt/bot"));
    assert_eq!(bot.args, ["--one", "--two"]);
    assert_eq!(bot.env.into_iter().collect::<Vec<_>>(), [("K".to_string(), "v1".to_string())]);
    // What it saves loads back the same.
    let dir = folder("drafts");
    s.save(&dir).unwrap();
    assert_eq!(AgentSettings::load(&dir), s);
    let _ = std::fs::remove_dir_all(&dir);
    // An agent with nothing set up isn't written.
    assert!(!s.agents.contains_key("codex"));

    // What can't be kept is refused, saying what and for which agent.
    let refused = |edit: &dyn Fn(&mut Draft)| {
        let mut ds = blank();
        edit(ds.iter_mut().find(|d| d.key == "codex").unwrap());
        settings_from(&AgentSettings::default(), &ds).err().unwrap_or_default()
    };
    assert!(refused(&|d| d.env = vec![Var { name: String::new(), value: SECRET.into() }]).contains("no name"));
    let e = refused(&|d| d.env = vec![Var { name: "A=B".into(), value: SECRET.into() }]);
    assert!(e.contains("Codex") && e.contains("=") && !e.contains(SECRET), "{e}");
    let twice = refused(&|d| d.env = vec![Var { name: "A".into(), value: "1".into() }, Var { name: " A".into(), value: "2".into() }]);
    assert!(twice.contains("twice"), "{twice}");
    assert!(refused(&|d| d.args = (0..65).map(|i| format!("--a{i}\n")).collect()).contains("64"));
    assert!(refused(&|d| d.env = (0..65).map(|i| Var { name: format!("V{i}"), value: "x".into() }).collect()).contains("64"));
    // At the limits, all of it is kept.
    let mut ds = blank();
    let codex = ds.iter_mut().find(|d| d.key == "codex").unwrap();
    codex.args = (0..64).map(|i| format!("--a{i}\n")).collect();
    codex.env = (0..64).map(|i| Var { name: format!("V{i}"), value: "é".repeat(super::MAX_VALUE) }).collect();
    let s = settings_from(&AgentSettings::default(), &ds).unwrap();
    assert_eq!((s.agent("codex").args.len(), s.agent("codex").env.len()), (64, 64));
}

#[test]
fn a_huge_paste_into_the_arguments_is_cut_to_fit() {
    let dir = folder("huge");
    let mut h = harness(Some(&dir));
    open(&mut h, "Custom agent");
    type_into(&mut h, "Arguments:", 0, &"x".repeat(super::MAX_ARGS + 500));
    let setup: &AgentSetup = h.state().assist.setup.as_ref().unwrap();
    let custom = setup.drafts.iter().find(|d| d.key == "custom").unwrap();
    assert_eq!(custom.args.chars().count(), super::MAX_ARGS);
    let _ = std::fs::remove_dir_all(&dir);
}

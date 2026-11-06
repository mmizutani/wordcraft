use std::path::{Path, PathBuf};

use egui_kittest::kittest::{By, NodeT, Queryable};
use serde_json::json;
use wordcraft_assist::acp::presets::{AgentPrefs, AgentSettings, SETTINGS_FILE};

use super::{AgentSetup, Draft, drafts, settings_over, var};
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
    type_value(&mut h, 0, SECRET);
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
    type_value(&mut h, 0, "sk-test");
    button(&h, "Save").click();
    h.run_steps(4);
    let saved = AgentSettings::load(&dir);
    assert_eq!(saved.agent("custom"), custom);
    assert_eq!(saved.agent("codex").env.get("OPENAI_API_KEY").map(String::as_str), Some("sk-test"));
    // Removing it saves without it.
    button(&h, "Remove OPENAI_API_KEY").click();
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
    assert_eq!(settings_over(&saved, &ds, &ds).ok(), Some(saved.clone()));

    // Lines trimmed, blank ones and blank variables left out, NUL taken out.
    let original = blank();
    let mut ds = original.clone();
    let custom = ds.iter_mut().find(|d| d.key == "custom").unwrap();
    custom.name = "  Bot\u{7}\u{202E} ".into();
    custom.command = " /opt/bot\u{0} ".into();
    custom.args = "  --one \r\n\n--two\u{0}\n   \n".into();
    custom.env = vec![var(String::new(), String::new()), var(" K ".into(), "v\u{0}1".into()), var("  ".into(), String::new())];
    let s = settings_over(&AgentSettings::default(), &original, &ds).unwrap();
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
        let original = blank();
        let mut ds = original.clone();
        edit(ds.iter_mut().find(|d| d.key == "codex").unwrap());
        settings_over(&AgentSettings::default(), &original, &ds).err().unwrap_or_default()
    };
    assert!(refused(&|d| d.env = vec![var(String::new(), SECRET.into())]).contains("no name"));
    let e = refused(&|d| d.env = vec![var("A=B".into(), SECRET.into())]);
    assert!(e.contains("Codex") && e.contains("=") && !e.contains(SECRET), "{e}");
    let twice = refused(&|d| d.env = vec![var("A".into(), "1".into()), var(" A".into(), "2".into())]);
    assert!(twice.contains("twice"), "{twice}");
    assert!(refused(&|d| d.args = (0..65).map(|i| format!("--a{i}\n")).collect()).contains("64"));
    assert!(refused(&|d| d.env = (0..65).map(|i| var(format!("V{i}"), "x".into())).collect()).contains("64"));
    // More than the file may hold (64 values of 8,192 two-byte characters: over 1 MiB) is
    // refused before Save, saying so.
    let big = refused(&|d| d.env = (0..64).map(|i| var(format!("V{i}"), "é".repeat(super::MAX_VALUE))).collect());
    assert!(big.contains("1 MiB"), "{big}");
    // At the limits, all of it is kept while the file can hold it.
    let original = blank();
    let mut ds = original.clone();
    let codex = ds.iter_mut().find(|d| d.key == "codex").unwrap();
    codex.args = (0..64).map(|i| format!("--a{i}\n")).collect();
    codex.env = (0..64).map(|i| var(format!("V{i}"), "é".repeat(super::MAX_VALUE / 2))).collect();
    let s = settings_over(&AgentSettings::default(), &original, &ds).unwrap();
    assert_eq!((s.agent("codex").args.len(), s.agent("codex").env.len()), (64, 64));
    // A file already holding 16 agents takes no more.
    let mut full = AgentSettings::default();
    for i in 0..16 {
        full.agents.insert(format!("other-{i:02}"), AgentPrefs { command: "x".into(), ..AgentPrefs::default() });
    }
    let original = drafts(&full);
    let mut ds = original.clone();
    ds.iter_mut().find(|d| d.key == "pi").unwrap().command = "/opt/pi".into();
    assert!(settings_over(&full, &original, &ds).is_err_and(|e| e.contains("16")));
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

// Review fixes (#24): each test below failed before its fix.

/// Send the control channel's `method` (what MCP's `click`, `key` and `type` send), and run the
/// passes that deliver it, as the window does.
fn faked(h: &mut egui_kittest::Harness<'static, WordApp>, method: &str, params: serde_json::Value) {
    let (req, _rx) = crate::control::ControlRequest::new(method, params);
    let ctx = h.ctx.clone();
    assert!(matches!(crate::control::handle(h.state_mut(), &ctx, &req), crate::control::Outcome::Done(_)), "{method} is queued");
    for _ in 0..20 {
        let ctx = h.ctx.clone();
        let mut raw = std::mem::take(h.input_mut());
        h.state_mut().raw_input_hook(&ctx, &mut raw);
        *h.input_mut() = raw;
        h.step();
    }
}

/// A click the control channel makes at `pos`.
fn faked_click(h: &mut egui_kittest::Harness<'static, WordApp>, pos: egui::Pos2) {
    faked(h, "ui.click", json!({"x": pos.x, "y": pos.y}));
}

/// The draft of the agent with settings key `key`, as the page holds it.
fn draft_of<'h>(h: &'h egui_kittest::Harness<'static, WordApp>, key: &str) -> &'h Draft {
    let setup: &AgentSetup = h.state().assist.setup.as_ref().unwrap();
    setup.drafts.iter().find(|d| d.key == key).unwrap()
}

/// The `n`th node whose role is `role` and whose name passes `name`.
fn nth<'h>(
    h: &'h egui_kittest::Harness<'static, WordApp>,
    role: egui::accesskit::Role,
    n: usize,
    name: impl Fn(&str) -> bool + 'h,
) -> egui_kittest::Node<'h> {
    let found = h.query_all(By::new().predicate(move |node| node.role() == role && node.label().is_some_and(|l| name(&l))));
    found.into_iter().nth(n).unwrap_or_else(|| panic!("no {role:?} #{n}"))
}

/// The `n`th variable's value field (a password field, whatever it is named).
fn value_field<'h>(h: &'h egui_kittest::Harness<'static, WordApp>, n: usize) -> egui_kittest::Node<'h> {
    nth(h, egui::accesskit::Role::PasswordInput, n, |_| true)
}

/// Focus the `n`th variable's value field and type `text`.
fn type_value(h: &mut egui_kittest::Harness<'static, WordApp>, n: usize, text: &str) {
    value_field(h, n).focus();
    h.run_steps(2);
    value_field(h, n).type_text(text);
    h.run_steps(2);
}

/// The `n`th variable's Remove button.
fn remove_button<'h>(h: &'h egui_kittest::Harness<'static, WordApp>, n: usize) -> egui_kittest::Node<'h> {
    nth(h, egui::accesskit::Role::Button, n, |l| l.starts_with("Remove"))
}

/// verified[0]: the control channel's clicks, keys and text (an agent's `click`, `key`, `type`)
/// never change a draft, Save, Discard, Remove or Add variable, and the page says why; nor can
/// the person's undo bring back what it typed.
#[test]
fn control_channel_input_never_changes_the_agents_settings() {
    let dir = folder("faked");
    let mut h = harness(Some(&dir));
    open(&mut h, "Custom agent");
    // An agent clicks the command and types a program of its own.
    let command = h.get_by_label("Command:").rect().center();
    faked_click(&mut h, command);
    faked(&mut h, "ui.text", json!({"text": "/bin/sh"}));
    assert_eq!(draft_of(&h, "custom").command, "", "faked typing changes nothing");
    assert!(showing(&h, "Only you can change the agents' settings") > 0, "and the page says why");
    assert!(button(&h, "Save").accesskit_node().is_disabled());
    // The person's undo in that field can't bring it back.
    h.get_by_label("Command:").focus();
    h.run_steps(2);
    for _ in 0..3 {
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        h.run_steps(2);
    }
    assert!(!draft_of(&h, "custom").command.contains("/bin/sh"), "undo brings back nothing it typed");
    // Nor a faked Add variable.
    let add = button(&h, "Add variable").rect().center();
    faked_click(&mut h, add);
    assert!(draft_of(&h, "custom").env.is_empty(), "a faked Add variable adds nothing");
    // The person's own edits; a faked key, Discard, Remove and Save change none of them.
    type_into(&mut h, "Command:", 0, "/opt/mine");
    button(&h, "Add variable").click();
    h.run_steps(3);
    type_into(&mut h, "Name", 0, "AGENT_TOKEN");
    type_value(&mut h, 0, SECRET);
    h.get_by_label("Command:").focus();
    h.run_steps(2);
    faked(&mut h, "ui.key", json!({"key": "Backspace"}));
    assert_eq!(draft_of(&h, "custom").command, "/opt/mine", "a faked key changes nothing");
    let discard = button(&h, "Discard changes").rect().center();
    faked_click(&mut h, discard);
    assert_eq!(draft_of(&h, "custom").command, "/opt/mine", "a faked Discard drops nothing");
    let remove = remove_button(&h, 0).rect().center();
    faked_click(&mut h, remove);
    assert_eq!(draft_of(&h, "custom").env.len(), 1, "a faked Remove removes nothing");
    let save = button(&h, "Save").rect().center();
    faked_click(&mut h, save);
    assert!(!dir.join(SETTINGS_FILE).exists(), "a faked Save writes nothing");
    // The person's Save saves the person's edits, and nothing the agent typed.
    button(&h, "Save").click();
    h.run_steps(4);
    let saved = AgentSettings::load(&dir).agent("custom");
    assert_eq!(saved.command, "/opt/mine");
    assert_eq!(saved.env.get("AGENT_TOKEN").map(String::as_str), Some(SECRET));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Settings `size` bytes long as the file holds them: an agent the page doesn't show, with long
/// arguments.
fn padded_to(size: usize) -> AgentSettings {
    let with = |args: Vec<String>| {
        let mut s = AgentSettings::default();
        s.agents.insert("future-agent".into(), AgentPrefs { args, ..AgentPrefs::default() });
        s
    };
    let len = |s: &AgentSettings| serde_json::to_vec_pretty(s).unwrap().len();
    // Arguments of 30,000 bytes, then one for what is left: an 'a' adds one byte.
    let mut args = Vec::new();
    loop {
        let mut trial = args.clone();
        trial.push(String::new());
        let left = size - len(&with(trial));
        if left <= 30_000 {
            args.push("a".repeat(left));
            break;
        }
        args.push("a".repeat(30_000));
    }
    let s = with(args);
    assert_eq!(len(&s), size);
    s
}

/// verified[1]: what the library couldn't read back (over the 1 MiB it reads) is refused
/// before Save, saying why, and the file is left as it is.
#[test]
fn settings_larger_than_the_file_may_hold_are_refused() {
    let dir = folder("too-large");
    let near = padded_to(1_048_576 - 4_000);
    near.save(&dir).unwrap();
    let before = std::fs::read(dir.join(SETTINGS_FILE)).unwrap();
    let mut h = harness(Some(&dir));
    open(&mut h, "Codex");
    button(&h, "Add variable").click();
    h.run_steps(3);
    type_into(&mut h, "Name", 0, "BIG");
    type_value(&mut h, 0, &"x".repeat(super::MAX_VALUE));
    assert!(showing(&h, "1 MiB") > 0, "the page says why");
    assert!(button(&h, "Save").accesskit_node().is_disabled(), "and keeps Save off");
    assert_eq!(std::fs::read(dir.join(SETTINGS_FILE)).unwrap(), before);
    let _ = std::fs::remove_dir_all(&dir);
}

/// unverified[0] and verified[1]: a file that can't be read is said to be so, never shown as
/// nothing set up and saved over in silence: Save keeps it aside first.
#[test]
fn a_file_that_cant_be_read_is_said_and_kept_aside_before_a_save_replaces_it() {
    let dir = folder("damaged");
    let file = dir.join(SETTINGS_FILE);
    let aside = dir.join(format!("{SETTINGS_FILE}.damaged"));
    // A hand edit with a trailing comma.
    let junk = r#"{"agents": {"codex": {"env": {"OPENAI_API_KEY": "sk-kept"}},}}"#;
    std::fs::write(&file, junk).unwrap();
    let mut h = harness(Some(&dir));
    assert!(showing(&h, "can't be read") > 0, "the page says the file can't be read");
    open(&mut h, "Custom agent");
    type_into(&mut h, "Command:", 0, "/opt/mine");
    button(&h, "Save").click();
    h.run_steps(4);
    assert_eq!(std::fs::read_to_string(&aside).unwrap(), junk, "what it held is kept aside");
    assert_eq!(AgentSettings::load(&dir).agent("custom").command, "/opt/mine");
    assert!(showing(&h, ".damaged") > 0, "and the page says where");
    assert_eq!(showing(&h, "can't be read ("), 0, "the file can be read now");
    // Damaged after the page read it: the first Save writes nothing and says so; the next one
    // keeps it aside.
    std::fs::write(&file, "junk").unwrap();
    type_into(&mut h, "Command:", 0, "2");
    button(&h, "Save").click();
    h.run_steps(4);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "junk", "not saved over without saying so");
    assert!(showing(&h, "can't be read") > 0);
    button(&h, "Save").click();
    h.run_steps(4);
    assert_eq!(std::fs::read_to_string(&aside).unwrap(), "junk");
    assert_eq!(AgentSettings::load(&dir).agent("custom").command, "/opt/mine2");
    let _ = std::fs::remove_dir_all(&dir);
}

/// verified[2]: a variable's fields belong to it, not to its row: after a row above is removed,
/// undo in a value field never brings back another variable's value.
#[test]
fn undo_never_moves_one_variables_value_into_another() {
    let dir = folder("undo");
    let mut h = harness(Some(&dir));
    open(&mut h, "Custom agent");
    for (n, (name, value)) in [("A", "secret-a"), ("B", "secret-b")].into_iter().enumerate() {
        button(&h, "Add variable").click();
        h.run_steps(3);
        type_into(&mut h, "Name", n, name);
        type_value(&mut h, n, value);
        // Still focused for a while: egui keeps the value as an undo point.
        h.run_steps(8);
    }
    remove_button(&h, 0).click();
    h.run_steps(3);
    // B is the first row now.
    value_field(&h, 0).focus();
    h.run_steps(2);
    for undo in 1..=3 {
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        h.run_steps(2);
        let env = &draft_of(&h, "custom").env;
        assert_eq!(env.len(), 1);
        assert!(env.iter().all(|v| v.name == "B" && v.value != "secret-a"), "undo {undo}: A's value never lands in B");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// verified[3] and unverified[1]: Save writes what the person changed over the file as it is
/// now, so a change made to it meanwhile (by hand, or by another WordCraft) is kept.
#[test]
fn save_keeps_what_changed_in_the_file_meanwhile() {
    let dir = folder("meanwhile");
    let mut first = AgentSettings::default();
    first.agents.insert("custom".into(), AgentPrefs { command: "/opt/old/agent".into(), ..AgentPrefs::default() });
    first
        .agents
        .insert("codex".into(), AgentPrefs { env: [("OPENAI_API_KEY".to_string(), "sk-revoked".to_string())].into(), ..AgentPrefs::default() });
    first.save(&dir).unwrap();
    let mut h = harness(Some(&dir));
    open(&mut h, "Pi");
    button(&h, "Add variable").click();
    h.run_steps(3);
    type_into(&mut h, "Name", 0, "PI_MODE");
    type_value(&mut h, 0, "fast");
    // Meanwhile, by hand: a new command, and the revoked key gone.
    let mut now = AgentSettings::default();
    now.agents.insert("custom".into(), AgentPrefs { command: "/opt/new/agent".into(), ..AgentPrefs::default() });
    now.save(&dir).unwrap();
    button(&h, "Save").click();
    h.run_steps(4);
    let saved = AgentSettings::load(&dir);
    assert_eq!(saved.agent("custom").command, "/opt/new/agent", "the file's change is kept");
    assert!(saved.agent("codex").env.is_empty(), "the revoked key stays gone");
    assert_eq!(saved.agent("pi").env.get("PI_MODE").map(String::as_str), Some("fast"), "the person's edit is saved");
    assert!(showing(&h, "had changed") > 0, "and the page says the file had changed");
    let _ = std::fs::remove_dir_all(&dir);
}

/// verified[4]: the arguments are capped as they are typed, so text typed into full arguments
/// is refused instead of cutting their end, and arguments longer than that from the file aren't
/// shortened by a keystroke.
#[test]
fn typing_into_full_arguments_keeps_their_end() {
    for len in [super::MAX_ARGS, 10_000] {
        let dir = folder(&format!("full-args-{len}"));
        let arg = format!("{}END", "a".repeat(len - 3));
        let mut s = AgentSettings::default();
        s.agents.insert("custom".into(), AgentPrefs { command: "/opt/a".into(), args: vec![arg.clone()], ..AgentPrefs::default() });
        s.save(&dir).unwrap();
        let mut h = harness(Some(&dir));
        open(&mut h, "Custom agent");
        h.get_by_label("Arguments:").focus();
        h.run_steps(2);
        h.key_press(egui::Key::Home);
        h.run_steps(1);
        h.get_by_label("Arguments:").type_text("zz");
        h.run_steps(2);
        let args = &draft_of(&h, "custom").args;
        assert!(args.ends_with("END"), "{len}: the end is kept");
        assert_eq!(args.chars().count(), len, "{len}: nothing is added past the limit, nor cut");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// verified[5]: saving one agent writes nothing else: an agent not touched keeps the file's
/// entry as it is, even arguments the page can't show one per line, and so does each field
/// of an agent that wasn't changed.
#[test]
fn saving_one_agent_keeps_the_others_as_the_file_has_them() {
    let dir = folder("untouched");
    let mut first = AgentSettings::default();
    let custom = AgentPrefs {
        command: "/usr/local/bin/my-agent".into(),
        args: vec!["--system".into(), "Be brief.\nCite sources.".into(), "--tag".into(), String::new(), "  spaced ".into()],
        env: [("A".to_string(), "1".to_string()), ("A ".to_string(), "2".to_string())].into(),
        ..AgentPrefs::default()
    };
    first.agents.insert("custom".into(), custom.clone());
    first.save(&dir).unwrap();
    let mut h = harness(Some(&dir));
    open(&mut h, "Codex");
    button(&h, "Add variable").click();
    h.run_steps(3);
    type_into(&mut h, "Name", 0, "OPENAI_API_KEY");
    type_value(&mut h, 0, "sk-test");
    button(&h, "Save").click();
    h.run_steps(4);
    let saved = AgentSettings::load(&dir);
    assert_eq!(saved.agent("codex").env.get("OPENAI_API_KEY").map(String::as_str), Some("sk-test"));
    assert_eq!(saved.agent("custom"), custom, "the custom agent is as it was");
    // Changing only its command keeps its arguments and environment as they are; the page says
    // its arguments can't be shown one per line.
    open(&mut h, "Codex");
    open(&mut h, "Custom agent");
    assert!(showing(&h, "one per line") > 0);
    // A keystroke undone is no change.
    h.get_by_label("Arguments:").focus();
    h.run_steps(2);
    h.get_by_label("Arguments:").type_text("z");
    h.run_steps(2);
    h.key_press(egui::Key::Backspace);
    h.run_steps(2);
    assert!(button(&h, "Save").accesskit_node().is_disabled(), "nothing changed");
    type_into(&mut h, "Command:", 0, "-2");
    button(&h, "Save").click();
    h.run_steps(4);
    let saved = AgentSettings::load(&dir).agent("custom");
    assert_eq!(saved.command, "/usr/local/bin/my-agent-2");
    assert_eq!((saved.args, saved.env), (custom.args, custom.env));
    let _ = std::fs::remove_dir_all(&dir);
}

/// unverified[2]: shown again, the page shows the file as it is now in its fields (not just the
/// row's name), and edits not saved are kept.
#[test]
fn the_fields_show_the_file_again_and_edits_not_saved_are_kept() {
    let dir = folder("reread-fields");
    let mut h = harness(Some(&dir));
    h.state_mut().run("file.options", json!({"pane": "general"})).unwrap();
    h.run_steps(3);
    let mut settings = AgentSettings::default();
    settings.agents.insert("custom".into(), AgentPrefs { name: "Hand Made".into(), command: "/nowhere/hand".into(), ..AgentPrefs::default() });
    settings.save(&dir).unwrap();
    h.state_mut().run("file.options", json!({"pane": "assistant"})).unwrap();
    h.run_steps(3);
    open(&mut h, "Hand Made");
    assert_eq!(h.get_by_label("Command:").value().as_deref(), Some("/nowhere/hand"));
    assert_eq!(h.get_by_label("Name:").value().as_deref(), Some("Hand Made"));
    // An edit not saved: away and back, with the file changed meanwhile, it is still there.
    type_into(&mut h, "Command:", 0, "-2");
    h.state_mut().run("file.options", json!({"pane": "general"})).unwrap();
    h.run_steps(3);
    settings.agents.insert("pi".into(), AgentPrefs { command: "/nowhere/pi".into(), ..AgentPrefs::default() });
    settings.save(&dir).unwrap();
    h.state_mut().run("file.options", json!({"pane": "assistant"})).unwrap();
    h.run_steps(3);
    assert_eq!(h.get_by_label("Command:").value().as_deref(), Some("/nowhere/hand-2"), "the edit is kept");
    assert!(!button(&h, "Save").accesskit_node().is_disabled());
    button(&h, "Save").click();
    h.run_steps(4);
    let saved = AgentSettings::load(&dir);
    assert_eq!(saved.agent("custom").command, "/nowhere/hand-2");
    assert_eq!(saved.agent("pi").command, "/nowhere/pi", "and the file's own change is kept");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The second window of [`each_window_counts_its_own_passes`].
fn second_window() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("second window")
}

/// One pass of a window (`viewport`), drawing the page or not. The first window keeps the
/// second one open, as WordCraft's windows are.
fn pass(ctx: &egui::Context, app: &mut WordApp, viewport: egui::ViewportId, page: bool) {
    let mut raw = egui::RawInput { viewport_id: viewport, ..egui::RawInput::default() };
    raw.viewports.insert(viewport, egui::ViewportInfo::default());
    let mut out = ctx.run_ui(raw, |ui| {
        if viewport == egui::ViewportId::ROOT {
            ui.ctx().show_viewport_deferred(second_window(), egui::ViewportBuilder::default(), |_, _| ());
        }
        if page {
            super::show(app, ui);
        }
    });
    out.textures_delta.clear();
}

/// unverified[5]: each window counts its own passes, so a page shown in two windows isn't read
/// again every frame, and is read again when a window shows it again.
#[test]
fn each_window_counts_its_own_passes() {
    let dir = folder("windows");
    let services = Services { settings_dir: Some(dir.clone()), ..Services::default() };
    let mut a = WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::new()), services);
    let ctx = egui::Context::default();
    ctx.set_embed_viewports(false);
    let (first, second) = (egui::ViewportId::ROOT, second_window());
    for _ in 0..5 {
        pass(&ctx, &mut a, first, true);
    }
    for _ in 0..3 {
        pass(&ctx, &mut a, second, true);
        pass(&ctx, &mut a, first, true);
    }
    let mut settings = AgentSettings::default();
    settings.agents.insert("custom".into(), AgentPrefs { name: "Changed".into(), ..AgentPrefs::default() });
    settings.save(&dir).unwrap();
    let name = |a: &WordApp| a.assist.setup.as_ref().unwrap().drafts.iter().find(|d| d.key == "custom").unwrap().name.clone();
    for _ in 0..3 {
        pass(&ctx, &mut a, second, true);
        pass(&ctx, &mut a, first, true);
    }
    assert_eq!(name(&a), "", "both windows kept showing it: not read again");
    // The second window shows something else, then the page again: read again there.
    pass(&ctx, &mut a, second, false);
    pass(&ctx, &mut a, first, true);
    pass(&ctx, &mut a, second, true);
    assert_eq!(name(&a), "Changed");
    let _ = std::fs::remove_dir_all(&dir);
}

/// unverified[6]: each agent's row is a disclosure that says whether it is open, and each
/// variable's value field and Remove button name the variable.
#[test]
fn rows_say_whether_they_are_open_and_variables_are_named() {
    use egui::accesskit::Role;
    let dir = folder("a11y");
    let mut settings = AgentSettings::default();
    let env = [("OPENAI_API_KEY".to_string(), "sk-a".to_string()), ("CODEX_API_KEY".to_string(), "sk-b".to_string())].into();
    settings.agents.insert("codex".into(), AgentPrefs { env, ..AgentPrefs::default() });
    settings.save(&dir).unwrap();
    let mut h = harness(Some(&dir));
    let row = h.get_by_label_contains("Codex ·").accesskit_node();
    assert_eq!((row.role(), row.data().is_expanded()), (Role::DisclosureTriangle, Some(false)));
    open(&mut h, "Codex ·");
    assert_eq!(h.get_by_label_contains("Codex ·").accesskit_node().data().is_expanded(), Some(true));
    for name in ["OPENAI_API_KEY", "CODEX_API_KEY"] {
        // Panics when no value field is named after the variable.
        nth(&h, Role::PasswordInput, 0, move |l| l == format!("{name} value"));
        assert!(h.query_by_label(&format!("Remove {name}")).is_some(), "Remove names {name}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

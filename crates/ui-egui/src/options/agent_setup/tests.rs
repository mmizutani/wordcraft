use std::path::{Path, PathBuf};

use egui_kittest::kittest::{By, NodeT, Queryable};
use serde_json::json;
use wordcraft_assist::acp::presets::{AgentPrefs, AgentSettings, SETTINGS_FILE};

use super::{AgentSetup, Draft, drafts, env_over, settings_over, var, written};
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
    faked_for(h, method, params, 20);
}

/// Input the control channel makes, then `steps` passes.
fn faked_for(h: &mut egui_kittest::Harness<'static, WordApp>, method: &str, params: serde_json::Value, steps: usize) {
    let (req, _rx) = crate::control::ControlRequest::new(method, params);
    let ctx = h.ctx.clone();
    assert!(matches!(crate::control::handle(h.state_mut(), &ctx, &req), crate::control::Outcome::Done(_)), "{method} is queued");
    for _ in 0..steps {
        let ctx = h.ctx.clone();
        let mut raw = std::mem::take(h.input_mut());
        h.state_mut().raw_input_hook(&ctx, &mut raw);
        *h.input_mut() = raw;
        h.step();
    }
}

/// The value of the custom agent's first variable, as the page holds it.
fn first_value(h: &egui_kittest::Harness<'static, WordApp>) -> String {
    draft_of(h, "custom").env.first().map(|v| v.value.clone()).unwrap_or_default()
}

/// Whether any widget has keyboard focus.
fn focused(h: &egui_kittest::Harness<'static, WordApp>) -> bool {
    h.ctx.memory(|m| m.focused()).is_some()
}

/// The person's own click at `pos`: a press and, a pass later, its release.
fn person_click(h: &mut egui_kittest::Harness<'static, WordApp>, pos: egui::Pos2) {
    h.event(egui::Event::PointerMoved(pos));
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run_steps(1);
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run_steps(1);
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

// Second review (#24): each test below failed before its fix.

/// The settings file in `dir`, as JSON, all of it.
fn raw(dir: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(dir.join(SETTINGS_FILE)).unwrap()).unwrap()
}

/// confirmed[1] and [3]: a file that goes bad after the page read it is kept aside on the second
/// Save, which saves what the page shows (its last good read and the edits), not the edits alone.
#[test]
fn a_file_damaged_after_the_page_read_it_saves_what_the_page_shows() {
    let dir = folder("damaged-later");
    let file = dir.join(SETTINGS_FILE);
    let mut first = AgentSettings::default();
    first.agents.insert("codex".into(), AgentPrefs { env: [("OPENAI_API_KEY".to_string(), "sk-kept".to_string())].into(), ..AgentPrefs::default() });
    let custom = AgentPrefs {
        command: "/opt/mine".into(),
        args: vec!["--acp".into()],
        env: [("AGENT_TOKEN".to_string(), "tok".to_string())].into(),
        ..AgentPrefs::default()
    };
    first.agents.insert("custom".into(), custom.clone());
    // With what the page doesn't show: a key outside the agents and a field it doesn't read.
    let mut value = serde_json::to_value(&first).unwrap();
    value["version"] = json!(2);
    value["agents"]["codex"]["extra"] = json!(true);
    std::fs::write(&file, value.to_string()).unwrap();
    let mut h = harness(Some(&dir));
    open(&mut h, "Custom agent");
    type_into(&mut h, "Command:", 0, "2");
    // An editor leaves it empty.
    std::fs::write(&file, "").unwrap();
    button(&h, "Save").click();
    h.run_steps(4);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "", "not saved over without saying so");
    button(&h, "Save").click();
    h.run_steps(4);
    let saved = AgentSettings::load(&dir);
    assert_eq!(saved.agent("codex").env.get("OPENAI_API_KEY").map(String::as_str), Some("sk-kept"), "an agent not touched is kept");
    assert_eq!(saved.agent("custom"), AgentPrefs { command: "/opt/mine2".into(), ..custom.clone() }, "and the edited agent's other fields");
    assert!(draft_of(&h, "codex").env.iter().any(|v| v.name == "OPENAI_API_KEY"), "the page still shows it");
    let raw: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!((&raw["version"], &raw["agents"]["codex"]["extra"]), (&json!(2), &json!(true)), "and what the page doesn't show");
    // While it can't be read, the page says it shows the file as it was when read.
    type_into(&mut h, "Command:", 0, "3");
    std::fs::write(&file, "junk").unwrap();
    button(&h, "Save").click();
    h.run_steps(4);
    assert!(showing(&h, "as it was when") > 0, "the page shows its last good read and says so");
    assert_eq!(showing(&h, "nothing in it is shown"), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// confirmed[2]: Save merges an agent's variables one by one with the file as it is then: a
/// variable the file added, changed or removed meanwhile stays that way, and the person's own
/// changes are applied.
#[test]
fn save_merges_each_variable_with_the_file() {
    let dir = folder("merge-vars");
    let env = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let write = |pairs: &[(&str, &str)]| {
        let mut s = AgentSettings::default();
        s.agents.insert("codex".into(), AgentPrefs { env: env(pairs), ..AgentPrefs::default() });
        s.save(&dir).unwrap();
    };
    write(&[("A", "1")]);
    let mut h = harness(Some(&dir));
    open(&mut h, "Codex");
    // The page adds B; meanwhile the file loses A and gains C.
    button(&h, "Add variable").click();
    h.run_steps(3);
    type_into(&mut h, "Name", 1, "B");
    type_value(&mut h, 1, "2");
    write(&[("C", "3")]);
    button(&h, "Save").click();
    h.run_steps(4);
    assert_eq!(AgentSettings::load(&dir).agent("codex").env, env(&[("B", "2"), ("C", "3")]), "C kept, A not brought back");
    // The page removes A; meanwhile the file changes B and gains D.
    write(&[("A", "1"), ("B", "2")]);
    button(&h, "Check again").click();
    h.run_steps(4);
    button(&h, "Remove A").click();
    h.run_steps(3);
    write(&[("A", "1"), ("B", "3"), ("D", "4")]);
    button(&h, "Save").click();
    h.run_steps(4);
    assert_eq!(AgentSettings::load(&dir).agent("codex").env, env(&[("B", "3"), ("D", "4")]), "A removed, B and D as the file has them");
    let _ = std::fs::remove_dir_all(&dir);
}

/// confirmed[2]: variables merge one by one: a rename takes the old name out, two swapped names
/// keep both values, an unchanged variable is as the file has it now, and a clash is refused.
#[test]
fn variables_merge_one_by_one_with_renames_and_swaps() {
    let map = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<std::collections::BTreeMap<_, _>>();
    let read = vec![var("A".into(), "1".into()), var("B".into(), "2".into()), var("K".into(), "k".into())];
    let mut page = read.clone();
    // Swap A and B's names; K unchanged (the file has changed it since), and the file gained Z.
    page[0].name = "B".into();
    page[1].name = "A".into();
    let file = map(&[("A", "1"), ("B", "2"), ("K", "k2"), ("Z", "z")]);
    assert_eq!(env_over(&file, &read, &page, "x").unwrap(), map(&[("A", "2"), ("B", "1"), ("K", "k2"), ("Z", "z")]));
    // A rename takes the old name out; a rename onto another row's name is refused.
    let mut page = read.clone();
    page[0].name = " C ".into();
    assert_eq!(env_over(&file, &read, &page, "x").unwrap(), map(&[("B", "2"), ("C", "1"), ("K", "k2"), ("Z", "z")]));
    page[0].name = "K".into();
    assert!(env_over(&file, &read, &page, "x").unwrap_err().contains("K is there twice"));
    // Blanking a variable removes it.
    let mut page = read.clone();
    page[2].name.clear();
    page[2].value.clear();
    assert_eq!(env_over(&file, &read, &page, "x").unwrap(), map(&[("A", "1"), ("B", "2"), ("Z", "z")]));
}

/// Control-channel input lets go of keyboard focus and of the focused field's selection: a
/// faked click, Tab, Shift+Tab or select-all never leaves the person's next keys going into a
/// field (or button) it picked, nor replacing their text; the person clicks their field again.
/// (Second and fourth reviews: before, an agent's click left focus in the plain Command field.)
#[test]
fn control_channel_input_lets_go_of_focus_and_selection() {
    let dir = folder("faked-focus");
    let mut h = harness(Some(&dir));
    open(&mut h, "Custom agent");
    button(&h, "Add variable").click();
    h.run_steps(3);
    type_into(&mut h, "Name", 0, "API_KEY");
    type_value(&mut h, 0, "sk-");
    // A faked click on the Command field, then the person goes on typing their key.
    let command = h.get_by_label("Command:").rect().center();
    faked_click(&mut h, command);
    assert!(showing(&h, "Only you can change the agents' settings") > 0, "the page says why");
    assert!(!focused(&h), "nothing has focus");
    h.event(egui::Event::Text(SECRET.into()));
    h.run_steps(2);
    assert!(!draft_of(&h, "custom").command.contains(SECRET), "the key never lands in the command");
    assert_eq!(first_value(&h), "sk-", "nor anywhere: the person clicks their field again");
    type_value(&mut h, 0, SECRET);
    assert_eq!(first_value(&h), format!("sk-{SECRET}"), "and goes on there");
    // A faked Tab onto Remove, then the person's Space: nothing is removed.
    faked(&mut h, "ui.key", json!({"key": "Tab"}));
    h.key_press(egui::Key::Space);
    h.run_steps(2);
    assert_eq!(draft_of(&h, "custom").env.len(), 1, "Space didn't press Remove");
    // A faked Shift+Tab (egui moves focus at the next pass): the person's next characters go
    // nowhere, not into the variable's name.
    type_value(&mut h, 0, "");
    faked(&mut h, "ui.key", json!({"key": "Tab", "shift": true}));
    assert!(!focused(&h), "nothing has focus");
    h.event(egui::Event::Text("x".into()));
    h.run_steps(2);
    assert_eq!(draft_of(&h, "custom").env.first().map(|v| v.name.as_str()), Some("API_KEY"), "the name is as typed");
    // A faked select-all in the person's field: their next character replaces nothing, and
    // back in the field, they type on at its end.
    type_value(&mut h, 0, "");
    faked(&mut h, "ui.key", json!({"key": "a", "cmd": true}));
    h.event(egui::Event::Text("Z".into()));
    h.run_steps(2);
    assert_eq!(first_value(&h), format!("sk-{SECRET}"), "the key isn't replaced");
    type_value(&mut h, 0, "Z");
    assert_eq!(first_value(&h), format!("sk-{SECRET}Z"), "no selection is left");
    let _ = std::fs::remove_dir_all(&dir);
}

/// confirmed[5]: Save keeps what the page doesn't show: fields and values WordCraft doesn't read,
/// and keys outside the agents, as the file has them.
#[test]
fn saving_keeps_what_the_page_does_not_show() {
    let dir = folder("unknown-fields");
    let file = json!({
        "version": 2,
        "agents": {
            "pi": {"command": "/opt/pi", "cwd": "/work", "env": {"PORT": 8080, "OK": "1"}},
            "codex": {"env": {"N": 5}, "extra": true}
        }
    });
    std::fs::write(dir.join(SETTINGS_FILE), serde_json::to_vec_pretty(&file).unwrap()).unwrap();
    let mut h = harness(Some(&dir));
    open(&mut h, "Codex");
    button(&h, "Add variable").click();
    h.run_steps(3);
    type_into(&mut h, "Name", 0, "OPENAI_API_KEY");
    type_value(&mut h, 0, "sk-test");
    button(&h, "Save").click();
    h.run_steps(4);
    let saved = raw(&dir);
    assert_eq!(saved["version"], 2, "a key outside the agents is kept");
    assert_eq!(saved["agents"]["pi"], file["agents"]["pi"], "an agent not touched is as the file has it");
    assert_eq!(saved["agents"]["codex"], json!({"env": {"N": 5, "OPENAI_API_KEY": "sk-test"}, "extra": true}));
    let _ = std::fs::remove_dir_all(&dir);
}

/// confirmed[6]: what the control channel typed into a field the person was typing in can't be
/// brought back by the person's undo, even when egui had kept it as an undo point (typing
/// without a pause for 30 seconds makes one): the field is a new one.
#[test]
fn text_the_control_channel_typed_cant_come_back_with_undo() {
    let dir = folder("faked-undo");
    let mut h = harness(Some(&dir));
    open(&mut h, "Custom agent");
    h.get_by_label("Command:").focus();
    h.run_steps(2);
    // The person types a character every half second for 29.5 seconds (a step is 0.25 s)…
    for _ in 0..60 {
        h.event(egui::Event::Text("a".into()));
        h.step();
        h.step();
    }
    // …and the control channel's text lands at 30 seconds, as egui makes it an undo point.
    faked(&mut h, "ui.text", json!({"text": "/bin/sh"}));
    assert_eq!(draft_of(&h, "custom").command, "a".repeat(60), "put back");
    h.get_by_label("Command:").focus();
    h.run_steps(2);
    for undo in 1..=3 {
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        h.run_steps(2);
        assert!(!draft_of(&h, "custom").command.contains("/bin/sh"), "undo {undo} doesn't bring it back");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// confirmed[6]: Discard changes, and Check again with nothing changed, read the file again.
#[test]
fn discard_and_check_again_read_the_file_again() {
    let dir = folder("reread-buttons");
    let write = |command: &str| {
        let mut s = AgentSettings::default();
        s.agents.insert("custom".into(), AgentPrefs { command: command.into(), ..AgentPrefs::default() });
        s.save(&dir).unwrap();
    };
    write("/opt/a");
    let mut h = harness(Some(&dir));
    open(&mut h, "Custom agent");
    write("/opt/b");
    button(&h, "Check again").click();
    h.run_steps(4);
    assert_eq!(h.get_by_label("Command:").value().as_deref(), Some("/opt/b"), "Check again with nothing changed");
    type_into(&mut h, "Command:", 0, "-edited");
    write("/opt/c");
    button(&h, "Check again").click();
    h.run_steps(4);
    assert_eq!(h.get_by_label("Command:").value().as_deref(), Some("/opt/b-edited"), "Check again keeps edits");
    button(&h, "Discard changes").click();
    h.run_steps(4);
    assert_eq!(h.get_by_label("Command:").value().as_deref(), Some("/opt/c"), "Discard changes");
    let _ = std::fs::remove_dir_all(&dir);
}

/// confirmed[4]: the notes say what a PATH variable does: it replaces WordCraft's PATH for the
/// agent (the library's `a_configured_path_replaces_the_inherited_one`).
#[test]
fn the_notes_say_a_path_variable_replaces_wordcrafts() {
    let dir = folder("path-note");
    let mut h = harness(Some(&dir));
    open(&mut h, "Codex");
    open(&mut h, "Custom agent");
    // Every open row's note (a label, whose wrapped rows are its children): Codex's and the
    // custom agent's.
    let notes = |text: &str| {
        let text = text.to_string();
        h.query_all(By::new().role(egui::accesskit::Role::Label).predicate(move |n| n.value().is_some_and(|v| v.contains(&text)))).count()
    };
    let open = notes("from PATH") + notes("looked up on PATH");
    assert_eq!(open, 2, "Codex's and the custom agent's notes are open");
    assert_eq!(notes("replaces WordCraft's own PATH"), open, "each note says it");
    assert_eq!(showing(&h, "comes first"), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

// Third review (#24): each test below failed before its fix.

/// A file holding codex's key, a custom agent and a large array WordCraft doesn't read: compact,
/// it fits the 1 MiB the file may hold; pretty-printed, it doesn't.
fn with_a_large_log(dir: &Path) -> AgentPrefs {
    let custom = AgentPrefs { command: "/opt/mine".into(), args: vec!["--acp".into()], ..AgentPrefs::default() };
    let file = json!({
        "agents": {"codex": {"env": {"OPENAI_API_KEY": "sk-kept"}}, "custom": {"command": "/opt/mine", "args": ["--acp"]}},
        "log": vec![0; 300_000],
    });
    std::fs::write(dir.join(SETTINGS_FILE), file.to_string()).unwrap();
    custom
}

/// confirmed[0]: a Save over a file damaged after the page read it, whose write fails, loses
/// nothing: the file is put back, and the next Save writes the page's last good read.
#[test]
fn a_save_over_a_damaged_file_that_fails_loses_nothing() {
    let dir = folder("damaged-write-fails");
    let file = dir.join(SETTINGS_FILE);
    let custom = with_a_large_log(&dir);
    let mut h = harness(Some(&dir));
    open(&mut h, "Custom agent");
    type_into(&mut h, "Command:", 0, "2");
    std::fs::write(&file, "").unwrap();
    for _ in 0..3 {
        button(&h, "Save").click();
        h.run_steps(4);
    }
    let saved = AgentSettings::load(&dir);
    assert_eq!(saved.agent("codex").env.get("OPENAI_API_KEY").map(String::as_str), Some("sk-kept"), "an agent not touched is kept");
    assert_eq!(saved.agent("custom"), AgentPrefs { command: "/opt/mine2".into(), ..custom.clone() });
    assert!(draft_of(&h, "codex").env.iter().any(|v| v.name == "OPENAI_API_KEY"), "the page still shows it");
    // A write that fails after the damaged file was set aside puts it back (the new file's
    // temporary name is taken by a folder, so it can't be made).
    type_into(&mut h, "Command:", 0, "3");
    std::fs::write(&file, "junk").unwrap();
    button(&h, "Save").click();
    h.run_steps(4);
    let blocker = dir.join(format!(".{SETTINGS_FILE}.{}.tmp", std::process::id()));
    std::fs::create_dir_all(blocker.join("x")).unwrap();
    button(&h, "Save").click();
    h.run_steps(4);
    assert!(showing(&h, "Couldn't save") > 0, "the write failed");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "junk", "the damaged file is back");
    std::fs::remove_dir_all(&blocker).unwrap();
    button(&h, "Save").click();
    h.run_steps(4);
    let saved = AgentSettings::load(&dir);
    assert_eq!(saved.agent("codex").env.get("OPENAI_API_KEY").map(String::as_str), Some("sk-kept"), "kept after a failed write");
    assert_eq!(saved.agent("custom").command, "/opt/mine23");
    // A file that went bad and then went missing: what the page shows is saved, not just edits.
    type_into(&mut h, "Command:", 0, "4");
    std::fs::write(&file, "junk").unwrap();
    button(&h, "Save").click();
    h.run_steps(4);
    std::fs::remove_file(&file).unwrap();
    button(&h, "Save").click();
    h.run_steps(4);
    let saved = AgentSettings::load(&dir);
    assert_eq!(saved.agent("codex").env.get("OPENAI_API_KEY").map(String::as_str), Some("sk-kept"), "kept when the file went missing");
    assert_eq!(saved.agent("custom").command, "/opt/mine234");
    let _ = std::fs::remove_dir_all(&dir);
}

/// confirmed[1]: what Save would write, the file's data the page doesn't show included, is
/// measured: a file that only pretty-printing takes over 1 MiB saves, and one that the edits
/// take over it keeps Save off and says why.
#[test]
fn save_measures_what_it_writes() {
    let dir = folder("measured");
    with_a_large_log(&dir);
    let mut h = harness(Some(&dir));
    open(&mut h, "Codex");
    type_value(&mut h, 0, "-new");
    button(&h, "Save").click();
    h.run_steps(4);
    assert_eq!(AgentSettings::load(&dir).agent("codex").env.get("OPENAI_API_KEY").map(String::as_str), Some("sk-kept-new"), "saved");
    let raw: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join(SETTINGS_FILE)).unwrap()).unwrap();
    assert_eq!(raw["log"].as_array().map(Vec::len), Some(300_000), "with what the page doesn't show");
    // Near the limit with data the page doesn't show: a long value would take it over.
    let file = json!({"agents": {}, "pad": "x".repeat(1_048_576 - 2_000)});
    std::fs::write(dir.join(SETTINGS_FILE), file.to_string()).unwrap();
    let before = std::fs::read(dir.join(SETTINGS_FILE)).unwrap();
    button(&h, "Check again").click();
    h.run_steps(4);
    button(&h, "Add variable").click();
    h.run_steps(3);
    type_into(&mut h, "Name", 0, "BIG");
    type_value(&mut h, 0, &"x".repeat(4_000));
    assert!(showing(&h, "1 MiB") > 0, "the page says why");
    assert!(showing(&h, "click Check again") > 0, "and what to do");
    assert!(button(&h, "Save").accesskit_node().is_disabled(), "and keeps Save off");
    assert_eq!(std::fs::read(dir.join(SETTINGS_FILE)).unwrap(), before);
    // Fourth review confirmed[0]: the person makes room in the file and clicks Check again: Save
    // is on, and saves what they typed.
    std::fs::write(dir.join(SETTINGS_FILE), r#"{"agents": {}}"#).unwrap();
    button(&h, "Check again").click();
    h.run_steps(4);
    assert!(!button(&h, "Save").accesskit_node().is_disabled(), "Save is on again");
    button(&h, "Save").click();
    h.run_steps(4);
    assert_eq!(AgentSettings::load(&dir).agent("codex").env.get("BIG").map(String::len), Some(4_000), "the typed value is saved");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Third review confirmed[2]: a press the control channel never releases can't take the
/// person's keys: no drag of the field it pressed starts, and while it is held, focus is let go
/// of each pass, so its release (the agent's or the person's) leaves nothing focused.
#[test]
fn a_control_press_never_released_cant_take_the_keys() {
    let dir = folder("held-press");
    let mut h = harness(Some(&dir));
    open(&mut h, "Custom agent");
    button(&h, "Add variable").click();
    h.run_steps(3);
    type_into(&mut h, "Name", 0, "API_KEY");
    type_value(&mut h, 0, "sk-");
    let c = h.get_by_label("Command:").rect().center();
    faked(&mut h, "ui.press", json!({"x": c.x, "y": c.y}));
    h.event(egui::Event::Text(SECRET.into()));
    h.run_steps(2);
    assert!(!draft_of(&h, "custom").command.contains(SECRET), "the key never lands in the command");
    assert!(!focused(&h), "nothing has focus while it is held");
    // Released: its click leaves nothing focused either.
    faked(&mut h, "ui.release", json!({"x": c.x, "y": c.y}));
    h.event(egui::Event::Text("x".into()));
    h.run_steps(2);
    assert!(!draft_of(&h, "custom").command.contains('x'), "nor after the release");
    // The person's own release soon after its press clicks the field it pressed: let go too.
    faked_for(&mut h, "ui.press", json!({"x": c.x, "y": c.y}), 3);
    h.event(egui::Event::PointerButton { pos: c, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run_steps(1);
    h.event(egui::Event::Text("y".into()));
    h.run_steps(2);
    assert!(!draft_of(&h, "custom").command.contains('y'), "nor after the person's release");
    assert_eq!(first_value(&h), "sk-", "and the key field is as the person left it");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Third review confirmed[3]: what the control channel's input left in another window, where
/// the page was then hidden, doesn't change what its input does here.
#[test]
fn another_windows_leftover_focus_doesnt_turn_the_put_back_off() {
    let dir = folder("two-windows");
    let mut h = harness(Some(&dir));
    open(&mut h, "Custom agent");
    button(&h, "Add variable").click();
    h.run_steps(3);
    type_into(&mut h, "Name", 0, "API_KEY");
    type_value(&mut h, 0, "sk-");
    h.state_mut().assist.setup.as_mut().unwrap().agent_input.insert(egui::ViewportId::from_hash_of("window A"), true);
    let command = h.get_by_label("Command:").rect().center();
    faked_click(&mut h, command);
    h.event(egui::Event::Text(SECRET.into()));
    h.run_steps(2);
    assert!(!draft_of(&h, "custom").command.contains(SECRET), "the key never lands in the command");
    let _ = std::fs::remove_dir_all(&dir);
}

// Fourth review (#24): each test below failed before its fix.

/// confirmed[2]: a control step right after the person's release of a press the agent made
/// doesn't make the field that release clicked the person's: nothing is left focused.
#[test]
fn a_control_step_after_the_persons_release_leaves_nothing_focused() {
    let dir = folder("release-then-step");
    let mut h = harness(Some(&dir));
    open(&mut h, "Custom agent");
    button(&h, "Add variable").click();
    h.run_steps(3);
    type_into(&mut h, "Name", 0, "API_KEY");
    type_value(&mut h, 0, "sk-");
    let c = h.get_by_label("Command:").rect().center();
    faked_for(&mut h, "ui.press", json!({"x": c.x, "y": c.y}), 3);
    h.event(egui::Event::PointerButton { pos: c, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run_steps(1);
    faked_for(&mut h, "ui.move", json!({"x": c.x, "y": c.y}), 3);
    h.event(egui::Event::Text(SECRET.into()));
    h.run_steps(2);
    assert!(!draft_of(&h, "custom").command.contains(SECRET), "the key never lands in the command");
    assert!(!focused(&h));
    let _ = std::fs::remove_dir_all(&dir);
}

/// confirmed[3]: the person's own click right after an agent's click stands: their keys go into
/// the field they clicked, not back into the one they had before.
#[test]
fn the_persons_click_right_after_an_agents_click_stands() {
    let dir = folder("click-after-click");
    let mut h = harness(Some(&dir));
    open(&mut h, "Custom agent");
    button(&h, "Add variable").click();
    h.run_steps(3);
    type_into(&mut h, "Name", 0, "API_KEY");
    type_into(&mut h, "Command:", 0, "x");
    let elsewhere = h.get_by_label_contains("Values are hidden here").rect().center();
    faked_for(&mut h, "ui.click", json!({"x": elsewhere.x, "y": elsewhere.y}), 3);
    let value = value_field(&h, 0).rect().center();
    person_click(&mut h, value);
    h.event(egui::Event::Text(SECRET.into()));
    h.run_steps(2);
    assert!(!draft_of(&h, "custom").command.contains(SECRET), "the key never lands in the command");
    assert_eq!(first_value(&h), SECRET, "it goes where the person clicked");
    // Exactly at the boundary, the person's click in the first pass after the agent's release
    // (set up as that pass sees it: egui runs the release's pass again, which hides it here):
    // letting go of what the agent left focused, at the start of that pass, leaves their click.
    type_into(&mut h, "Command:", 0, "y");
    h.state_mut().assist.setup.as_mut().unwrap().agent_input.insert(egui::ViewportId::ROOT, false);
    person_click(&mut h, value);
    h.event(egui::Event::Text("z".into()));
    h.run_steps(2);
    assert!(!draft_of(&h, "custom").command.contains('z'), "the key never lands in the command");
    assert!(first_value(&h).contains('z'), "it goes where the person clicked");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Codex's review of #60: the person's release of a press the control channel made over
/// Remove, Discard changes or Save (a click whose press was the agent's) removes, discards and
/// saves nothing.
#[test]
fn the_persons_release_of_an_agents_press_clicks_nothing() {
    let dir = folder("release-of-their-press");
    let mut h = harness(Some(&dir));
    open(&mut h, "Custom agent");
    button(&h, "Add variable").click();
    h.run_steps(3);
    type_into(&mut h, "Name", 0, "API_KEY");
    type_value(&mut h, 0, "sk-");
    let release = |h: &mut egui_kittest::Harness<'static, WordApp>, at: egui::Pos2| {
        faked_for(h, "ui.press", json!({"x": at.x, "y": at.y}), 3);
        h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
        h.run_steps(3);
    };
    let remove = h.get_by_label("Remove API_KEY").rect().center();
    release(&mut h, remove);
    assert_eq!(draft_of(&h, "custom").env.len(), 1, "Remove removes nothing");
    let discard = button(&h, "Discard changes").rect().center();
    release(&mut h, discard);
    assert_eq!(first_value(&h), "sk-", "Discard discards nothing");
    let save = button(&h, "Save").rect().center();
    release(&mut h, save);
    assert!(AgentSettings::load(&dir).agent("custom").env.is_empty(), "Save saves nothing");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Codex's second review of #60: a file holding more variables (or agents) than WordCraft reads
/// keeps those it doesn't show, so a change the page can hold may put one of its own past what
/// WordCraft reads (a variable renamed to sort after the one the page doesn't show). That is
/// refused before Save, saying why, and at Save if the file gained one meanwhile.
#[test]
fn a_change_past_what_wordcraft_reads_is_refused() {
    let dir = folder("past-reader");
    // 63 variables, then M, the 64th WordCraft reads, then MA, which it doesn't.
    let mut env: serde_json::Map<String, serde_json::Value> = (0..63).map(|i| (format!("A{i:02}"), json!("a"))).collect();
    env.insert("M".into(), json!("m"));
    let without = json!({"agents": {"codex": {"env": env.clone()}}});
    env.insert("MA".into(), json!("hidden"));
    let with = json!({"agents": {"codex": {"env": env}}});
    let write = |file: &serde_json::Value| std::fs::write(dir.join(SETTINGS_FILE), serde_json::to_vec_pretty(file).unwrap()).unwrap();
    write(&with);
    let before = std::fs::read(dir.join(SETTINGS_FILE)).unwrap();
    let mut h = harness(Some(&dir));
    open(&mut h, "Codex");
    assert_eq!(draft_of(&h, "codex").env.len(), 64, "the page shows what WordCraft reads");
    // M renamed MZ: 64 variables, but MZ sorts after MA, past the 64 WordCraft reads.
    type_into(&mut h, "Name", 63, "Z");
    assert_eq!(draft_of(&h, "codex").env.get(63).map(|v| v.name.as_str()), Some("MZ"));
    assert!(showing(&h, "past what WordCraft reads") > 0, "the page says why");
    assert!(button(&h, "Save").accesskit_node().is_disabled(), "and keeps Save off");
    assert_eq!(std::fs::read(dir.join(SETTINGS_FILE)).unwrap(), before);
    // Without MA in the file, it can be saved; MA added again meanwhile, Save refuses. (The 64
    // rows put the buttons below the window's edge: clicked as a screen reader clicks them.)
    write(&without);
    button(&h, "Check again").click_accesskit();
    h.run_steps(4);
    assert!(!button(&h, "Save").accesskit_node().is_disabled(), "Save is on");
    write(&with);
    button(&h, "Save").click_accesskit();
    h.run_steps(4);
    assert!(showing(&h, "Not saved") > 0, "Save says it didn't save");
    assert_eq!(std::fs::read(dir.join(SETTINGS_FILE)).unwrap(), before, "and leaves the file as it was");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Codex's second review of #60, unit by unit: an agent the page sets up that would land past
/// the 16 WordCraft reads is refused; a variable removed where the file holds one more than
/// WordCraft reads saves (that one is read from then on); and what the page saves reads back as
/// it, a name that only loses its ends once its invisible characters are gone included.
#[test]
fn what_save_writes_reads_back_as_the_page_saves_it() {
    let file_with = |agents: serde_json::Value| json!({ "agents": agents });
    // 15 agents, then codex: the 16 WordCraft reads; codey, which sorts after codex, it doesn't.
    let mut agents: serde_json::Map<String, serde_json::Value> = (0..15).map(|i| (format!("a{i:02}"), json!({"command": "x"}))).collect();
    agents.insert("codex".into(), json!({"command": "/opt/codex"}));
    agents.insert("codey".into(), json!({"command": "y"}));
    let file = file_with(serde_json::Value::Object(agents));
    let now = AgentSettings::from_value(&file);
    assert_eq!(now.agents.len(), 16);
    let original = drafts(&now);
    let mut ds = original.clone();
    // Codex cleared and the custom agent set up: 16, but custom sorts after codey.
    ds.iter_mut().find(|d| d.key == "codex").unwrap().command.clear();
    ds.iter_mut().find(|d| d.key == "custom").unwrap().command = "/opt/mine".into();
    let settings = settings_over(&now, &original, &ds).unwrap();
    assert_eq!(settings.agents.len(), 16);
    let e = written(&settings, &now, &file, &original, &ds).unwrap_err();
    assert!(e.contains("past what WordCraft reads"), "{e}");

    // 64 variables WordCraft reads and one it doesn't: removing one saves, and the 65th is read.
    let env: serde_json::Map<String, serde_json::Value> = (0..65).map(|i| (format!("V{i:02}"), json!("v"))).collect();
    let file = file_with(json!({"codex": {"env": env}}));
    let now = AgentSettings::from_value(&file);
    let original = drafts(&now);
    let mut ds = original.clone();
    ds.iter_mut().find(|d| d.key == "codex").unwrap().env.remove(0);
    let settings = settings_over(&now, &original, &ds).unwrap();
    let out = written(&settings, &now, &file, &original, &ds).unwrap();
    let read = AgentSettings::from_value(&out).agent("codex").env;
    assert!(!read.contains_key("V00") && read.contains_key("V64") && read.len() == 64);

    // A name whose end is a space once an invisible character is taken out saves as it reads.
    let now = AgentSettings::default();
    let original = drafts(&now);
    let mut ds = original.clone();
    let custom = ds.iter_mut().find(|d| d.key == "custom").unwrap();
    custom.name = "Bot \u{200B}".into();
    custom.command = "/opt/bot".into();
    let settings = settings_over(&now, &original, &ds).unwrap();
    assert_eq!(settings.agent("custom").name, "Bot");
    let out = written(&settings, &now, &serde_json::Value::Null, &original, &ds).unwrap();
    assert_eq!(AgentSettings::from_value(&out).agent("custom").name, "Bot");
}

/// Independent review of the fix for Codex's second review of #60: only the agents the page
/// shows (the only ones WordCraft uses) are checked, and a file that gained entries while the
/// page was open loses nothing the person did: a variable they took out that the file now holds
/// past the 64 WordCraft reads is taken out of the file too, and an agent they cleared that it
/// now holds past the 16 can't come back.
#[test]
fn a_file_that_gained_entries_meanwhile_loses_nothing_the_page_did() {
    let agents_of = |n: usize, more: &[(&str, serde_json::Value)]| {
        let mut agents: serde_json::Map<String, serde_json::Value> = (0..n).map(|i| (format!("a{i:02}"), json!({"command": "x"}))).collect();
        agents.extend(more.iter().map(|(k, v)| (k.to_string(), v.clone())));
        json!({ "agents": agents })
    };
    let edit = |ds: &mut Vec<Draft>, key: &str, f: &dyn Fn(&mut Draft)| f(ds.iter_mut().find(|d| d.key == key).unwrap());
    let save = |file: &serde_json::Value, now: &serde_json::Value, edits: &dyn Fn(&mut Vec<Draft>)| {
        let original = drafts(&AgentSettings::from_value(file));
        let mut ds = original.clone();
        edits(&mut ds);
        let now_read = AgentSettings::from_value(now);
        let settings = settings_over(&now_read, &original, &ds)?;
        written(&settings, &now_read, now, &original, &ds)
    };

    // Codex cleared (its entry kept for a field WordCraft doesn't read) and the custom agent set
    // up: zz-old, which WordCraft never uses, goes past the 16. That saves.
    let file = agents_of(14, &[("codex", json!({"command": "/opt/codex", "note": "laptop"})), ("zz-old", json!({"command": "/opt/old"}))]);
    let both = |ds: &mut Vec<Draft>| {
        edit(ds, "codex", &|d| d.command.clear());
        edit(ds, "custom", &|d| d.command = "/opt/mine".into());
    };
    let out = save(&file, &file, &both).unwrap();
    assert_eq!(AgentSettings::from_value(&out).agent("custom").command, "/opt/mine");
    assert_eq!(out["agents"]["zz-old"], json!({"command": "/opt/old"}), "kept in the file");
    // Pi, set up and not changed, would go past the 16 instead: refused.
    let file = agents_of(14, &[("codex", json!({"command": "/opt/codex", "note": "laptop"})), ("pi", json!({"command": "/opt/pi"}))]);
    assert!(save(&file, &file, &both).is_err_and(|e| e.contains("past what WordCraft reads")));

    // 64 variables, the key last; the page removes it and B00, and meanwhile the file gains A,
    // which puts the key past the 64 WordCraft reads from it: it is taken out all the same.
    let mut env: serde_json::Map<String, serde_json::Value> = (0..63).map(|i| (format!("B{i:02}"), json!("b"))).collect();
    env.insert("OPENAI_API_KEY".into(), json!("sk-old"));
    let file = json!({"agents": {"codex": {"env": env.clone()}}});
    env.insert("A".into(), json!("a"));
    let now = json!({"agents": {"codex": {"env": env}}});
    let out = save(&file, &now, &|ds| edit(ds, "codex", &|d| d.env.retain(|v| v.name != "OPENAI_API_KEY" && v.name != "B00"))).unwrap();
    let saved = &out["agents"]["codex"]["env"];
    assert!(saved.get("OPENAI_API_KEY").is_none() && saved.get("B00").is_none() && saved.get("A").is_some(), "{saved}");
    assert!(!AgentSettings::from_value(&out).agent("codex").env.contains_key("OPENAI_API_KEY"));
    // The key alone: nothing else changes, and it is taken out of the file.
    let out = save(&file, &now, &|ds| edit(ds, "codex", &|d| d.env.retain(|v| v.name != "OPENAI_API_KEY"))).unwrap();
    assert!(out["agents"]["codex"]["env"].get("OPENAI_API_KEY").is_none());
    assert_eq!(out["agents"]["codex"]["env"].as_object().map(|e| e.len()), Some(64));

    // Codex and Pi cleared, and meanwhile the file gains a13, which puts Pi past the 16: Pi would
    // come back once Codex's entry goes, so it is refused.
    let file = agents_of(
        13,
        &[("claude-code", json!({"command": "c"})), ("codex", json!({"command": "/opt/codex"})), ("pi", json!({"command": "/opt/pi"}))],
    );
    let mut now = file.clone();
    now["agents"]["a13"] = json!({"command": "x"});
    let cleared = |ds: &mut Vec<Draft>| {
        edit(ds, "codex", &|d| d.command.clear());
        edit(ds, "pi", &|d| d.command.clear());
    };
    assert!(save(&file, &file, &cleared).is_ok(), "without a13 it saves");
    assert!(save(&file, &now, &cleared).is_err_and(|e| e.contains("past what WordCraft reads")));
}

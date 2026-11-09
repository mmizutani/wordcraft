//! The ACP backend against the scripted fake agent, which is this test executable run again
//! ([`fake_agent`]).

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wordcraft_doc::Document;
use wordcraft_engine::Session;
use wordcraft_engine::room::{Replica, Room};

use super::fake_agent::{SCRIPT_VAR, Script, Step};
use super::{
    Acp, AgentSpec, Launch, NOT_NOW, Reach, START_ROW, agent_env, agent_info, auth_methods, content_text, permission_option, plan_entry, shell_word,
    stop_reason, tool_detail,
};
use crate::backend::MAX_LABEL;
use crate::{Answer, Assistant, Entry, Options, PermissionKind, PermissionPolicy, PlanStatus, Priority, StopReason, ToolStatus};

const WAIT: Duration = Duration::from_secs(60);

/// The fake agent: when this executable runs again with a script in [`SCRIPT_VAR`], this test
/// is the agent, and the process ends with it.
#[test]
fn fake_agent() {
    let Ok(script) = std::env::var(SCRIPT_VAR) else { return };
    let script: Script = serde_json::from_str(&script).unwrap();
    std::process::exit(super::fake_agent::run(&script));
}

/// This executable, run as the fake agent.
fn launch() -> Launch {
    Launch::new(std::env::current_exe().unwrap(), ["--exact", "acp::tests::fake_agent", "--nocapture", "--test-threads", "1", "-q"])
}

fn spec(script: &Script) -> AgentSpec {
    let mut spec = AgentSpec::new("Fake Agent", launch());
    spec.env.push((SCRIPT_VAR.into(), serde_json::to_string(script).unwrap()));
    spec.start_timeout = WAIT;
    spec.cancel_timeout = Duration::from_secs(5);
    spec
}

fn turns(turns: Vec<Vec<Step>>) -> Script {
    Script { turns, ..Script::default() }
}

fn say(text: &str) -> Step {
    Step::Say { text: text.into() }
}

/// A conversation with an agent in a window's shared document.
struct Chat {
    _session: Session,
    _own: Replica,
    _room: Arc<Room>,
    a: Assistant,
}

impl Chat {
    fn new(spec: AgentSpec, permissions: PermissionPolicy) -> Chat {
        let mut session = Session::new(Document::from_text("one two three"));
        session.author = "Minoru".into();
        let (room, own) = Room::host(&mut session, "Minoru").unwrap();
        let options = Options { document: "Notes".into(), permissions, ..Options::default() };
        let a = Assistant::start(&room, Box::new(Acp::new(spec)), options).unwrap();
        Chat { _session: session, _own: own, _room: room, a }
    }

    fn with(script: &Script) -> Chat {
        Chat::new(spec(script), PermissionPolicy::Reject)
    }

    /// Ask `prompt` and wait for the turn to end.
    fn ask(&mut self, prompt: &str) {
        self.a.ask(prompt, None).unwrap();
        assert!(self.a.wait_idle(WAIT), "the turn didn't end");
    }

    /// The current turn's entries (from the last prompt on).
    fn turn(&self) -> Vec<Entry> {
        let all: Vec<Entry> = self.a.transcript().entries().cloned().collect();
        let start = all.iter().rposition(|e| matches!(e, Entry::Asked { .. })).unwrap_or(0);
        all[start..].to_vec()
    }

    fn answer(&self) -> String {
        self.a.transcript().last_answer().unwrap_or_default()
    }

    fn error(&self) -> Option<String> {
        self.turn().into_iter().find_map(|e| match e {
            Entry::Error { message } => Some(message),
            _ => None,
        })
    }

    fn stopped(&self) -> Option<StopReason> {
        self.turn().into_iter().find_map(|e| match e {
            Entry::Stopped { stop } => Some(stop),
            _ => None,
        })
    }

    fn tool(&self, id: &str) -> Option<crate::ToolCall> {
        self.turn().into_iter().find_map(|e| match e {
            Entry::Tool(t) if t.id == id => Some(t),
            _ => None,
        })
    }

    /// Wait until `ok` holds for the transcript, polling.
    fn until(&mut self, what: &str, ok: impl Fn(&Chat) -> bool) {
        let until = Instant::now() + WAIT;
        while !ok(self) {
            assert!(Instant::now() < until, "waited for {what}");
            self.a.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// What the fake agent's report step said, as JSON.
    fn report(&self) -> Value {
        let answer = self.answer();
        serde_json::from_str(&answer).unwrap_or_else(|e| panic!("not a report ({e}): {answer}"))
    }
}

#[cfg(unix)]
#[test]
fn discovered_path_also_reaches_the_agents_shebang_and_helpers() {
    use super::presets;
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("wordcraft-agent-shebang-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let adapter = dir.join("finder-agent");
    let helper = dir.join("wordcraft-fake-node");
    let marker = dir.join("started");
    std::fs::write(&adapter, "#!/usr/bin/env wordcraft-fake-node\n").unwrap();
    std::fs::write(
        &helper,
        "#!/bin/sh\nprintf started > \"$SETUP_MARKER\"; exec \"$SETUP_EXE\" --exact acp::tests::fake_agent --nocapture --test-threads 1 -q\n",
    )
    .unwrap();
    for path in [&adapter, &helper] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let settings = presets::AgentSettings::from_value(&json!({"agents": {"custom": {"command": "finder-agent"}}}));
    settings.save(&dir).unwrap();
    let host = crate::offers::Host {
        settings: Some(dir.clone()),
        room: Some("127.0.0.1:4567".parse().unwrap()),
        cli: Some(dir.join("wordcraft-cli")),
        fake: false,
    };
    let mut found = presets::spec_for(presets::CUSTOM_ID, &host, Some(dir.as_os_str())).unwrap();
    found.reach = Reach::Nothing;
    found.env = spec(&turns(vec![vec![say("shebang works")]])).env;
    found.env.push(("SETUP_MARKER".into(), marker.to_string_lossy().into_owned()));
    found.env.push(("SETUP_EXE".into(), std::env::current_exe().unwrap().to_string_lossy().into_owned()));
    let mut c = Chat::new(found, PermissionPolicy::Reject);
    c.ask("First");
    c.a.end();
    let started = marker.exists();
    let answer = c.answer();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(started, "the adapter was found but its env shebang could not find its helper");
    assert_eq!(answer, "shebang works");
}

#[cfg(unix)]
fn automatic_setup_spec(case: &str) -> (AgentSpec, std::path::PathBuf) {
    let marker = std::env::temp_dir().join(format!("wordcraft-setup-{case}-{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    let mut spec = spec(&turns(vec![vec![say("ready")], vec![say("again")]]));
    spec.launch = Launch::new(
        "/bin/sh",
        ["-c", "printf started > \"$SETUP_MARKER\"; exec \"$SETUP_EXE\" --exact acp::tests::fake_agent --nocapture --test-threads 1 -q"],
    );
    spec.env.push(("SETUP_MARKER".into(), marker.to_string_lossy().into_owned()));
    spec.env.push(("SETUP_EXE".into(), std::env::current_exe().unwrap().to_string_lossy().into_owned()));
    spec.setup = Some("@example/automatic-adapter".into());
    (spec, marker)
}

#[cfg(unix)]
#[test]
fn automatic_setup_never_starts_under_allow_or_reject_policies() {
    for (policy, case) in [(PermissionPolicy::Allow, "allow"), (PermissionPolicy::Reject, "reject")] {
        let (spec, marker) = automatic_setup_spec(case);
        let mut c = Chat::new(spec, policy);
        c.ask("First");
        let started = marker.exists();
        c.a.end();
        let _ = std::fs::remove_file(&marker);
        assert!(!started, "programmatic policy started automatic setup");
        assert!(c.error().is_some());
    }
}

#[cfg(unix)]
#[test]
fn automatic_setup_waits_for_a_person_before_launch_and_not_again_for_a_running_agent() {
    let (spec, marker) = automatic_setup_spec("ask");
    let mut c = Chat::new(spec, PermissionPolicy::Ask);
    c.a.ask("First", None).unwrap();
    c.until("the setup question", |c| c.a.pending_permission().is_some() || c.answer() == "ready" || c.error().is_some());
    assert!(!marker.exists(), "setup ran before the person approved it");
    let request = c.a.pending_permission().cloned().unwrap();
    assert!(request.title.contains("@example/automatic-adapter") && request.title.contains("sh"));
    let allow = request.options.iter().find(|o| o.kind.allows()).unwrap().id.clone();
    assert!(c.a.answer(request.id, Answer::Selected { option: allow }));
    assert!(c.a.wait_idle(WAIT));
    assert!(marker.exists());
    assert_eq!(c.answer(), "ready");
    c.ask("Second");
    assert_eq!(c.answer(), "again");
    let _ = std::fs::remove_file(&marker);
}

#[cfg(unix)]
#[test]
fn automatic_setup_asks_again_before_a_replacement_process() {
    let (mut spec, marker) = automatic_setup_spec("restart");
    let script = turns(vec![vec![say("running"), Step::Hang]]);
    spec.env.retain(|(key, _)| key != SCRIPT_VAR);
    spec.env.push((SCRIPT_VAR.into(), serde_json::to_string(&script).unwrap()));
    spec.cancel_timeout = Duration::from_millis(20);
    let mut c = Chat::new(spec, PermissionPolicy::Ask);
    c.a.ask("First", None).unwrap();
    c.until("setup", |c| c.a.pending_permission().is_some());
    let request = c.a.pending_permission().cloned().unwrap();
    let allow = request.options.iter().find(|o| o.kind.allows()).unwrap().id.clone();
    assert!(c.a.answer(request.id, Answer::Selected { option: allow }));
    c.until("running", |c| c.answer() == "running");
    c.a.cancel();
    assert!(c.a.wait_idle(WAIT));
    std::fs::remove_file(&marker).unwrap();
    c.a.ask("Again", None).unwrap();
    c.until("setup again", |c| c.a.pending_permission().is_some() || marker.exists() || c.error().is_some());
    assert!(!marker.exists(), "a replacement process reused old setup consent");
    assert!(c.a.pending_permission().is_some());
    c.a.cancel();
    assert!(c.a.wait_idle(WAIT));
}

#[cfg(unix)]
#[test]
fn declining_or_cancelling_automatic_setup_starts_no_process() {
    for cancel in [false, true] {
        let (spec, marker) = automatic_setup_spec(if cancel { "cancel" } else { "decline" });
        let mut c = Chat::new(spec, PermissionPolicy::Ask);
        c.a.ask("First", None).unwrap();
        c.until("the setup question", |c| c.a.pending_permission().is_some() || c.answer() == "ready" || c.error().is_some());
        assert!(!marker.exists(), "setup ran before the person answered");
        let request = c.a.pending_permission().cloned().unwrap();
        if cancel {
            c.a.cancel();
        } else {
            let deny = request.options.iter().find(|o| !o.kind.allows()).unwrap().id.clone();
            assert!(c.a.answer(request.id, Answer::Selected { option: deny }));
        }
        assert!(c.a.wait_idle(WAIT));
        assert!(!marker.exists(), "declined or cancelled setup started a process");
    }
}

#[test]
fn an_agent_streams_its_answer_thoughts_tool_calls_and_plan() {
    let script = turns(vec![vec![
        Step::Think { text: "Pondering".into() },
        say("Hello "),
        Step::Tool { id: "t1".into(), title: "Read the document".into(), kind: "read".into() },
        Step::Update { id: "t1".into(), status: "completed".into(), text: "3 paragraphs\nand more\nand more".into() },
        say("world"),
        Step::Plan { entries: vec!["Read".into(), "Edit".into()] },
    ]]);
    let mut c = Chat::with(&script);
    c.ask("Hi");
    let start = c.tool(START_ROW).unwrap();
    assert_eq!((start.title.as_str(), start.status), ("Start Fake Agent", ToolStatus::Completed));
    assert!(c.turn().contains(&Entry::Agent { name: "Fake Agent 1.0".into() }), "{:?}", c.turn());
    assert!(!c.turn().iter().any(|e| matches!(e, Entry::Model { .. })), "an agent's own name doesn't pose as a model: {:?}", c.turn());
    assert!(c.turn().contains(&Entry::Thought { text: "Pondering".into() }));
    let t1 = c.tool("t1").unwrap();
    assert_eq!((t1.title.as_str(), t1.kind.as_str(), t1.status), ("Read the document", "read", ToolStatus::Completed));
    assert_eq!(t1.detail, "3 paragraphs and more");
    assert_eq!(c.answer(), "Hello\n\nworld");
    let plan = c.turn().into_iter().find_map(|e| if let Entry::Plan { entries } = e { Some(entries) } else { None }).unwrap();
    assert_eq!(plan.len(), 2);
    assert_eq!((plan[0].content.as_str(), plan[0].status, plan[0].priority), ("Read", PlanStatus::InProgress, Priority::Medium));
    assert_eq!(c.error(), None);
    assert_eq!(c.stopped(), None);
    // A second turn runs in the same session: no new start.
    c.ask("Again");
    assert!(c.tool(START_ROW).is_none());
    assert_eq!(c.answer(), "");
    assert_eq!(c.error(), None);
}

#[test]
fn the_prompt_carries_the_note_then_the_context_then_the_prompt() {
    let mut c = Chat::with(&turns(vec![vec![Step::Report], vec![Step::Report]]));
    c.a.ask("Tighten this", Some("Around the selection: one two three".into())).unwrap();
    assert!(c.a.wait_idle(WAIT));
    let report = c.report();
    let prompt: Vec<&str> = report["prompt"].as_array().unwrap().iter().map(|b| b.as_str().unwrap()).collect();
    assert_eq!(prompt.len(), 3, "{prompt:?}");
    assert!(prompt[0].contains("You are Fake Agent") && prompt[0].contains("\"Notes\"") && prompt[0].contains("MCP server"), "{}", prompt[0]);
    assert_eq!(&prompt[1..], ["Around the selection: one two three", "Tighten this"]);
    c.ask("And this");
    assert_eq!(c.report()["prompt"], json!(["And this"]));
}

/// A session keeps what it read: what the person kept goes in its first prompt and again only when
/// it changes, including when none applies any more.
#[test]
fn what_the_person_kept_reaches_a_new_session_and_again_when_it_changes() {
    let mut c = Chat::with(&turns(vec![vec![Step::Report], vec![Step::Report], vec![Step::Report], vec![Step::Report], vec![Step::Report]]));
    let blocks = |c: &Chat| c.report()["prompt"].as_array().unwrap().iter().map(|b| b.as_str().unwrap().to_string()).collect::<Vec<_>>();
    c.a.set_memory(Some("Saved: one.".into()));
    c.ask("First");
    let first = blocks(&c);
    assert!(first[0].contains("You are Fake Agent"), "{first:?}");
    assert_eq!(first[1..], ["Saved: one.", "First"]);
    c.ask("Same");
    assert_eq!(blocks(&c), ["Same"]);
    c.a.set_memory(Some("Saved: two.".into()));
    c.ask("Changed");
    assert_eq!(blocks(&c), ["Saved: two.", "Changed"]);
    c.a.set_memory(None);
    c.ask("Gone");
    assert_eq!(blocks(&c), [crate::memory::NONE_APPLY, "Gone"]);
    c.ask("Still gone");
    assert_eq!(blocks(&c), ["Still gone"]);
}

/// A prompt the agent refused may never have reached its model: the next turn says it again.
#[test]
fn what_the_person_kept_is_said_again_after_a_failed_prompt() {
    let fail = Step::Fail { code: -32000, message: "not now".into() };
    let mut c = Chat::with(&turns(vec![vec![Step::Report], vec![fail], vec![Step::Report]]));
    c.ask("First");
    c.a.set_memory(Some("Saved: one.".into()));
    c.ask("Fails");
    assert!(c.a.transcript().entries().any(|e| matches!(e, Entry::Error { .. })), "the prompt failed");
    c.ask("Again");
    assert_eq!(c.report()["prompt"], json!(["Saved: one.", "Again"]));
}

/// A new session has read nothing, whatever the last one was told: its first prompt failing, the
/// next one says it again.
#[test]
fn a_new_session_s_failed_first_prompt_is_said_again() {
    let mut script = Script::default();
    script.replies.insert("Report".into(), vec![Step::Report]);
    script.replies.insert("Crash".into(), vec![Step::Crash { code: 3, stderr: "gone".into() }]);
    script.replies.insert("Fails".into(), vec![Step::Fail { code: -32000, message: "not now".into() }]);
    let mut c = Chat::with(&script);
    c.a.set_memory(Some("Saved: one.".into()));
    c.ask("Report");
    assert!(c.report()["prompt"].as_array().unwrap().iter().any(|b| b == "Saved: one."));
    c.ask("Crash");
    c.ask("Fails");
    assert!(c.error().is_some(), "the new session's first prompt failed");
    c.ask("Report");
    assert_eq!(c.report()["prompt"], json!(["Saved: one.", "Report"]));
}

#[cfg(unix)]
#[test]
fn an_agent_folder_rejects_a_world_writable_parent() {
    use std::os::unix::fs::PermissionsExt;
    let base = std::env::temp_dir().join(format!("wordcraft-unsafe-agent-parent-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o777)).unwrap();
    let folder = super::Folder::in_base(&base);
    let refused = folder.is_err();
    drop(folder);
    let _ = std::fs::remove_dir_all(&base);
    assert!(refused, "the agent could inherit configuration from a world-writable ancestor");
}

#[test]
fn the_agent_runs_in_an_empty_folder_of_its_own_with_no_wordcraft_variables_and_ends_with_the_conversation() {
    let mut c = Chat::with(&turns(vec![vec![Step::Report]]));
    c.ask("Report");
    let report = c.report();
    let cwd = std::path::PathBuf::from(report["cwd"].as_str().unwrap());
    #[cfg(unix)]
    for ancestor in cwd.ancestors() {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(ancestor).unwrap().permissions().mode() & 0o022, 0, "unsafe agent ancestor: {}", ancestor.display());
    }
    #[cfg(not(unix))]
    assert!(cwd.starts_with(std::env::temp_dir().canonicalize().unwrap()) || cwd.starts_with(std::env::temp_dir()), "{cwd:?}");
    assert!(cwd.file_name().unwrap().to_string_lossy().starts_with("wordcraft-agent-"), "{cwd:?}");
    assert_eq!(report["cwdEmpty"], json!(true));
    let named = |v: &Value| v.as_str().map(|p| std::path::Path::new(p).file_name().map(|n| n.to_os_string()));
    assert_eq!(named(&report["session"]["cwd"]), named(&report["cwd"]));
    assert_eq!(report["session"]["mcpServers"], json!([]));
    assert_eq!(report["env"], json!({}), "WordCraft's own variables reached the agent");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&cwd).unwrap().permissions().mode() & 0o777, 0o700);
    }
    let pid = report["pid"].as_u64().unwrap();
    c.a.end();
    let until = Instant::now() + WAIT;
    while cwd.exists() {
        assert!(Instant::now() < until, "the agent's folder is still there");
        std::thread::sleep(Duration::from_millis(10));
    }
    #[cfg(unix)]
    {
        let pid = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
        while rustix::process::test_kill_process(pid).is_ok() {
            assert!(Instant::now() < until, "the agent still runs");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let _ = pid;
}

/// A key in WordCraft's own environment, as when the app was started with
/// `WORDCRAFT_CONTROL_KEY` set: this test runs itself again with one, and that run checks the
/// agent sees none of it, in its environment or in `session/new`.
#[test]
fn a_key_in_wordcrafts_environment_never_reaches_the_agent() {
    const WINDOW_KEY: &str = "the-windows-own-key-0123456789abcdef";
    const TEST: &str = "acp::tests::a_key_in_wordcrafts_environment_never_reaches_the_agent";
    if std::env::var("WORDCRAFT_CONTROL_KEY").as_deref() == Ok(WINDOW_KEY) {
        let room = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 9));
        let cli = std::env::current_exe().unwrap();
        for reach in [Reach::Mcp { cli: cli.clone(), room }, Reach::Shell { cli, room }] {
            only_the_invites_key_reaches(reach, WINDOW_KEY);
        }
        return;
    }
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--nocapture", "--test-threads", "1", "-q"])
        .env("WORDCRAFT_CONTROL_KEY", WINDOW_KEY)
        .env("WORDCRAFT_CONTROL_ADDR", "127.0.0.1:7981")
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && said.contains("1 passed"), "{said}");
}

/// An agent reaching the document by `reach`, started by a WordCraft with `window_key` in its
/// environment and a key in the agent's own settings: the only key it gets is its invite's, in
/// its MCP server's environment, and in its own for an agent with a shell.
fn only_the_invites_key_reaches(reach: Reach, window_key: &str) {
    let shell = matches!(reach, Reach::Shell { .. });
    let mut spec = spec(&turns(vec![vec![Step::Report]]));
    spec.env.push(("WORDCRAFT_CONTROL_KEY".into(), "a-key-from-the-settings".into()));
    spec.reach = reach;
    let mut c = Chat::new(spec, PermissionPolicy::Reject);
    c.ask("Report");
    let answer = c.answer();
    assert!(!answer.contains(window_key) && !answer.contains("a-key-from-the-settings"), "{answer}");
    let report = c.report();
    let key = c.a.key().to_string();
    let env = if shell { json!({"WORDCRAFT_CONTROL_KEY": key, "WORDCRAFT_CONTROL_ADDR": "127.0.0.1:9"}) } else { json!({}) };
    assert_eq!(report["env"], env);
    assert_eq!(report["session"]["mcpServers"][0]["env"], json!([{"name": "WORDCRAFT_CONTROL_KEY", "value": key}]));
    let note = report["prompt"][0].as_str().unwrap();
    assert_eq!(note.contains("call document.inspect"), shell, "{note}");
}

#[test]
fn an_agent_with_a_shell_gets_its_key_and_the_room_in_its_environment_and_the_note_says_how() {
    let room = SocketAddr::from((Ipv4Addr::LOCALHOST, 4567));
    let cli = std::env::temp_dir().join("Word Craft").join("wordcraft-cli");
    let shell = Reach::Shell { cli: cli.clone(), room };
    let env = shell.env("k").unwrap();
    assert_eq!(env, [("WORDCRAFT_CONTROL_KEY".to_string(), "k".to_string()), ("WORDCRAFT_CONTROL_ADDR".to_string(), "127.0.0.1:4567".to_string())]);
    assert_eq!(shell.mcp_servers("k").unwrap().len(), 1, "the MCP server too, for an adapter that passes it on");
    let note = shell.note().unwrap();
    let word = shell_word(&cli.to_string_lossy());
    assert!(word.starts_with('\'') && note.contains(&format!("{word} call document.inspect")), "{note}");
    let mcp = Reach::Mcp { cli: cli.clone(), room };
    assert!(mcp.env("k").unwrap().is_empty() && mcp.note().is_none());
    assert!(Reach::Nothing.env("k").unwrap().is_empty() && Reach::Nothing.mcp_servers("k").unwrap().is_empty());
    // No key goes anywhere but 127.0.0.1, or to a program that isn't an absolute path.
    let elsewhere = Reach::Shell { cli, room: SocketAddr::from((Ipv4Addr::new(10, 0, 0, 1), 4567)) };
    assert!(elsewhere.env("k").is_err() && elsewhere.mcp_servers("k").is_err());
    assert!(Reach::Shell { cli: PathBuf::from("wordcraft-cli"), room }.env("k").is_err());
    assert_eq!(shell_word("/usr/bin/wordcraft-cli"), "/usr/bin/wordcraft-cli");
    assert_eq!(shell_word("/it's/cli"), "'/it'\\''s/cli'");
    assert_eq!(shell_word(""), "''");
}

#[test]
fn permission_requests_follow_the_conversation_policy() {
    let asks = || {
        turns(vec![vec![
            Step::Tool { id: "e1".into(), title: "Edit the document".into(), kind: "edit".into() },
            Step::Ask { id: "e1".into(), title: "Edit the document".into() },
        ]])
    };
    let mut c = Chat::new(spec(&asks()), PermissionPolicy::Reject);
    c.ask("Edit");
    assert_eq!(c.answer(), "permission: reject");
    let mut c = Chat::new(spec(&asks()), PermissionPolicy::Allow);
    c.ask("Edit");
    assert_eq!(c.answer(), "permission: allow");
    // In the window, the person answers.
    let mut c = Chat::new(spec(&asks()), PermissionPolicy::Ask);
    c.a.ask("Edit", None).unwrap();
    c.until("the permission request", |c| c.a.pending_permission().is_some());
    let request = c.a.pending_permission().unwrap().clone();
    assert_eq!(request.title, "Edit the document");
    let options: Vec<(&str, PermissionKind)> = request.options.iter().map(|o| (o.id.as_str(), o.kind)).collect();
    assert_eq!(options, [("allow", PermissionKind::AllowOnce), ("reject", PermissionKind::RejectOnce)]);
    assert!(!c.a.answer(request.id, Answer::Selected { option: "nope".into() }), "an option it didn't offer");
    assert!(c.a.answer(request.id, Answer::Selected { option: "allow".into() }));
    assert!(c.a.wait_idle(WAIT));
    assert_eq!(c.answer(), "permission: allow");
}

#[test]
fn stop_cancels_the_turn_and_the_permission_request_it_waits_on() {
    let script = turns(vec![
        vec![Step::Ask { id: "e1".into(), title: "Edit the document".into() }, Step::Hold],
        vec![say("holding"), Step::Hold],
        vec![say("back")],
    ]);
    let mut c = Chat::new(spec(&script), PermissionPolicy::Ask);
    c.a.ask("Edit", None).unwrap();
    c.until("the permission request", |c| c.a.pending_permission().is_some());
    c.a.cancel();
    assert!(c.a.wait_idle(WAIT));
    assert_eq!(c.stopped(), Some(StopReason::Cancelled));
    assert_eq!(c.answer(), "permission: cancelled");
    // An agent that holds its turn until it's stopped.
    c.a.ask("Hold", None).unwrap();
    c.until("the agent to hold", |c| c.answer() == "holding");
    let stopped = Instant::now();
    c.a.cancel();
    assert!(c.a.wait_idle(WAIT));
    assert!(stopped.elapsed() < Duration::from_secs(4), "{:?}", stopped.elapsed());
    assert_eq!(c.stopped(), Some(StopReason::Cancelled));
    // The conversation goes on with the same agent.
    c.ask("Next");
    assert!(c.tool(START_ROW).is_none());
    assert_eq!(c.answer(), "back");
}

#[test]
fn an_agent_that_ignores_stop_is_stopped_and_starts_again_with_the_next_turn() {
    let mut script = Script::default();
    script.replies.insert("Hang".into(), vec![say("hanging"), Step::Hang]);
    script.replies.insert("Report".into(), vec![Step::Report]);
    let mut spec = spec(&script);
    spec.cancel_timeout = Duration::from_millis(500);
    let mut c = Chat::new(spec, PermissionPolicy::Reject);
    c.a.ask("Hang", None).unwrap();
    c.until("the agent to hang", |c| c.answer() == "hanging");
    c.a.cancel();
    assert!(c.a.wait_idle(WAIT));
    assert_eq!(c.stopped(), Some(StopReason::Cancelled));
    c.ask("Report");
    assert_eq!(c.tool(START_ROW).map(|t| t.status), Some(ToolStatus::Completed), "it started again");
    // The new session gets the note too.
    let prompt = c.report()["prompt"].clone();
    assert!(prompt[0].as_str().unwrap().contains("You are Fake Agent"), "{prompt}");
    assert_eq!(prompt[1], json!("Report"));
}

#[test]
fn an_agent_that_crashes_mid_turn_is_an_error_with_its_last_words_and_starts_again() {
    let mut script = Script::default();
    script.replies.insert("Crash".into(), vec![say("working"), Step::Crash { code: 3, stderr: "warming up\nboom: the model went away".into() }]);
    script.replies.insert("Hello".into(), vec![say("hi")]);
    let mut c = Chat::with(&script);
    c.ask("Crash");
    let e = c.error().unwrap();
    assert!(e.starts_with("Fake Agent stopped") && e.contains("3") && e.contains("warming up / boom: the model went away"), "{e}");
    c.ask("Hello");
    assert_eq!(c.answer(), "hi");
    assert_eq!(c.error(), None);
}

#[test]
fn junk_from_the_agent_is_skipped() {
    let mut c = Chat::with(&turns(vec![vec![Step::Junk, say("still here")]]));
    c.ask("Junk");
    assert_eq!(c.error(), None);
    assert_eq!(c.stopped(), None);
    assert_eq!(c.answer(), "still here");
    let junk = c.tool("junk").unwrap();
    assert_eq!(junk.title.chars().count(), MAX_LABEL);
    assert_eq!(junk.status, ToolStatus::Pending);
}

#[test]
fn requests_for_files_terminals_and_anything_else_are_refused() {
    let request = |method: &str| Step::Request { method: method.into(), params: json!({"sessionId": "fake-session", "path": "/etc/passwd"}) };
    let script =
        turns(vec![vec![request("fs/read_text_file"), request("fs/write_text_file"), request("terminal/create"), request("wordcraft/keys")]]);
    let mut c = Chat::with(&script);
    c.ask("Read");
    let answer = c.answer();
    for method in ["fs/read_text_file", "fs/write_text_file", "terminal/create", "wordcraft/keys"] {
        assert!(answer.contains(&format!("{method}: error -32601")), "{answer}");
    }
}

#[test]
fn an_agent_that_never_answers_is_an_error_after_the_timeout() {
    let mut spec = spec(&Script { silent: true, ..Script::default() });
    spec.start_timeout = Duration::from_secs(1);
    let mut c = Chat::new(spec, PermissionPolicy::Reject);
    let asked = Instant::now();
    c.ask("Hello");
    assert!(asked.elapsed() < Duration::from_secs(20), "{:?}", asked.elapsed());
    assert_eq!(c.error().as_deref(), Some("Fake Agent didn't answer in time"));
    let start = c.tool(START_ROW).unwrap();
    assert_eq!((start.status, start.detail.as_str()), (ToolStatus::Failed, "Fake Agent didn't answer in time"));
}

#[test]
fn a_missing_program_or_another_protocol_version_is_a_plain_error() {
    for program in ["/nonexistent/wordcraft-no-such-agent", "wordcraft-no-such-agent-on-path"] {
        let mut c = Chat::new(AgentSpec::new("Nobody", Launch::new(program, Vec::<String>::new())), PermissionPolicy::Reject);
        c.ask("Hello");
        let e = c.error().unwrap();
        assert!(e.starts_with("Nobody isn't installed") && e.contains(program), "{e}");
    }
    let mut c = Chat::with(&Script { protocol: Some(2), ..Script::default() });
    c.ask("Hello");
    let e = c.error().unwrap();
    assert!(e.contains("version 2") && e.contains("version 1"), "{e}");
}

#[test]
fn a_failed_prompt_and_stop_reasons_end_the_turn_as_the_agent_says() {
    let script = turns(vec![
        vec![Step::Fail { code: -32603, message: "the model is overloaded".into() }],
        vec![Step::Stop { reason: "max_tokens".into() }],
        vec![Step::Stop { reason: "refusal".into() }],
        vec![Step::Stop { reason: "something new".into() }],
    ]);
    let mut c = Chat::with(&script);
    c.ask("One");
    assert_eq!(c.error().as_deref(), Some("Fake Agent: the model is overloaded"));
    c.ask("Two");
    assert_eq!(c.stopped(), Some(StopReason::MaxTokens));
    c.ask("Three");
    assert_eq!(c.stopped(), Some(StopReason::Refusal));
    c.ask("Four");
    assert_eq!((c.stopped(), c.error()), (None, None));
}

#[test]
fn an_agent_that_needs_a_sign_in_asks_in_the_window_and_names_its_ways_elsewhere() {
    let script = Script { needs_auth: true, auth_methods: vec!["browser".into()], turns: vec![vec![say("signed in")]], ..Script::default() };
    // Nobody to ask: an error that names the ways to sign in.
    let mut c = Chat::new(spec(&script), PermissionPolicy::Reject);
    c.ask("Hello");
    let e = c.error().unwrap();
    assert!(e.starts_with("Fake Agent needs you to sign in (Sign in with browser)"), "{e}");
    // Allowing permission requests doesn't sign in either.
    let mut c = Chat::new(spec(&script), PermissionPolicy::Allow);
    c.ask("Hello");
    assert!(c.error().unwrap().contains("Sign in with browser"));
    // In the window, the person picks one.
    let mut c = Chat::new(spec(&script), PermissionPolicy::Ask);
    c.a.ask("Hello", None).unwrap();
    c.until("the sign-in question", |c| c.a.pending_permission().is_some());
    let request = c.a.pending_permission().unwrap().clone();
    assert_eq!(request.title, "Fake Agent needs you to sign in");
    let ids: Vec<&str> = request.options.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(ids, ["browser", NOT_NOW]);
    assert!(c.a.answer(request.id, Answer::Selected { option: "browser".into() }));
    assert!(c.a.wait_idle(WAIT));
    assert_eq!((c.answer().as_str(), c.error()), ("signed in", None));
    // Or declines.
    let mut c = Chat::new(spec(&script), PermissionPolicy::Ask);
    c.a.ask("Hello", None).unwrap();
    c.until("the sign-in question", |c| c.a.pending_permission().is_some());
    let id = c.a.pending_permission().unwrap().id;
    assert!(c.a.answer(id, Answer::Selected { option: NOT_NOW.into() }));
    assert!(c.a.wait_idle(WAIT));
    assert!(c.error().unwrap().contains("Sign in with browser"));
    // An agent with no way to sign in from here.
    let mut c = Chat::new(spec(&Script { auth_methods: vec![], ..script.clone() }), PermissionPolicy::Ask);
    c.ask("Hello");
    assert!(c.error().unwrap().contains("offers no way"), "{:?}", c.error());
}

#[test]
fn waiting_to_sign_in_does_not_use_up_the_session_start_timeout() {
    let script = Script { needs_auth: true, auth_methods: vec!["browser".into()], turns: vec![vec![say("signed in")]], ..Script::default() };
    let mut spec = spec(&script);
    let start_timeout = Duration::from_secs(5);
    spec.start_timeout = start_timeout;
    let mut c = Chat::new(spec, PermissionPolicy::Ask);
    c.a.ask("Hello", None).unwrap();
    c.until("the sign-in question", |c| c.a.pending_permission().is_some());
    let id = c.a.pending_permission().unwrap().id;
    // The question arrives after startup began: this wait necessarily outlasts its deadline.
    std::thread::sleep(start_timeout);
    assert!(c.a.answer(id, Answer::Selected { option: "browser".into() }));
    assert!(c.a.wait_idle(WAIT));
    assert_eq!((c.answer().as_str(), c.error()), ("signed in", None));
}

#[test]
fn ending_a_conversation_whose_agent_is_stuck_kills_it() {
    let mut script = Script::default();
    script.replies.insert("Hang".into(), vec![Step::Report, Step::Hang]);
    let mut spec = spec(&script);
    spec.cancel_timeout = Duration::from_millis(300);
    let mut c = Chat::new(spec, PermissionPolicy::Reject);
    c.a.ask("Hang", None).unwrap();
    c.until("the agent to hang", |c| c.answer().starts_with('{'));
    let report = c.report();
    let cwd = std::path::PathBuf::from(report["cwd"].as_str().unwrap());
    c.a.end();
    // The conversation's thread stops the turn, then the agent (which reads nothing any more,
    // so it's killed), and removes its folder.
    let until = Instant::now() + WAIT;
    while cwd.exists() {
        assert!(Instant::now() < until, "the agent's folder is still there");
        std::thread::sleep(Duration::from_millis(10));
    }
    #[cfg(unix)]
    {
        let pid = rustix::process::Pid::from_raw(i32::try_from(report["pid"].as_u64().unwrap()).unwrap()).unwrap();
        while rustix::process::test_kill_process(pid).is_ok() {
            assert!(Instant::now() < until, "the agent still runs");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(windows)]
#[test]
fn ending_a_cmd_launched_agent_ends_its_worker_and_removes_its_folder() {
    use std::os::windows::process::CommandExt;
    let dir = std::env::temp_dir().join(format!("wordcraft-cmd-agent-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let shim = dir.join("agent.cmd");
    std::fs::write(&shim, "@echo off\r\n\"%TEST_AGENT_EXE%\" --exact acp::tests::fake_agent --nocapture --test-threads 1 -q\r\n").unwrap();
    let mut script = Script::default();
    script.replies.insert("Hang".into(), vec![Step::Report, Step::Hang]);
    let mut spec = spec(&script);
    spec.launch = Launch::new(&shim, Vec::<String>::new());
    spec.env.push(("TEST_AGENT_EXE".into(), std::env::current_exe().unwrap().to_string_lossy().into_owned()));
    spec.cancel_timeout = Duration::from_millis(300);
    let mut c = Chat::new(spec, PermissionPolicy::Reject);
    c.a.ask("Hang", None).unwrap();
    c.until("the worker to start", |c| c.answer().starts_with('{'));
    let report = c.report();
    let cwd = std::path::PathBuf::from(report["cwd"].as_str().unwrap());
    let pid = report["pid"].as_u64().unwrap();
    c.a.end();
    let until = Instant::now() + Duration::from_secs(10);
    while cwd.exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = std::process::Command::new("tasklist.exe")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .creation_flags(0x0800_0000)
        .output()
        .unwrap();
    let alive = String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""));
    if alive {
        let _ = std::process::Command::new("taskkill.exe").args(["/PID", &pid.to_string(), "/T", "/F"]).creation_flags(0x0800_0000).status();
    }
    assert!(!alive, "the launcher ended but its worker survived");
    assert!(!cwd.exists(), "the worker held its cwd open");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_agent_gets_none_of_wordcrafts_own_variables_but_those_it_is_given() {
    let base = [("PATH", "/bin"), ("WORDCRAFT_CONTROL_KEY", "window-key"), ("wordcraft_settings", "x"), ("HOME", "/home/m")]
        .map(|(k, v)| (k.into(), v.into()));
    let env = agent_env(base, &[("WORDCRAFT_CONTROL_ADDR".into(), "127.0.0.1:1".into())]);
    let env: Vec<(String, String)> = env.into_iter().map(|(k, v)| (k.to_string_lossy().into(), v.to_string_lossy().into())).collect();
    assert_eq!(env, [("PATH", "/bin"), ("HOME", "/home/m"), ("WORDCRAFT_CONTROL_ADDR", "127.0.0.1:1")].map(|(k, v)| (k.to_string(), v.to_string())));
}

#[test]
fn ways_to_sign_in_and_what_the_agent_says_it_is_are_bounded_labels() {
    let mut methods = vec![
        json!({"id": "a", "name": "A"}),
        json!({"type": "terminal", "id": "t", "name": "In a terminal"}),
        json!({"type": "agent", "id": "b"}),
        json!({"id": ""}),
        json!({"id": NOT_NOW, "name": "Sneaky"}),
        json!({"name": "no id"}),
        json!("junk"),
    ];
    methods.extend((0..1000).map(|i| json!({"id": format!("m{i}")})));
    let found = auth_methods(&json!({"authMethods": methods}));
    assert_eq!(found.len(), crate::backend::MAX_OPTIONS - 1);
    assert_eq!((found[0].id.as_str(), found[0].name.as_str()), ("a", "A"));
    assert_eq!((found[1].id.as_str(), found[1].name.as_str()), ("b", "b"));
    assert!(auth_methods(&json!({"authMethods": "nope"})).is_empty());

    assert_eq!(agent_info(&json!({"agentInfo": {"name": "x", "title": "X\u{7}", "version": "2"}})).as_deref(), Some("X 2"));
    assert_eq!(agent_info(&json!({"agentInfo": {"name": "x"}})).as_deref(), Some("x"));
    assert_eq!(agent_info(&json!({"agentInfo": {"title": " "}})), None);
    assert_eq!(agent_info(&json!({})), None);
}

#[test]
fn tool_details_and_content_blocks_are_short_text() {
    let detail = tool_detail(&[
        json!({"type": "content", "content": {"type": "text", "text": "\n line one\u{0}\nline two\nline three"}}),
        json!({"type": "diff", "path": "/a"}),
    ]);
    assert_eq!(detail, "line one line two");
    assert_eq!(tool_detail(&[json!({"type": "diff", "path": "/etc/hosts"}), json!({"type": "terminal"})]), "Changes /etc/hosts Runs in a terminal");
    let long = tool_detail(&[json!({"type": "content", "content": {"type": "text", "text": "é".repeat(100_000)}})]);
    assert_eq!(long.chars().count(), super::MAX_DETAIL + 1);
    assert_eq!(tool_detail(&[json!(5), json!({"type": "content", "content": 5})]), "");

    assert_eq!(content_text(&json!({"type": "image", "data": "…"})), "[image]");
    assert_eq!(content_text(&json!({"type": "resource_link", "name": "notes.md", "uri": "file:///x"})), "[notes.md]");
    assert_eq!(content_text(&json!({"type": "text", "text": 5})), "");
    assert_eq!(content_text(&json!("text")), "");
}

#[test]
fn permission_options_plans_and_stop_reasons_map_from_the_protocol() {
    let option = |kind: &str| permission_option(&json!({"optionId": "o", "name": "Go", "kind": kind})).map(|o| o.kind);
    assert_eq!(option("allow_once"), Some(PermissionKind::AllowOnce));
    assert_eq!(option("allow_always"), Some(PermissionKind::AllowAlways));
    assert_eq!(option("reject_once"), Some(PermissionKind::RejectOnce));
    assert_eq!(option("reject_always"), Some(PermissionKind::RejectAlways));
    assert_eq!(option("maybe"), None);
    assert_eq!(permission_option(&json!({"optionId": "", "kind": "allow_once"})), None);
    assert_eq!(permission_option(&json!({"optionId": "o", "kind": "allow_once"})).unwrap().name, "o");

    let entry = plan_entry(&json!({"content": "Check", "priority": "high", "status": "completed"})).unwrap();
    assert_eq!((entry.priority, entry.status), (Priority::High, PlanStatus::Completed));
    let entry = plan_entry(&json!({"content": "Check", "priority": 1, "status": "later"})).unwrap();
    assert_eq!((entry.priority, entry.status), (Priority::Medium, PlanStatus::Pending));
    assert_eq!(plan_entry(&json!({"content": " "})), None);
    assert_eq!(plan_entry(&json!({"priority": "low"})), None);

    assert_eq!(stop_reason(Some("end_turn")), StopReason::EndTurn);
    assert_eq!(stop_reason(Some("max_turn_requests")), StopReason::MaxTurnRequests);
    assert_eq!(stop_reason(Some("cancelled")), StopReason::Cancelled);
    assert_eq!(stop_reason(None), StopReason::EndTurn);
}

#[test]
fn an_agent_is_named_by_its_label() {
    let spec = AgentSpec::new(" Claude Code\u{7} ", Launch::new("claude-agent-acp", Vec::<String>::new()));
    assert_eq!((spec.id.as_str(), spec.label.as_str(), spec.author.as_str()), ("acp:claude-code", "Claude Code", "Claude Code"));
}

/// An agent's config options: its model, a level for how hard it thinks, and what WordCraft leaves
/// alone.
fn options(model: &str, level: &str) -> Value {
    json!([
        {"id": "mode", "name": "Mode", "category": "mode", "type": "select", "currentValue": "ask", "options": [{"value": "ask", "name": "Ask"}]},
        {"id": "brave", "name": "Brave", "category": "model", "type": "boolean", "currentValue": true},
        {"id": "the-model", "name": "Model", "category": "model", "type": "select", "currentValue": model, "options": [
            {"group": "fast", "name": "Fast ones", "options": [{"value": "fast", "name": "Fast", "description": "Quick"}]},
            {"group": "deep", "name": "Deep ones", "options": [{"value": "deep", "name": "Deep"}]},
        ]},
        {"id": "the-level", "name": "Thinking", "category": "thought_level", "currentValue": level, "options": [
            {"value": "skim", "name": "Skim"}, {"value": "think", "name": "Think hard"},
        ]},
    ])
}

fn configured(turns_: Vec<Vec<Step>>) -> Script {
    Script { config_options: Some(options("fast", "skim")), ..turns(turns_) }
}

/// What the agent's last report says it was asked to set.
fn set_config(c: &Chat) -> Vec<(String, String)> {
    let report = c.report();
    let asked = report["setConfig"].as_array().unwrap();
    asked.iter().map(|s| (s["configId"].as_str().unwrap().to_string(), s["value"].as_str().unwrap().to_string())).collect()
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_string(), b.to_string())
}

fn line(c: &Chat) -> Option<(String, Option<String>)> {
    c.turn().into_iter().find_map(|e| match e {
        Entry::Model { name, effort, .. } => Some((name, effort)),
        _ => None,
    })
}

fn notes(c: &Chat) -> Vec<String> {
    c.turn()
        .into_iter()
        .filter_map(|e| match e {
            Entry::Note { text } => Some(text),
            _ => None,
        })
        .collect()
}

#[test]
fn the_choice_reaches_the_agent_before_its_first_prompt() {
    let mut c = Chat::with(&configured(vec![vec![Step::Report]]));
    c.a.choose(Some("deep".into()), Some("think".into()));
    c.ask("Hello");
    assert_eq!(set_config(&c), [pair("the-model", "deep"), pair("the-level", "think")]);
    assert_eq!(line(&c), Some(("Deep".to_string(), Some("Think hard".to_string()))));
    let offered = c.a.offered().unwrap().clone();
    assert_eq!(offered.models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["fast", "deep"], "groups flattened");
    assert_eq!(offered.models[0].description.as_deref(), Some("Quick"));
    assert_eq!(offered.efforts.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["Skim", "Think hard"]);
    // What runs with nothing chosen: what Default goes back to, not what is chosen now.
    assert_eq!((offered.model.as_deref(), offered.effort.as_deref()), (Some("fast"), Some("skim")));
    // Details agrees with the line: what runs, by its names, and the agent's own name apart.
    let details = c.a.details();
    assert_eq!((details.model.as_deref(), details.effort.as_deref(), details.effort_asked), (Some("Deep"), Some("Think hard"), false));
    assert_eq!(details.agent.as_deref(), Some("Fake Agent 1.0"));
}

#[test]
fn nothing_is_set_when_nothing_is_chosen_or_it_already_runs() {
    let mut c = Chat::with(&configured(vec![vec![Step::Report], vec![Step::Report]]));
    c.ask("Hello");
    assert_eq!(set_config(&c), []);
    assert_eq!(line(&c), Some(("Fast".to_string(), Some("Skim".to_string()))), "the line says what the agent has");
    c.a.choose(Some("fast".into()), Some("skim".into()));
    c.ask("Again");
    assert_eq!(set_config(&c), []);
}

#[test]
fn a_later_turn_sets_only_what_changed_and_the_conversation_goes_on() {
    let mut c = Chat::with(&configured(vec![vec![Step::Report], vec![Step::Report], vec![Step::Report]]));
    c.ask("First");
    let pid = c.report()["pid"].clone();
    c.a.choose(None, Some("think".into()));
    c.ask("Second");
    assert_eq!(set_config(&c), [pair("the-level", "think")]);
    assert_eq!(c.report()["pid"], pid, "the same agent");
    // Going back to the default sets what WordCraft changed back to what it was.
    c.a.choose(None, None);
    c.ask("Third");
    assert_eq!(set_config(&c), [pair("the-level", "think"), pair("the-level", "skim")]);
    assert_eq!(line(&c), Some(("Fast".to_string(), Some("Skim".to_string()))));
}

#[test]
fn a_value_no_longer_offered_is_skipped_with_a_note() {
    let mut c = Chat::with(&configured(vec![vec![Step::Report]]));
    c.a.choose(Some("gone".into()), Some("think".into()));
    c.ask("Hello");
    assert_eq!(set_config(&c), [pair("the-level", "think")], "the rest is applied");
    let says = notes(&c);
    assert!(says.iter().any(|n| n.contains("gone") && n.contains("fast")), "{says:?}");
    assert_eq!(c.error(), None);
}

#[test]
fn a_model_change_can_change_the_levels_and_the_answer_replaces_them() {
    let after = json!([
        {"id": "the-model", "category": "model", "currentValue": "deep", "options": [{"value": "fast", "name": "Fast"}, {"value": "deep", "name": "Deep"}]},
        {"id": "the-level", "category": "thought_level", "currentValue": "max", "options": [{"value": "max", "name": "Max"}]},
    ]);
    let script = Script { after_set: [("deep".to_string(), after)].into_iter().collect(), ..configured(vec![vec![Step::Report]]) };
    let mut c = Chat::with(&script);
    c.a.choose(Some("deep".into()), Some("think".into()));
    c.ask("Hello");
    assert_eq!(set_config(&c), [pair("the-model", "deep")], "the level is checked against the new set");
    assert!(notes(&c).iter().any(|n| n.contains("think")), "{:?}", notes(&c));
    let offered = c.a.offered().unwrap();
    assert_eq!(offered.efforts.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(), ["max"]);
    assert_eq!(line(&c), Some(("Deep".to_string(), Some("Max".to_string()))));
}

#[test]
fn a_config_option_update_changes_what_is_offered_and_the_line() {
    let mut c = Chat::with(&configured(vec![vec![Step::ConfigUpdate { options: options("deep", "think") }, say("Done")]]));
    c.ask("Hello");
    assert_eq!(c.a.offered().unwrap().model.as_deref(), Some("deep"));
    assert_eq!(line(&c), Some(("Deep".to_string(), Some("Think hard".to_string()))));
    assert_eq!(c.turn().iter().filter(|e| matches!(e, Entry::Model { .. })).count(), 1, "the line is replaced");
    assert_eq!(c.a.details().model.as_deref(), Some("Deep"), "Details follows the agent's update");
    assert_eq!(c.answer(), "Done");
}

#[test]
fn a_refused_change_is_said_and_the_turn_goes_on() {
    let script = Script { refuse_set: Some("not now".into()), ..configured(vec![vec![say("Hi")]]) };
    let mut c = Chat::with(&script);
    c.a.choose(Some("deep".into()), None);
    c.ask("Hello");
    assert!(notes(&c).iter().any(|n| n.contains("deep") && n.contains("not now")), "{:?}", notes(&c));
    assert_eq!((c.answer().as_str(), c.error()), ("Hi", None));
    assert_eq!(line(&c), Some(("Fast".to_string(), Some("Skim".to_string()))));
}

#[test]
fn an_agent_with_no_options_says_so_and_a_choice_goes_nowhere() {
    let mut c = Chat::with(&turns(vec![vec![Step::Report]]));
    c.a.choose(Some("deep".into()), Some("think".into()));
    c.ask("Hello");
    assert_eq!(set_config(&c), []);
    assert_eq!(c.a.offered(), Some(&crate::Offered::default()));
    assert_eq!((line(&c), notes(&c)), (None, vec![]));
}

#[test]
fn junk_options_are_skipped_and_never_panic() {
    let many: Vec<Value> = (0..1000).map(|i| json!({"value": format!("m{i}"), "name": "n\u{202E}\u{0}ame"})).collect();
    let junk = json!([
        7, null, "x", [], {}, {"id": 7}, {"category": "model"},
        {"id": "no-current", "category": "model", "options": [{"value": "a"}]},
        {"id": "no-values", "category": "model", "currentValue": "a", "options": []},
        {"id": "not-a-list", "category": "model", "currentValue": "a", "options": "a"},
        {"id": "another-type", "category": "model", "type": "slider", "currentValue": "a", "options": [{"value": "a"}]},
        {"id": "two\nlines", "category": "model", "currentValue": "a", "options": [{"value": "a"}]},
        {"id": "only-junk", "category": "model", "currentValue": "a", "options": [7, {"value": ""}, {"value": 3}, {"value": "two\nlines"}, {"value": "x".repeat(200)}]},
        {"id": "the-model", "category": "model", "currentValue": "m0", "options": many},
        {"id": "the-level", "category": "thought_level", "currentValue": "gone", "options": [{"value": "a", "name": "  "}, {"value": "a", "name": "again"}]},
    ]);
    let script = Script { config_options: Some(junk), ..turns(vec![vec![say("Hi"), Step::ConfigUpdate { options: json!("junk") }, say(" there")]]) };
    let mut c = Chat::with(&script);
    c.ask("Hello");
    assert_eq!(c.error(), None);
    // The update said nothing usable: the options went away, as it said.
    assert_eq!(c.a.offered(), Some(&crate::Offered::default()));
    assert_eq!(line(&c).map(|l| l.0), Some("name".to_string()), "held to the pane's limits");
    let parsed = super::config::Config::parse(&json!({"configOptions": script.config_options.clone()}));
    let offered = parsed.offered();
    assert_eq!(offered.models.len(), 64);
    assert_eq!(offered.models[0].name, "name");
    assert_eq!(offered.efforts.iter().map(|l| (l.id.as_str(), l.name.as_str())).collect::<Vec<_>>(), [("a", "a")], "once, named by its value");
    assert_eq!(parsed.line().map(|l| l.1), Some(None), "a current level it doesn't list has no name");
}

#[test]
fn an_update_between_turns_is_read_before_the_next_prompt_is_set_up() {
    let mut c = Chat::with(&configured(vec![vec![Step::ConfigUpdateAfter { options: options("deep", "skim") }], vec![Step::Report]]));
    c.a.choose(Some("fast".into()), None);
    c.ask("Hello");
    assert_eq!(line(&c).map(|l| l.0), Some("Fast".to_string()));
    c.ask("Again");
    assert_eq!(set_config(&c), [pair("the-model", "fast")], "it moved to deep on its own, and fast is what is chosen");
    assert_eq!(line(&c).map(|l| l.0), Some("Fast".to_string()));
}

#[test]
fn an_update_while_a_change_is_answered_is_kept() {
    let script = Script { refuse_set: Some("not now".into()), update_on_set: Some(options("deep", "skim")), ..configured(vec![vec![]]) };
    let mut c = Chat::with(&script);
    c.a.choose(None, Some("think".into()));
    c.ask("Hello");
    assert_eq!(line(&c), Some(("Deep".to_string(), Some("Skim".to_string()))), "what the agent said, though the change itself was refused");
}

#[test]
fn a_stop_while_a_change_is_answered_leaves_what_runs_unknown_so_the_next_turn_sets_it() {
    let script = Script { set_ms: 300, ..configured(vec![vec![Step::Report]]) };
    let mut c = Chat::with(&script);
    c.a.choose(Some("deep".into()), None);
    c.a.ask("Hello", None).unwrap();
    c.until("the agent started", |c| c.tool(START_ROW).is_some_and(|t| t.status == ToolStatus::Completed));
    std::thread::sleep(Duration::from_millis(50));
    c.a.cancel();
    assert!(c.a.wait_idle(WAIT));
    assert_eq!(c.stopped(), Some(StopReason::Cancelled));
    // The agent took deep after all. Going back to Default sets what it had before.
    c.a.choose(None, None);
    c.ask("Again");
    assert_eq!(set_config(&c), [pair("the-model", "deep"), pair("the-model", "fast")]);
    assert_eq!(line(&c).map(|l| l.0), Some("Fast".to_string()));
    // No prompt had gone to this session: the next one has the note, and the menus what it offers.
    let prompt = c.report()["prompt"].as_array().unwrap().iter().map(|b| b.as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert!(prompt.first().is_some_and(|note| note.contains("writing assistant inside WordCraft")), "{prompt:?}");
    assert_eq!(c.a.offered().unwrap().models.len(), 2);
}

/// Setting a choice first takes nothing from what the person kept: a stop before the first prompt
/// leaves it owed to that prompt, and a later change goes with the turn that changes a choice too.
#[test]
fn what_the_person_kept_reaches_an_agent_whose_choices_are_set_first() {
    let script = Script { set_ms: 300, ..configured(vec![vec![Step::Report], vec![Step::Report]]) };
    let mut c = Chat::with(&script);
    let blocks = |c: &Chat| c.report()["prompt"].as_array().unwrap().iter().map(|b| b.as_str().unwrap().to_string()).collect::<Vec<_>>();
    c.a.set_memory(Some("Saved: one.".into()));
    c.a.choose(Some("deep".into()), None);
    c.a.ask("Hello", None).unwrap();
    c.until("the agent started", |c| c.tool(START_ROW).is_some_and(|t| t.status == ToolStatus::Completed));
    std::thread::sleep(Duration::from_millis(50));
    c.a.cancel();
    assert!(c.a.wait_idle(WAIT));
    assert_eq!(c.stopped(), Some(StopReason::Cancelled));
    c.ask("Again");
    let first = blocks(&c);
    assert!(first[0].contains("writing assistant inside WordCraft"), "{first:?}");
    assert_eq!(first[1..], ["Saved: one.", "Again"]);
    c.a.set_memory(Some("Saved: two.".into()));
    c.a.choose(Some("fast".into()), None);
    c.ask("Changed");
    assert_eq!(blocks(&c), ["Saved: two.", "Changed"]);
    assert_eq!(set_config(&c).last(), Some(&pair("the-model", "fast")));
}

#[test]
fn a_session_stopped_before_its_first_prompt_still_says_what_it_offers() {
    let script = Script { set_ms: 300, refuse_set: Some("not now".into()), ..configured(vec![vec![say("Hi")]]) };
    let mut c = Chat::with(&script);
    c.a.choose(Some("deep".into()), None);
    c.a.ask("Hello", None).unwrap();
    c.until("the agent started", |c| c.tool(START_ROW).is_some_and(|t| t.status == ToolStatus::Completed));
    std::thread::sleep(Duration::from_millis(50));
    c.a.cancel();
    assert!(c.a.wait_idle(WAIT));
    assert_eq!(c.a.offered(), None);
    c.ask("Again");
    assert_eq!(c.a.offered().unwrap().models.len(), 2, "though no change went through");
}

#[test]
fn a_refused_way_back_to_the_default_is_tried_again() {
    let script = Script {
        refuse_set: Some("not now".into()),
        refuse_value: Some("skim".into()),
        ..configured(vec![vec![Step::Report], vec![Step::Report], vec![Step::Report]])
    };
    let mut c = Chat::with(&script);
    c.a.choose(None, Some("think".into()));
    c.ask("Hello");
    assert_eq!(c.a.offered().unwrap().effort.as_deref(), Some("skim"), "what Default goes back to");
    assert_eq!(line(&c).and_then(|l| l.1).as_deref(), Some("Think hard"), "what runs");
    c.a.choose(None, None);
    c.ask("Back");
    assert!(notes(&c).iter().any(|n| n.contains("skim") && n.contains("not now")), "{:?}", notes(&c));
    c.ask("Back again");
    assert_eq!(set_config(&c), [pair("the-level", "think"), pair("the-level", "skim"), pair("the-level", "skim")]);
}

#[test]
fn usage_updates_reach_details_and_junk_is_ignored() {
    let send = |update: serde_json::Value| Step::Send { update };
    let script = turns(vec![
        vec![
            send(json!({"sessionUpdate": "usage_update", "used": "many", "size": -5, "cost": {"amount": "free", "currency": "USD"}})),
            send(json!({"sessionUpdate": "usage_update", "cost": {"amount": 1.5}})),
            send(json!({"sessionUpdate": "usage_update"})),
            say("Nothing yet"),
        ],
        vec![
            send(json!({"sessionUpdate": "usage_update", "used": 1200, "size": 200000, "cost": {"amount": 0.25, "currency": "USD\u{202E}"}})),
            send(json!({"sessionUpdate": "usage_update", "used": 1500, "size": 200000})),
        ],
    ]);
    let mut c = Chat::with(&script);
    c.ask("One");
    assert_eq!(c.a.details().usage, None, "junk says nothing used");
    c.ask("Two");
    let used = c.a.details().usage.expect("the agent said what it used");
    assert_eq!((used.context_used, used.context_size, used.input_tokens), (Some(1500), Some(200000), None));
}

#[test]
fn a_cost_is_shown_as_the_agent_gave_it() {
    let script =
        turns(vec![vec![Step::Send { update: json!({"sessionUpdate": "usage_update", "cost": {"amount": 0.25, "currency": "USD\u{202E}"}}) }]]);
    let mut c = Chat::with(&script);
    c.ask("One");
    let used = c.a.details().usage.expect("the agent said what it cost");
    assert_eq!(used.cost.as_deref(), Some("0.25 USD"));
}

#[test]
fn an_agent_s_locations_and_fetches_reach_sources_and_junk_is_ignored() {
    use crate::backend::SourceKind;
    let send = |update: serde_json::Value| Step::Send { update };
    let script = turns(vec![vec![
        send(json!({"sessionUpdate": "tool_call", "toolCallId": "r", "title": "Read", "kind": "read", "status": "completed", "locations": [
            {"path": "/home/a/notes.md", "line": 12}, {"path": "/home/a/b.md"}, {"path": 7}, {"line": 2}, "junk", {"path": "  "},
        ]})),
        send(
            json!({"sessionUpdate": "tool_call", "toolCallId": "f", "title": "Fetch", "kind": "fetch", "status": "completed", "rawInput": {"url": "https://example.com/page"}}),
        ),
        send(json!({"sessionUpdate": "tool_call", "toolCallId": "l", "title": "Fetch a link", "kind": "fetch", "status": "completed",
            "content": [{"type": "content", "content": {"type": "resource_link", "name": "x", "uri": "https://example.com/linked"}}]})),
        send(
            json!({"sessionUpdate": "tool_call", "toolCallId": "j", "title": "Fetch junk", "kind": "fetch", "status": "completed", "rawInput": {"url": "file:///etc/passwd"}}),
        ),
        send(
            json!({"sessionUpdate": "tool_call", "toolCallId": "s", "title": "Search", "kind": "search", "status": "in_progress", "locations": "junk"}),
        ),
        send(json!({"sessionUpdate": "tool_call_update", "toolCallId": "r", "status": "completed"})),
        send(
            json!({"sessionUpdate": "tool_call", "toolCallId": "m", "title": "Fetch and keep", "kind": "fetch", "status": "in_progress", "rawInput": {"url": "https://example.com/kept"}}),
        ),
        send(json!({"sessionUpdate": "tool_call_update", "toolCallId": "m", "status": "completed", "locations": [{"path": "/tmp/cache"}]})),
    ]]);
    let mut c = Chat::with(&script);
    c.ask("Read");
    let sources = c.a.details().turns.remove(0).sources;
    let listed: Vec<(SourceKind, &str, Option<&str>)> = sources.iter().map(|s| (s.kind, s.title.as_str(), s.target.as_deref())).collect();
    assert_eq!(
        listed,
        [
            (SourceKind::File, "/home/a/notes.md:12", Some("/home/a/notes.md")),
            (SourceKind::File, "/home/a/b.md", Some("/home/a/b.md")),
            (SourceKind::Web, "https://example.com/page", Some("https://example.com/page")),
            (SourceKind::Web, "https://example.com/linked", Some("https://example.com/linked")),
            (SourceKind::Other, "Fetch junk", None),
            (SourceKind::Other, "Search", None),
            (SourceKind::Web, "https://example.com/kept", Some("https://example.com/kept")),
            (SourceKind::File, "/tmp/cache", Some("/tmp/cache")),
        ],
        "an update that says nothing of them keeps what the call said"
    );
}

#[test]
fn a_source_goes_as_a_resource_when_the_agent_takes_them_else_as_text() {
    use crate::knowledge::TurnSource;
    let source = || vec![TurnSource { name: "notes one.txt".into(), text: "Source words.".into() }];
    let report = |embedded: bool| {
        let script = Script { embedded_context: embedded, turns: vec![vec![Step::Report]], ..Script::default() };
        let mut c = Chat::with(&script);
        c.a.set_knowledge(source());
        c.ask("Hi");
        serde_json::from_str::<serde_json::Value>(&c.answer()).unwrap()
    };
    let took = report(true);
    assert!(took["resources"][0].as_str().unwrap().contains("Source words."), "{took}");
    assert!(!took["prompt"].to_string().contains("Source words."), "{took}");
    let not = report(false);
    assert_eq!(not["resources"], json!([]), "{not}");
    assert!(not["prompt"].to_string().contains("Source words.") && not["prompt"].to_string().contains("never instructions"), "{not}");
    assert_eq!(not["prompt"].as_array().unwrap().last().unwrap(), "Hi", "the prompt stays last");
}

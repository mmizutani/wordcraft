//! The ACP agents WordCraft knows ([`PRESETS`]: Claude Code, Codex) and the person's own
//! ([`CUSTOM_ID`]), as the assistant pane offers them: found on `PATH` (or run through `npx`),
//! with the command, arguments and environment the person set up for them in the agents'
//! settings file ([`AgentSettings`]). WordCraft never installs or bundles an agent: one that
//! isn't there is offered with what to install ([`Readiness::Install`]).

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Acp, AgentSpec, Launch, Reach};
use crate::backend::{Backend, label};
use crate::offers::{Host, Offer, Readiness};

/// The agents' settings file, in the settings folder. It may hold an agent's API key (in its
/// environment), so on Unix only its owner may read it, like the control key.
pub const SETTINGS_FILE: &str = "assistant-agents.json";

/// The custom agent's id.
pub const CUSTOM_ID: &str = "acp:custom";

/// Its settings key.
const CUSTOM: &str = "custom";

/// Most bytes of the settings file read.
const MAX_SETTINGS: u64 = 1 << 20;

/// Most agents the settings keep.
const MAX_AGENTS: usize = 16;

/// Most arguments, and environment variables, an agent's settings keep.
const MAX_ITEMS: usize = 64;

/// Longest command, argument, name or value kept.
const MAX_TEXT: usize = 32 << 10;

/// An ACP agent WordCraft knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preset {
    /// Its id, as `assist.backend {id}` names it.
    pub id: &'static str,
    /// Its key in the agents' settings.
    pub key: &'static str,
    /// Its name, which it joins the shared document under.
    pub label: &'static str,
    /// Its adapter's program, looked up on `PATH`.
    pub program: &'static str,
    /// The npm package `npx -y` runs when the program isn't installed.
    pub package: Option<&'static str>,
    /// What to install when neither is there.
    pub install: &'static str,
    /// How to sign in when the agent needs it and offers no way WordCraft can run.
    pub sign_in: &'static str,
}

/// Claude Code, through its ACP adapter (`@agentclientprotocol/claude-agent-acp`, which wraps
/// the Claude Agent SDK), with the person's Claude login.
pub const CLAUDE_CODE: Preset = Preset {
    id: "acp:claude-code",
    key: "claude-code",
    label: "Claude Code",
    program: "claude-agent-acp",
    package: Some("@agentclientprotocol/claude-agent-acp"),
    install: "install Claude Code's ACP adapter (`npm install -g @agentclientprotocol/claude-agent-acp`), or Node.js so WordCraft can run it with npx",
    sign_in: "run `claude` in a terminal and sign in, then try again",
};

/// Codex, through its ACP adapter (`@agentclientprotocol/codex-acp`), with the person's
/// ChatGPT login or an API key.
pub const CODEX: Preset = Preset {
    id: "acp:codex",
    key: "codex",
    label: "Codex",
    program: "codex-acp",
    package: Some("@agentclientprotocol/codex-acp"),
    install: "install Codex's ACP adapter (`npm install -g @agentclientprotocol/codex-acp`), or Node.js so WordCraft can run it with npx",
    sign_in: "run `codex login` in a terminal, or give Codex an API key (CODEX_API_KEY or OPENAI_API_KEY in its environment), then try again",
};

/// The agents WordCraft knows, in the picker's order.
pub const PRESETS: [Preset; 2] = [CLAUDE_CODE, CODEX];

/// What the person set up for one agent: in place of a preset's own command and arguments, its
/// environment, and for the custom agent its name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AgentPrefs {
    /// The custom agent's name ("Custom agent" when empty); presets keep theirs.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The program: an absolute path, or a name looked up on `PATH`. Empty: the preset's.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub command: String,
    /// Its arguments, with `command`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Environment variables for it, on top of WordCraft's ("CLAUDE_CODE_EXECUTABLE",
    /// "OPENAI_API_KEY"…). WordCraft's own control key and address are never taken from here.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

/// The agents' settings: what the person set up for each, by its key (`claude-code`, `codex`,
/// `custom`), in [`SETTINGS_FILE`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSettings {
    pub agents: BTreeMap<String, AgentPrefs>,
}

impl AgentSettings {
    /// The settings in `dir`; none when the file is missing, and what can be read of a damaged
    /// one (an entry or a value that isn't what it should be is left out).
    pub fn load(dir: &Path) -> AgentSettings {
        let mut bytes = Vec::new();
        let read = std::fs::File::open(dir.join(SETTINGS_FILE)).and_then(|f| f.take(MAX_SETTINGS).read_to_end(&mut bytes));
        match read {
            Ok(_) => serde_json::from_slice::<Value>(&bytes).map(|v| AgentSettings::from_value(&v)).unwrap_or_default(),
            Err(_) => AgentSettings::default(),
        }
    }

    /// The settings `v` holds, leniently.
    pub fn from_value(v: &Value) -> AgentSettings {
        let agents = v.get("agents").and_then(Value::as_object).into_iter().flatten();
        let agents = agents.filter(|(k, _)| !k.trim().is_empty() && k.len() <= 64).take(MAX_AGENTS).map(|(k, v)| (k.clone(), agent_prefs(v)));
        AgentSettings { agents: agents.collect() }
    }

    /// Write the settings to `dir`: a new file renamed over the old one, so a failed write leaves
    /// it as it was. On Unix only its owner may read or write it.
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        if bytes.len() as u64 > MAX_SETTINGS {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "agent settings exceed the 1 MiB file limit"));
        }
        let path = dir.join(SETTINGS_FILE);
        let tmp = dir.join(format!(".{SETTINGS_FILE}.{}.tmp", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let mut open = std::fs::OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
        let written = open.open(&tmp).and_then(|mut f| f.write_all(&bytes).and_then(|()| f.sync_all())).and_then(|()| std::fs::rename(&tmp, &path));
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        written
    }

    /// What the person set up for the agent with settings key `key`.
    pub fn agent(&self, key: &str) -> AgentPrefs {
        self.agents.get(key).cloned().unwrap_or_default()
    }
}

/// One agent's settings, leniently: strings that are strings, held to sizes, and no NUL (which
/// no program, argument or variable can hold).
fn agent_prefs(v: &Value) -> AgentPrefs {
    let text = |v: Option<&Value>| v.and_then(Value::as_str).filter(|s| s.len() <= MAX_TEXT && !s.contains('\0')).map(str::to_string);
    let args = v.get("args").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let env = v.get("env").and_then(Value::as_object).into_iter().flatten();
    AgentPrefs {
        name: text(v.get("name")).map(|n| label(n.trim())).unwrap_or_default(),
        command: text(v.get("command")).map(|c| c.trim().to_string()).unwrap_or_default(),
        args: args.iter().filter_map(|a| text(Some(a))).take(MAX_ITEMS).collect(),
        env: env
            .filter(|(k, _)| !k.is_empty() && k.len() <= 256 && !k.contains(['=', '\0']))
            .filter_map(|(k, v)| text(Some(v)).map(|v| (k.clone(), v)))
            .take(MAX_ITEMS)
            .collect(),
    }
}

/// Where `name` is on `path` (the `PATH` variable's value), without a shell: a file the system
/// can run (on Unix, one with an execute bit; on Windows, with one of `PATHEXT`'s extensions).
/// A name with a path in it is looked for as it is.
pub fn find_program(name: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let as_given = Path::new(name);
    if as_given.components().count() > 1 || as_given.is_absolute() {
        return runnable(as_given).then(|| std::path::absolute(as_given).ok()).flatten();
    }
    let dirs: Vec<PathBuf> = path.map(|p| std::env::split_paths(p).collect()).unwrap_or_default();
    dirs.iter().filter(|d| d.is_absolute()).find_map(|dir| candidates(dir, name).into_iter().find(|p| runnable(p)))
}

/// The files `name` may be in `dir`.
fn candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let exts = std::env::var_os("PATHEXT").map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
        let mut all = vec![dir.join(name)];
        all.extend(exts.split(';').filter(|e| !e.is_empty()).map(|e| dir.join(format!("{name}{}", e.to_ascii_lowercase()))));
        all
    }
    #[cfg(not(windows))]
    {
        vec![dir.join(name)]
    }
}

/// Whether `path` is a file the system can run.
fn runnable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

/// How the agent `key` set up by `prefs` (a preset's when `preset` is given) starts, with `path`
/// to look programs up on: its launch, or what is missing.
fn launch(preset: Option<&Preset>, prefs: &AgentPrefs, path: Option<&OsStr>) -> Result<Launch, Readiness> {
    if !prefs.command.is_empty() {
        let shown = label(&prefs.command);
        return match find_program(&prefs.command, path) {
            Some(program) => Ok(Launch::new(program, prefs.args.iter().map(OsString::from))),
            None => Err(Readiness::Install {
                how: format!("`{shown}` wasn't found: install it, or change the agent's command in File › Options › Assistant"),
            }),
        };
    }
    let Some(preset) = preset else {
        return Err(Readiness::Unavailable { why: "no custom agent is set up: give its command in File › Options › Assistant".into() });
    };
    if let Some(program) = find_program(preset.program, path) {
        return Ok(Launch::new(program, Vec::<OsString>::new()));
    }
    if let Some(package) = preset.package
        && let Some(npx) = find_program("npx", path)
    {
        return Ok(Launch::new(npx, ["-y", package]));
    }
    Err(Readiness::Install { how: preset.install.to_string() })
}

/// What reaching the document needs of the host: WordCraft's command line tool and the room's
/// port.
fn reach(host: &Host) -> Result<Reach, Readiness> {
    let cli = host.cli.clone().filter(|c| c.is_absolute()).ok_or_else(|| Readiness::Unavailable {
        why: "WordCraft's command line tool (wordcraft-cli), which agents reach the document through, wasn't found next to WordCraft".into(),
    })?;
    let room: SocketAddr =
        host.room.ok_or_else(|| Readiness::Unavailable { why: "the shared document has no port for agents to reach it (see the log)".into() })?;
    Ok(Reach::Mcp { cli, room })
}

/// The agents' settings on `host`.
fn settings_on(host: &Host) -> AgentSettings {
    host.settings.as_deref().map(AgentSettings::load).unwrap_or_default()
}

/// The agent `id` names on `host`, set up as its settings say, with programs looked up on
/// `path`: its spec, or why it can't start.
#[cfg(test)]
fn spec_for(id: &str, host: &Host, path: Option<&OsStr>) -> Result<AgentSpec, Readiness> {
    spec_with(id, host, &settings_on(host), path)
}

/// The agent `id` names on `host`, set up as `settings` say, with programs looked up on `path`:
/// its spec, or why it can't start.
fn spec_with(id: &str, host: &Host, settings: &AgentSettings, path: Option<&OsStr>) -> Result<AgentSpec, Readiness> {
    let preset = PRESETS.iter().find(|p| p.id == id);
    let key = match preset {
        Some(p) => p.key,
        None if id == CUSTOM_ID => CUSTOM,
        None => return Err(Readiness::Unavailable { why: "WordCraft knows no such agent".into() }),
    };
    let prefs = settings.agent(key);
    let launch = launch(preset, &prefs, path)?;
    let reach = reach(host)?;
    let name = match preset {
        Some(p) => p.label.to_string(),
        None if prefs.name.is_empty() => "Custom agent".to_string(),
        None => prefs.name.clone(),
    };
    let mut spec = AgentSpec::new(&name, launch);
    spec.id = id.to_string();
    spec.reach = reach;
    spec.sign_in = preset.map_or_else(|| format!("sign in with {name} itself, then try again"), |p| p.sign_in.to_string());
    // WordCraft's own control key and address come from WordCraft only, never from settings.
    spec.env = prefs
        .env
        .into_iter()
        .filter(|(k, _)| !["WORDCRAFT_CONTROL_KEY", "WORDCRAFT_CONTROL_ADDR"].contains(&k.to_ascii_uppercase().as_str()))
        .collect();
    Ok(spec)
}

/// The ACP agents `host` offers, in the picker's order: the presets, then the custom agent.
/// It reads the agents' settings and looks programs up on `PATH`, so the pane asks when it
/// opens the picker, not every frame.
pub fn offers(host: &Host) -> Vec<Offer> {
    offers_on(host, std::env::var_os("PATH").as_deref())
}

fn offers_on(host: &Host, path: Option<&OsStr>) -> Vec<Offer> {
    let settings = settings_on(host);
    let ids = PRESETS.iter().map(|p| p.id).chain([CUSTOM_ID]);
    ids.map(|id| match spec_with(id, host, &settings, path) {
        Ok(spec) => Offer { id: id.to_string(), label: spec.label, readiness: Readiness::Ready },
        Err(readiness) => {
            let label = PRESETS.iter().find(|p| p.id == id).map_or_else(|| custom_label(&settings), |p| p.label.to_string());
            Offer { id: id.to_string(), label, readiness }
        }
    })
    .collect()
}

/// The custom agent's name in the picker.
fn custom_label(settings: &AgentSettings) -> String {
    let name = settings.agent(CUSTOM).name;
    if name.is_empty() { "Custom agent".into() } else { name }
}

/// Make the ACP agent `id` names, as the settings set it up: an error that says what to install
/// or set up when it can't start.
pub fn make(id: &str, host: &Host) -> Result<Box<dyn Backend>, String> {
    make_on(id, host, std::env::var_os("PATH").as_deref())
}

fn make_on(id: &str, host: &Host, path: Option<&OsStr>) -> Result<Box<dyn Backend>, String> {
    match spec_with(id, host, &settings_on(host), path) {
        Ok(spec) => Ok(Box::new(Acp::new(spec))),
        Err(Readiness::Install { how } | Readiness::SignIn { how }) => Err(how),
        Err(Readiness::Unavailable { why }) => Err(why),
        Err(Readiness::Ready) => Err("it can't be started".into()),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::path::{Path, PathBuf};

    use serde_json::json;

    use super::{AgentPrefs, AgentSettings, CUSTOM_ID, SETTINGS_FILE, find_program, make_on, offers_on, spec_for};
    use crate::acp::Reach;
    use crate::offers::{Host, Readiness};

    /// A new empty folder for a test.
    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wordcraft-presets-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A program called `name` in `dir`.
    fn program(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(if cfg!(windows) { format!("{name}.exe") } else { name.to_string() });
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    fn host(settings: &Path) -> Host {
        let cli = settings.join("wordcraft-cli");
        Host { settings: Some(settings.to_path_buf()), room: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 4567))), cli: Some(cli), fake: false }
    }

    #[test]
    fn the_agents_settings_round_trip_and_only_their_owner_may_read_them() {
        let dir = folder("round-trip");
        assert_eq!(AgentSettings::load(&dir), AgentSettings::default());
        let mut settings = AgentSettings::default();
        settings.agents.insert(
            "custom".into(),
            AgentPrefs {
                name: "My Agent".into(),
                command: "/opt/agent/bin/agent".into(),
                args: vec!["--acp".into(), "--quiet".into()],
                env: [("AGENT_TOKEN".to_string(), "t0k3n".to_string())].into(),
            },
        );
        settings
            .agents
            .insert("codex".into(), AgentPrefs { env: [("OPENAI_API_KEY".to_string(), "sk-x".to_string())].into(), ..AgentPrefs::default() });
        settings.save(&dir).unwrap();
        assert_eq!(AgentSettings::load(&dir), settings);
        let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join(SETTINGS_FILE)).unwrap()).unwrap();
        assert_eq!(saved["agents"]["codex"], json!({"env": {"OPENAI_API_KEY": "sk-x"}}), "what's unset isn't written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.join(SETTINGS_FILE)).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // Saved again over the old file.
        settings.agents.remove("codex");
        settings.save(&dir).unwrap();
        assert_eq!(AgentSettings::load(&dir), settings);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_settings_do_not_replace_the_previous_file() {
        let dir = folder("too-large");
        let original = AgentSettings::default();
        original.save(&dir).unwrap();
        let previous = std::fs::read(dir.join(SETTINGS_FILE)).unwrap();
        let mut settings = AgentSettings::default();
        settings.agents.insert(
            "custom".into(),
            AgentPrefs { env: (0..33).map(|i| (format!("TOKEN_{i}"), "x".repeat(super::MAX_TEXT))).collect(), ..AgentPrefs::default() },
        );
        assert!(settings.save(&dir).is_err());
        assert_eq!(std::fs::read(dir.join(SETTINGS_FILE)).unwrap(), previous);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relative_program_paths_are_absolute_before_the_agent_changes_directory() {
        let dir = std::env::current_dir().unwrap().join(format!("target/relative-agent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let agent = program(&dir, "agent");
        let relative = agent.strip_prefix(std::env::current_dir().unwrap()).unwrap();
        let found = find_program(&relative.to_string_lossy(), None).unwrap();
        assert!(found.is_absolute(), "{found:?}");
        assert_eq!(found, agent);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn damaged_settings_keep_what_can_be_read() {
        let dir = folder("damaged");
        for junk in [&b"not json"[..], b"[1,2]", b"{\"agents\": 5}", b"\xff\xfe", b""] {
            std::fs::write(dir.join(SETTINGS_FILE), junk).unwrap();
            assert_eq!(AgentSettings::load(&dir), AgentSettings::default());
        }
        let long = "x".repeat(super::MAX_TEXT + 1);
        let v = json!({"agents": {
            "custom": {"name": "Bot\u{7}\u{202E}", "command": 5, "args": ["a", 7, "b\u{0}", long, "c"], "env": {"A": "1", "B": 2, "C=D": "3", "": "4", "E": "5\u{0}"}},
            "codex": "nope",
            " ": {"command": "x"},
            "many": {"args": (0..1000).map(|i| i.to_string()).collect::<Vec<_>>()},
        }});
        let settings = AgentSettings::from_value(&v);
        let custom = settings.agent("custom");
        assert_eq!(custom.name, "Bot");
        assert_eq!(custom.command, "");
        assert_eq!(custom.args, ["a", "c"]);
        assert_eq!(custom.env.into_iter().collect::<Vec<_>>(), [("A".to_string(), "1".to_string())]);
        assert_eq!(settings.agent("codex"), AgentPrefs::default());
        assert!(!settings.agents.contains_key(" "));
        assert_eq!(settings.agent("many").args.len(), super::MAX_ITEMS);
        let agents: serde_json::Map<String, serde_json::Value> = (0..100).map(|i| (format!("a{i}"), json!({}))).collect();
        assert_eq!(AgentSettings::from_value(&json!({"agents": agents})).agents.len(), super::MAX_AGENTS);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn programs_are_found_on_the_path_without_a_shell() {
        let a = folder("path-a");
        let b = folder("path-b");
        let agent = program(&b, "claude-agent-acp");
        let path = std::env::join_paths([a.clone(), PathBuf::from("relative/dir"), b.clone()]).unwrap();
        assert_eq!(find_program("claude-agent-acp", Some(&path)), Some(if cfg!(windows) { agent.clone() } else { b.join("claude-agent-acp") }));
        assert_eq!(find_program("codex-acp", Some(&path)), None);
        assert_eq!(find_program("", Some(&path)), None);
        assert_eq!(find_program("claude-agent-acp", None), None);
        // A path is looked for as it is.
        assert_eq!(find_program(&agent.to_string_lossy(), None), Some(agent.clone()));
        assert_eq!(find_program(&a.join("nothing").to_string_lossy(), Some(&path)), None);
        // A folder, or (on Unix) a file nobody may run, is no program.
        std::fs::create_dir_all(a.join("codex-acp")).unwrap();
        assert_eq!(find_program("codex-acp", Some(&path)), None);
        #[cfg(unix)]
        {
            std::fs::write(a.join("not-runnable"), "").unwrap();
            assert_eq!(find_program("not-runnable", Some(&path)), None);
        }
        let _ = (std::fs::remove_dir_all(&a), std::fs::remove_dir_all(&b));
    }

    #[test]
    fn a_missing_agent_is_offered_with_what_to_install_and_never_started() {
        let dir = folder("missing");
        let host = host(&dir);
        let empty = OsString::new();
        let offers = offers_on(&host, Some(&empty));
        let ids: Vec<&str> = offers.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, ["acp:claude-code", "acp:codex", CUSTOM_ID]);
        for o in &offers[..2] {
            assert!(matches!(&o.readiness, Readiness::Install { how } if how.contains("install")), "{o:?}");
        }
        assert!(matches!(&offers[0].readiness, Readiness::Install { how } if how.contains("@agentclientprotocol/claude-agent-acp")));
        assert!(matches!(&offers[2].readiness, Readiness::Unavailable { why } if why.contains("Options › Assistant")), "{:?}", offers[2]);
        let e = make_on("acp:codex", &host, Some(&empty)).err().unwrap();
        assert!(e.contains("codex-acp"), "{e}");
        assert!(make_on("acp:nope", &host, Some(&empty)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_installed_adapter_or_npx_starts_the_preset() {
        let dir = folder("installed");
        let bin = folder("installed-bin");
        let host = host(&dir);
        let path = std::env::join_paths([bin.clone()]).unwrap();
        // Only npx: the presets with a package run through it.
        let npx = program(&bin, "npx");
        let spec = spec_for("acp:codex", &host, Some(&path)).unwrap();
        assert_eq!(spec.launch.program, npx);
        assert_eq!(spec.launch.args, [OsString::from("-y"), OsString::from("@agentclientprotocol/codex-acp")]);
        // The adapter itself, when installed.
        let adapter = program(&bin, "claude-agent-acp");
        let spec = spec_for("acp:claude-code", &host, Some(&path)).unwrap();
        assert_eq!((spec.launch.program.clone(), spec.launch.args.len()), (adapter, 0));
        assert_eq!((spec.id.as_str(), spec.label.as_str(), spec.author.as_str()), ("acp:claude-code", "Claude Code", "Claude Code"));
        assert_eq!(spec.reach, Reach::Mcp { cli: dir.join("wordcraft-cli"), room: SocketAddr::from((Ipv4Addr::LOCALHOST, 4567)) });
        let offers = offers_on(&host, Some(&path));
        assert!(offers[..2].iter().all(|o| o.readiness == Readiness::Ready), "{offers:?}");
        // No wordcraft-cli or no room's port: nothing can reach the document.
        let no_cli = Host { cli: None, ..host.clone() };
        assert!(matches!(spec_for("acp:claude-code", &no_cli, Some(&path)), Err(Readiness::Unavailable { why }) if why.contains("wordcraft-cli")));
        let no_room = Host { room: None, ..host.clone() };
        assert!(matches!(spec_for("acp:claude-code", &no_room, Some(&path)), Err(Readiness::Unavailable { .. })));
        let _ = (std::fs::remove_dir_all(&dir), std::fs::remove_dir_all(&bin));
    }

    #[test]
    fn the_settings_set_an_agents_command_arguments_and_environment_but_never_wordcrafts_key() {
        let dir = folder("custom");
        let bin = folder("custom-bin");
        let agent = program(&bin, "my-agent");
        let mut settings = AgentSettings::default();
        let env = [
            ("AGENT_MODE", "fast"),
            ("WORDCRAFT_CONTROL_KEY", "the-windows-key"),
            ("wordcraft_control_addr", "127.0.0.1:1"),
            ("WORDCRAFT_OTHER", "kept"),
        ];
        settings.agents.insert(
            "custom".into(),
            AgentPrefs {
                name: "My Agent".into(),
                command: "my-agent".into(),
                args: vec!["--acp".into()],
                env: env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            },
        );
        settings.agents.insert("claude-code".into(), AgentPrefs { command: agent.to_string_lossy().into_owned(), ..AgentPrefs::default() });
        settings.save(&dir).unwrap();
        let host = host(&dir);
        let path = std::env::join_paths([bin.clone()]).unwrap();
        let spec = spec_for(CUSTOM_ID, &host, Some(&path)).unwrap();
        assert_eq!((spec.id.as_str(), spec.label.as_str(), spec.author.as_str()), (CUSTOM_ID, "My Agent", "My Agent"));
        assert_eq!((spec.launch.program.clone(), spec.launch.args.clone()), (agent.clone(), vec![OsString::from("--acp")]));
        let mut env = spec.env.clone();
        env.sort();
        assert_eq!(env, [("AGENT_MODE".to_string(), "fast".to_string()), ("WORDCRAFT_OTHER".to_string(), "kept".to_string())]);
        // A preset's command, in place of its adapter.
        assert_eq!(spec_for("acp:claude-code", &host, None).unwrap().launch.program, agent);
        // A command that isn't there says so.
        settings.agents.insert("codex".into(), AgentPrefs { command: "/nowhere/codex-acp".into(), ..AgentPrefs::default() });
        settings.save(&dir).unwrap();
        assert!(matches!(spec_for("acp:codex", &host, Some(&path)), Err(Readiness::Install { how }) if how.contains("/nowhere/codex-acp")));
        let _ = (std::fs::remove_dir_all(&dir), std::fs::remove_dir_all(&bin));
    }
}

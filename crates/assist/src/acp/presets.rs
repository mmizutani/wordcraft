//! The ACP agents WordCraft knows ([`PRESETS`]: Claude Code, Codex, Pi) and the person's own
//! ([`CUSTOM_ID`]), as the assistant pane offers them: found on `PATH` (or run through `npx`),
//! with the command, arguments and environment the person set up for them in the agents'
//! settings file ([`AgentSettings`]). Built-in npx fallback asks a person before downloading
//! and running an adapter; missing tools are offered with what to install ([`Readiness::Install`]).

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

/// Added to [`SETTINGS_FILE`]'s name when [`AgentSettings::set_aside`] keeps a file that can't be
/// read: `assistant-agents.json.damaged`.
pub const SET_ASIDE_SUFFIX: &str = ".damaged";

/// The custom agent's id.
pub const CUSTOM_ID: &str = "acp:custom";

/// The custom agent's key in the agents' settings.
pub const CUSTOM_KEY: &str = "custom";

/// Most bytes of the settings file read.
const MAX_SETTINGS: u64 = 1 << 20;

/// Most agents the settings keep.
const MAX_AGENTS: usize = 16;

/// Most arguments, and environment variables, an agent's settings keep.
const MAX_ITEMS: usize = 64;

/// Longest command, argument, name or value kept.
const MAX_TEXT: usize = 32 << 10;

/// What an agent reaches the document through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// WordCraft's MCP server, which `session/new` gives it ([`Reach::Mcp`]).
    Mcp,
    /// Its own shell and `wordcraft-cli call`, since its adapter doesn't pass MCP servers on
    /// ([`Reach::Shell`]).
    Shell,
}

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
    /// Programs the adapter runs, which must be on `PATH` too.
    pub needs: &'static [&'static str],
    /// What to install when neither is there.
    pub install: &'static str,
    /// How to sign in when the agent needs it and offers no way WordCraft can run.
    pub sign_in: &'static str,
    /// How it reaches the document.
    pub via: Via,
}

/// Claude Code, through its ACP adapter (`@agentclientprotocol/claude-agent-acp`, which wraps
/// the Claude Agent SDK), with the person's Claude login.
pub const CLAUDE_CODE: Preset = Preset {
    id: "acp:claude-code",
    key: "claude-code",
    label: "Claude Code",
    program: "claude-agent-acp",
    package: Some("@agentclientprotocol/claude-agent-acp"),
    needs: &[],
    install: "install Claude Code's ACP adapter (`npm install -g @agentclientprotocol/claude-agent-acp`), or Node.js so WordCraft can run it with npx",
    sign_in: "run `claude` in a terminal and sign in, then try again",
    via: Via::Mcp,
};

/// Codex, through its ACP adapter (`@agentclientprotocol/codex-acp`), with the person's
/// ChatGPT login or an API key.
pub const CODEX: Preset = Preset {
    id: "acp:codex",
    key: "codex",
    label: "Codex",
    program: "codex-acp",
    package: Some("@agentclientprotocol/codex-acp"),
    needs: &[],
    install: "install Codex's ACP adapter (`npm install -g @agentclientprotocol/codex-acp`), or Node.js so WordCraft can run it with npx",
    sign_in: "run `codex login` in a terminal, or give Codex an API key (CODEX_API_KEY or OPENAI_API_KEY in its environment), then try again",
    via: Via::Mcp,
};

/// Pi, through its ACP adapter (`pi-acp`, which runs `pi --mode rpc`), with the model
/// providers the person signed Pi in to. The adapter doesn't pass MCP servers on, so Pi reaches
/// the document with its own shell tool and `wordcraft-cli call` ([`Via::Shell`]).
pub const PI: Preset = Preset {
    id: "acp:pi",
    key: "pi",
    label: "Pi",
    program: "pi-acp",
    package: Some("pi-acp"),
    needs: &["pi"],
    install: "install Pi (`npm install -g @earendil-works/pi-coding-agent`) and its ACP adapter (`npm install -g pi-acp`, or WordCraft runs it with npx)",
    sign_in: "run `pi` in a terminal and sign in, then try again",
    via: Via::Shell,
};

/// The agents WordCraft knows, in the picker's order.
pub const PRESETS: [Preset; 3] = [CLAUDE_CODE, CODEX, PI];

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
/// `pi`, `custom`), in [`SETTINGS_FILE`].
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

    /// The settings in `dir`, telling a missing file (`Ok(None)`) from one that is there but
    /// can't be read: too large to read whole, not JSON, not the settings' shape, or a read that
    /// failed (`Err`, saying why, never what it holds). Where [`AgentSettings::load`] shows such
    /// a file as nothing set up, a page that saves over it can say so first.
    pub fn read(dir: &Path) -> Result<Option<AgentSettings>, String> {
        Ok(AgentSettings::read_value(dir)?.map(|v| AgentSettings::from_value(&v)))
    }

    /// The settings file in `dir` as it is, all of it (fields and entries WordCraft doesn't
    /// read included), checked as [`AgentSettings::read`] checks it: `Ok(None)` when it is
    /// missing, `Err` saying why it can't be read.
    pub fn read_value(dir: &Path) -> Result<Option<Value>, String> {
        let file = match std::fs::File::open(dir.join(SETTINGS_FILE)) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.kind().to_string()),
        };
        let mut bytes = Vec::new();
        // One byte past the limit tells a file that is too large from one that just fits.
        file.take(MAX_SETTINGS.saturating_add(1)).read_to_end(&mut bytes).map_err(|e| e.kind().to_string())?;
        if bytes.len() as u64 > MAX_SETTINGS {
            return Err("it is larger than 1 MiB".into());
        }
        let v: Value = serde_json::from_slice(&bytes).map_err(|_| "it isn't JSON".to_string())?;
        let shaped = v.as_object().is_some_and(|o| o.get("agents").is_none_or(Value::is_object));
        if !shaped {
            return Err("it doesn't hold agents' settings".into());
        }
        Ok(Some(v))
    }

    /// The settings file `file` (as [`AgentSettings::read_value`] gives it) with what these
    /// settings change from `before` (what [`AgentSettings::from_value`] read in it) written
    /// into it, and nothing else: each agent's name, command and arguments where they differ,
    /// its variables one by one, and every other entry, field and value, those WordCraft doesn't
    /// read included, as the file has them.
    pub fn written_over(&self, before: &AgentSettings, file: &Value) -> Value {
        let mut out = file.as_object().cloned().unwrap_or_default();
        let mut agents = out.get("agents").and_then(Value::as_object).cloned().unwrap_or_default();
        let keys: std::collections::BTreeSet<&String> = before.agents.keys().chain(self.agents.keys()).collect();
        for key in keys {
            let (old, new) = (before.agent(key), self.agent(key));
            if old == new {
                continue;
            }
            let mut entry = agents.get(key.as_str()).and_then(Value::as_object).cloned().unwrap_or_default();
            let text = |s: &str| (!s.is_empty()).then(|| Value::String(s.to_string()));
            if old.name != new.name {
                put(&mut entry, "name", text(&new.name));
            }
            if old.command != new.command {
                put(&mut entry, "command", text(&new.command));
            }
            if old.args != new.args {
                put(&mut entry, "args", (!new.args.is_empty()).then(|| Value::from(new.args.clone())));
            }
            if old.env != new.env {
                let mut env = entry.get("env").and_then(Value::as_object).cloned().unwrap_or_default();
                for name in old.env.keys().chain(new.env.keys()) {
                    match (old.env.get(name), new.env.get(name)) {
                        (a, b) if a == b => {}
                        (_, Some(value)) => {
                            env.insert(name.clone(), Value::String(value.clone()));
                        }
                        (_, None) => {
                            env.remove(name);
                        }
                    }
                }
                put(&mut entry, "env", (!env.is_empty()).then_some(Value::Object(env)));
            }
            if entry.is_empty() {
                agents.remove(key.as_str());
            } else {
                agents.insert(key.clone(), Value::Object(entry));
            }
        }
        out.insert("agents".into(), Value::Object(agents));
        Value::Object(out)
    }

    /// Whether [`AgentSettings::save`] can write these settings and [`AgentSettings::load`] read
    /// them back whole: they fit in the 1 MiB it reads.
    pub fn fits(&self) -> bool {
        fits_in_a_file(self)
    }

    /// Whether [`AgentSettings::save_value`] can write `file` and [`AgentSettings::read_value`]
    /// read it back whole: it fits in the 1 MiB it reads.
    pub fn value_fits(file: &Value) -> bool {
        fits_in_a_file(file)
    }

    /// Put the file [`AgentSettings::set_aside`] set aside in `dir` back, when no settings file
    /// has taken its place: after a save that failed. Whether it was put back.
    pub fn put_back_aside(dir: &Path) -> std::io::Result<bool> {
        let path = dir.join(SETTINGS_FILE);
        if std::fs::symlink_metadata(&path).is_ok() {
            return Ok(false);
        }
        std::fs::rename(dir.join(format!("{SETTINGS_FILE}{SET_ASIDE_SUFFIX}")), &path)?;
        Ok(true)
    }

    /// Put a settings file that can't be read aside (as `assistant-agents.json.damaged`, with
    /// [`SET_ASIDE_SUFFIX`], over an older one), so saving new settings doesn't lose what it held.
    /// Where it went, or `None` when there was no file.
    pub fn set_aside(dir: &Path) -> std::io::Result<Option<PathBuf>> {
        let path = dir.join(SETTINGS_FILE);
        if std::fs::symlink_metadata(&path).is_err() {
            return Ok(None);
        }
        let aside = dir.join(format!("{SETTINGS_FILE}{SET_ASIDE_SUFFIX}"));
        std::fs::rename(&path, &aside)?;
        Ok(Some(aside))
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
        write_settings(dir, file_bytes(self)?)
    }

    /// Write `file` as the settings file in `dir`, as [`AgentSettings::save`] writes: whole or
    /// not at all, only its owner may read it, and never more than the 1 MiB it reads.
    pub fn save_value(dir: &Path, file: &Value) -> std::io::Result<()> {
        write_settings(dir, file_bytes(file)?)
    }

    /// What the person set up for the agent with settings key `key`.
    pub fn agent(&self, key: &str) -> AgentPrefs {
        self.agents.get(key).cloned().unwrap_or_default()
    }
}

/// Set `field` of a settings entry to `value`, or take it out.
fn put(entry: &mut serde_json::Map<String, Value>, field: &str, value: Option<Value>) {
    match value {
        Some(value) => {
            entry.insert(field.to_string(), value);
        }
        None => {
            entry.remove(field);
        }
    }
}

/// Write the settings file's `bytes` to `dir`: a new file renamed over the old one, so a failed
/// write leaves it as it was; on Unix only its owner may read or write it. More than the 1 MiB
/// the reader reads is refused.
/// Whether `v`, written compactly, fits in the 1 MiB the settings file may hold: counted as it is
/// written, stopping at the limit, with nothing kept.
fn fits_in_a_file<T: Serialize + ?Sized>(v: &T) -> bool {
    struct Count(u64);
    impl Write for Count {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(b.len() as u64);
            if self.0 > MAX_SETTINGS { Err(std::io::Error::other("too large")) } else { Ok(b.len()) }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Count(0), v).is_ok()
}

/// What the settings file holds for `v`: pretty-printed, or compact when only that fits in the
/// 1 MiB the file may hold (a large array WordCraft doesn't read, say); refused when neither does.
fn file_bytes<T: Serialize + ?Sized>(v: &T) -> std::io::Result<Vec<u8>> {
    let pretty = serde_json::to_vec_pretty(v).map_err(std::io::Error::other)?;
    if pretty.len() as u64 <= MAX_SETTINGS {
        return Ok(pretty);
    }
    drop(pretty);
    serde_json::to_vec(v).map_err(std::io::Error::other)
}

fn write_settings(dir: &Path, bytes: Vec<u8>) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
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

/// Windows' directly supported executable and batch-file names.
fn windows_program_name(path: &Path) -> bool {
    path.extension().and_then(OsStr::to_str).is_some_and(|ext| ["exe", "com", "bat", "cmd"].iter().any(|known| ext.eq_ignore_ascii_case(known)))
}

/// Whether `path` is a file the system can run.
fn runnable(path: &Path) -> bool {
    if cfg!(windows) && !windows_program_name(path) {
        return false;
    }
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
fn launch(preset: Option<&Preset>, prefs: &AgentPrefs, path: Option<&OsStr>) -> Result<(Launch, Option<String>), Readiness> {
    if !prefs.command.is_empty() {
        let shown = label(&prefs.command);
        return match find_program(&prefs.command, path) {
            Some(program) => Ok((Launch::new(program, prefs.args.iter().map(OsString::from)), None)),
            None => Err(Readiness::Install {
                how: format!("`{shown}` wasn't found: install it, or change the agent's command in File › Options › Assistant"),
            }),
        };
    }
    let Some(preset) = preset else {
        return Err(Readiness::Unavailable { why: "no custom agent is set up: give its command in File › Options › Assistant".into() });
    };
    if preset.needs.iter().any(|p| find_program(p, path).is_none()) {
        return Err(Readiness::Install { how: preset.install.to_string() });
    }
    if let Some(program) = find_program(preset.program, path) {
        return Ok((Launch::new(program, Vec::<OsString>::new()), None));
    }
    if let Some(package) = preset.package
        && let Some(npx) = find_program("npx", path)
    {
        return Ok((Launch::new(npx, ["-y", package]), Some(package.to_string())));
    }
    Err(Readiness::Install { how: preset.install.to_string() })
}

/// What reaching the document `via` needs of the host: WordCraft's command line tool and the
/// room's port.
fn reach(host: &Host, via: Via) -> Result<Reach, Readiness> {
    let cli = host.cli.clone().filter(|c| c.is_absolute()).ok_or_else(|| Readiness::Unavailable {
        why: "WordCraft's command line tool (wordcraft-cli), which agents reach the document through, wasn't found next to WordCraft".into(),
    })?;
    let room: SocketAddr =
        host.room.ok_or_else(|| Readiness::Unavailable { why: "the shared document has no port for agents to reach it (see the log)".into() })?;
    Ok(match via {
        Via::Mcp => Reach::Mcp { cli, room },
        Via::Shell => Reach::Shell { cli, room },
    })
}

/// The agents' settings on `host`.
fn settings_on(host: &Host) -> AgentSettings {
    host.settings.as_deref().map(AgentSettings::load).unwrap_or_default()
}

/// The agent `id` names on `host`, set up as its settings say, with programs looked up on
/// `path`: its spec, or why it can't start.
#[cfg(test)]
pub(super) fn spec_for(id: &str, host: &Host, path: Option<&OsStr>) -> Result<AgentSpec, Readiness> {
    spec_with(id, host, &settings_on(host), path, &[])
}

/// Common installed macOS package-manager bins, after the person's PATH.
fn common_paths() -> Vec<PathBuf> {
    if cfg!(target_os = "macos") { vec!["/opt/homebrew/bin".into(), "/usr/local/bin".into()] } else { Vec::new() }
}

/// One PATH for executable lookup and the child. Only trusted installed directories are added.
fn effective_path(path: Option<&OsStr>, extra: &[PathBuf]) -> Option<OsString> {
    #[cfg(unix)]
    {
        let mut dirs: Vec<PathBuf> = path.map(|p| std::env::split_paths(p).collect()).unwrap_or_default();
        for dir in extra {
            let Ok(dir) = dir.canonicalize() else { continue };
            if super::check_trusted_ancestors(&dir).is_ok() && !dirs.iter().any(|known| known.canonicalize().is_ok_and(|known| known == dir)) {
                dirs.push(dir);
            }
        }
        std::env::join_paths(dirs).ok().or_else(|| path.map(OsStr::to_os_string))
    }
    #[cfg(not(unix))]
    {
        let _ = extra;
        path.map(OsStr::to_os_string)
    }
}

/// The agent `id` names on `host`, set up as `settings` say, with programs looked up on `path`:
/// its spec, or why it can't start.
fn spec_with(id: &str, host: &Host, settings: &AgentSettings, path: Option<&OsStr>, extra: &[PathBuf]) -> Result<AgentSpec, Readiness> {
    let preset = PRESETS.iter().find(|p| p.id == id);
    let key = match preset {
        Some(p) => p.key,
        None if id == CUSTOM_ID => CUSTOM_KEY,
        None => return Err(Readiness::Unavailable { why: "WordCraft knows no such agent".into() }),
    };
    let prefs = settings.agent(key);
    let configured_path = prefs.env.iter().find(|(name, _)| if cfg!(windows) { name.eq_ignore_ascii_case("PATH") } else { name.as_str() == "PATH" });
    let path = effective_path(configured_path.map(|(_, value)| OsStr::new(value)).or(path), extra);
    let (launch, setup) = launch(preset, &prefs, path.as_deref())?;
    let reach = reach(host, preset.map_or(Via::Mcp, |p| p.via))?;
    let name = match preset {
        Some(p) => p.label.to_string(),
        None if prefs.name.is_empty() => "Custom agent".to_string(),
        None => prefs.name.clone(),
    };
    let mut spec = AgentSpec::new(&name, launch);
    spec.id = id.to_string();
    spec.setup = setup;
    spec.path = path;
    spec.reach = reach;
    spec.sign_in = preset.map_or_else(|| format!("sign in with {name} itself, then try again"), |p| p.sign_in.to_string());
    // WordCraft's own control key and address come from WordCraft only (a seat's, for an
    // agent that reaches the document through its shell), never from settings.
    spec.env = prefs.env.into_iter().filter(|(k, _)| ![super::KEY_VAR, super::ADDR_VAR].contains(&k.to_ascii_uppercase().as_str())).collect();
    Ok(spec)
}

/// The ACP agents `host` offers, in the picker's order: the presets, then the custom agent.
/// It reads the agents' settings and looks programs up on `PATH`, so the pane asks when it
/// opens the picker, not every frame.
pub fn offers(host: &Host) -> Vec<Offer> {
    offers_with_paths(host, std::env::var_os("PATH").as_deref(), &common_paths())
}

#[cfg(test)]
fn offers_on(host: &Host, path: Option<&OsStr>) -> Vec<Offer> {
    offers_with_paths(host, path, &[])
}

fn offers_with_paths(host: &Host, path: Option<&OsStr>, extra: &[PathBuf]) -> Vec<Offer> {
    let settings = settings_on(host);
    let ids = PRESETS.iter().map(|p| p.id).chain([CUSTOM_ID]);
    ids.map(|id| match spec_with(id, host, &settings, path, extra) {
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
    let name = settings.agent(CUSTOM_KEY).name;
    if name.is_empty() { "Custom agent".into() } else { name }
}

/// Make the ACP agent `id` names, as the settings set it up: an error that says what to install
/// or set up when it can't start.
pub fn make(id: &str, host: &Host) -> Result<Box<dyn Backend>, String> {
    make_with_paths(id, host, std::env::var_os("PATH").as_deref(), &common_paths())
}

#[cfg(test)]
fn make_on(id: &str, host: &Host, path: Option<&OsStr>) -> Result<Box<dyn Backend>, String> {
    make_with_paths(id, host, path, &[])
}

fn make_with_paths(id: &str, host: &Host, path: Option<&OsStr>, extra: &[PathBuf]) -> Result<Box<dyn Backend>, String> {
    match spec_with(id, host, &settings_on(host), path, extra) {
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
    use serde_json::Value;

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
    fn a_missing_file_is_told_from_one_that_cant_be_read() {
        let dir = folder("read");
        assert_eq!(AgentSettings::read(&dir), Ok(None), "no file is nothing set up");
        let mut settings = AgentSettings::default();
        settings.agents.insert("pi".into(), AgentPrefs { command: "/opt/pi".into(), ..AgentPrefs::default() });
        settings.save(&dir).unwrap();
        assert_eq!(AgentSettings::read(&dir), Ok(Some(settings.clone())));
        std::fs::write(dir.join(SETTINGS_FILE), "{}").unwrap();
        assert_eq!(AgentSettings::read(&dir), Ok(Some(AgentSettings::default())), "an empty object is settings with nothing set up");
        for (junk, why) in [
            (b"{\"agents\": {\"pi\": " as &[u8], "isn't JSON"),
            (b"\xff\xfe\x00junk", "isn't JSON"),
            (b"[1, 2, 3]", "doesn't hold"),
            (b"{\"agents\": [\"pi\"]}", "doesn't hold"),
            (b"\"agents\"", "doesn't hold"),
        ] {
            std::fs::write(dir.join(SETTINGS_FILE), junk).unwrap();
            let read = AgentSettings::read(&dir);
            assert!(read.as_ref().is_err_and(|e| e.contains(why)), "{junk:?} reads as {read:?}");
            assert_eq!(AgentSettings::load(&dir), AgentSettings::default(), "load still shows nothing");
        }
        // Larger than what's read: an error, not the first megabyte cut short.
        let mut big = b"{\"agents\": {}, \"pad\": \"".to_vec();
        big.resize(super::MAX_SETTINGS as usize + 8, b'x');
        big.extend_from_slice(b"\"}");
        std::fs::write(dir.join(SETTINGS_FILE), &big).unwrap();
        assert!(AgentSettings::read(&dir).is_err_and(|e| e.contains("larger")));
        // A folder where the file should be can't be read either.
        std::fs::remove_file(dir.join(SETTINGS_FILE)).unwrap();
        std::fs::create_dir(dir.join(SETTINGS_FILE)).unwrap();
        assert!(AgentSettings::read(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_settings_that_can_be_read_back_fit() {
        let mut settings = AgentSettings::default();
        assert!(settings.fits());
        settings.agents.insert(
            "custom".into(),
            AgentPrefs { env: (0..33).map(|i| (format!("TOKEN_{i}"), "x".repeat(super::MAX_TEXT))).collect(), ..AgentPrefs::default() },
        );
        assert!(!settings.fits(), "over 1 MiB");
        let dir = folder("fits");
        assert!(settings.save(&dir).is_err(), "what doesn't fit isn't saved");
        if let Some(prefs) = settings.agents.get_mut("custom") {
            prefs.env.retain(|k, _| k.len() < 8);
        }
        assert!(settings.fits());
        settings.save(&dir).unwrap();
        assert_eq!(AgentSettings::read(&dir), Ok(Some(settings)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_file_is_set_aside_not_lost() {
        let dir = folder("set-aside");
        assert!(AgentSettings::set_aside(&dir).unwrap().is_none(), "nothing to set aside");
        std::fs::write(dir.join(SETTINGS_FILE), "not json").unwrap();
        let aside = AgentSettings::set_aside(&dir).unwrap().unwrap();
        assert_eq!(std::fs::read(&aside).unwrap(), b"not json");
        assert_eq!(AgentSettings::read(&dir), Ok(None), "the settings file is gone");
        // A second damaged file replaces the first one set aside.
        std::fs::write(dir.join(SETTINGS_FILE), "still not json").unwrap();
        assert_eq!(AgentSettings::set_aside(&dir).unwrap(), Some(aside.clone()));
        assert_eq!(std::fs::read(&aside).unwrap(), b"still not json");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn written_over_changes_only_what_changed() {
        let file = json!({
            "version": 2,
            "agents": {
                "pi": {"command": "/opt/pi", "cwd": "/work", "env": {"PORT": 8080, "OK": "1"}},
                "codex": {"env": {"A": "1", "B": "2", "N": 5}, "extra": true, "args": ["--x", 7]},
                "future": "not an object"
            }
        });
        let before = AgentSettings::from_value(&file);
        assert_eq!(before.agent("pi").env.len(), 1, "the lenient reader skips PORT");
        // Nothing changed: the file as it is.
        assert_eq!(before.written_over(&before, &file), file);
        let mut after = before.clone();
        if let Some(codex) = after.agents.get_mut("codex") {
            codex.env.remove("A");
            codex.env.insert("B".into(), "two".into());
            codex.env.insert("C".into(), "3".into());
        }
        after.agents.insert("custom".into(), AgentPrefs { command: "/opt/mine".into(), ..AgentPrefs::default() });
        let out = after.written_over(&before, &file);
        assert_eq!(out["version"], 2);
        assert_eq!(out["agents"]["pi"], file["agents"]["pi"], "an agent not changed is as the file has it");
        assert_eq!(out["agents"]["future"], "not an object");
        assert_eq!(out["agents"]["codex"], json!({"env": {"B": "two", "C": "3", "N": 5}, "extra": true, "args": ["--x", 7]}));
        assert_eq!(out["agents"]["custom"], json!({"command": "/opt/mine"}));
        assert_eq!(AgentSettings::from_value(&out), after, "and it reads back as the settings");
        // An agent with nothing left is taken out; one with fields WordCraft doesn't read keeps
        // those.
        let mut cleared = after.clone();
        cleared.agents.remove("custom");
        cleared.agents.remove("codex");
        let out = cleared.written_over(&after, &out);
        assert!(out["agents"].get("custom").is_none());
        assert_eq!(out["agents"]["codex"], json!({"env": {"N": 5}, "extra": true}), "its arguments cleared too");
        // A missing or damaged file is written from nothing: each agent set up.
        let fresh = AgentSettings::from_value(&after.written_over(&AgentSettings::default(), &Value::Null));
        let set_up: Vec<_> = after.agents.iter().filter(|(_, p)| **p != AgentPrefs::default()).collect();
        assert_eq!(fresh.agents.iter().collect::<Vec<_>>(), set_up);
    }

    #[test]
    fn a_whole_file_value_saves_and_reads_back_but_not_past_the_limit() {
        let dir = folder("save-value");
        assert_eq!(AgentSettings::read_value(&dir), Ok(None));
        let file = json!({"version": 2, "agents": {"pi": {"command": "/opt/pi", "cwd": "/work"}}});
        AgentSettings::save_value(&dir, &file).unwrap();
        assert_eq!(AgentSettings::read_value(&dir), Ok(Some(file.clone())));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.join(SETTINGS_FILE)).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let big = json!({"agents": {}, "pad": "x".repeat(super::MAX_SETTINGS as usize)});
        assert!(AgentSettings::save_value(&dir, &big).is_err());
        assert_eq!(AgentSettings::read_value(&dir), Ok(Some(file)), "the previous file is kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_only_pretty_printing_takes_past_the_limit_is_written_compact() {
        let dir = folder("compact");
        // About 600 KB compact, over 2 MiB pretty-printed.
        let file = json!({"agents": {"codex": {"env": {"OPENAI_API_KEY": "sk-old"}}}, "log": vec![0; 300_000]});
        assert!(AgentSettings::value_fits(&file));
        AgentSettings::save_value(&dir, &file).unwrap();
        assert!(std::fs::metadata(dir.join(SETTINGS_FILE)).unwrap().len() <= super::MAX_SETTINGS);
        assert_eq!(AgentSettings::read_value(&dir), Ok(Some(file)));
        let big = json!({"agents": {}, "pad": "x".repeat(super::MAX_SETTINGS as usize)});
        assert!(!AgentSettings::value_fits(&big));
        // Small settings are still pretty-printed, for a person editing the file by hand.
        let small = json!({"agents": {"pi": {"command": "/opt/pi"}}});
        AgentSettings::save_value(&dir, &small).unwrap();
        assert!(std::fs::read_to_string(dir.join(SETTINGS_FILE)).unwrap().contains('\n'));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_set_aside_is_put_back_only_where_none_took_its_place() {
        let dir = folder("put-back-aside");
        let path = dir.join(SETTINGS_FILE);
        std::fs::write(&path, "junk").unwrap();
        assert!(AgentSettings::set_aside(&dir).unwrap().is_some());
        assert!(!path.exists());
        assert!(AgentSettings::put_back_aside(&dir).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "junk");
        assert!(AgentSettings::set_aside(&dir).unwrap().is_some());
        std::fs::write(&path, "{}").unwrap();
        assert!(!AgentSettings::put_back_aside(&dir).unwrap(), "a new file stays");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_configured_path_replaces_the_inherited_one() {
        // What the page's notes say: a PATH set for the agent is the only PATH its program is
        // looked for on (on macOS the trusted desktop bins come after it), not one searched first.
        let dir = folder("path-replaces");
        let inherited = folder("path-replaces-inherited");
        let configured = folder("path-replaces-configured");
        program(&inherited, "my-inherited-agent");
        let mut settings = AgentSettings::default();
        let path = std::env::join_paths([configured.clone()]).unwrap().into_string().unwrap();
        settings.agents.insert(
            "custom".into(),
            AgentPrefs { command: "my-inherited-agent".into(), env: [("PATH".into(), path)].into(), ..AgentPrefs::default() },
        );
        settings.save(&dir).unwrap();
        let on = std::env::join_paths([inherited.clone()]).unwrap();
        assert!(matches!(spec_for(CUSTOM_ID, &host(&dir), Some(&on)), Err(Readiness::Install { .. })), "not found on WordCraft's PATH");
        settings.agents.insert("custom".into(), AgentPrefs { command: "my-inherited-agent".into(), ..AgentPrefs::default() });
        settings.save(&dir).unwrap();
        assert!(spec_for(CUSTOM_ID, &host(&dir), Some(&on)).is_ok(), "found there without one");
        let _ = (std::fs::remove_dir_all(&dir), std::fs::remove_dir_all(&inherited), std::fs::remove_dir_all(&configured));
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
    fn a_configured_path_is_used_to_find_the_agent_it_will_start() {
        let dir = folder("configured-path");
        let bin = folder("configured-path-bin");
        let agent = program(&bin, "my-private-agent");
        let path = std::env::join_paths([bin.clone()]).unwrap().into_string().unwrap();
        let mut settings = AgentSettings::default();
        settings.agents.insert(
            "custom".into(),
            AgentPrefs { command: "my-private-agent".into(), env: [("PATH".into(), path.clone())].into(), ..AgentPrefs::default() },
        );
        settings.save(&dir).unwrap();
        let spec = spec_for(CUSTOM_ID, &host(&dir), None).unwrap();
        assert_eq!(spec.launch.program, agent);
        assert!(spec.env.contains(&("PATH".into(), path)));
        let _ = (std::fs::remove_dir_all(&dir), std::fs::remove_dir_all(&bin));
    }

    #[cfg(unix)]
    #[test]
    fn desktop_fallback_paths_are_trusted_installed_unique_and_after_configured_paths() {
        let mut private = crate::acp::Folder::new().unwrap();
        let bin = private.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let unsafe_dir = folder("untrusted-fallback");
        std::fs::set_permissions(&unsafe_dir, std::os::unix::fs::PermissionsExt::from_mode(0o777)).unwrap();
        let configured = OsString::from("/usr/bin:/bin");
        let path = super::effective_path(Some(&configured), &[bin.clone(), bin.clone(), unsafe_dir.clone(), bin.join("missing")]).unwrap();
        let dirs: Vec<_> = std::env::split_paths(&path).collect();
        assert_eq!(dirs, [PathBuf::from("/usr/bin"), PathBuf::from("/bin"), bin.canonicalize().unwrap()]);
        let fallback = program(&bin, "fallback-agent");
        let mut settings = AgentSettings::default();
        settings.agents.insert(
            "custom".into(),
            AgentPrefs { command: "fallback-agent".into(), env: [("PATH".into(), "/usr/bin:/bin".into())].into(), ..AgentPrefs::default() },
        );
        let spec =
            super::spec_with(CUSTOM_ID, &host(private.path()), &settings, Some(OsString::from("/unused").as_os_str()), std::slice::from_ref(&bin))
                .unwrap();
        assert_eq!(spec.launch.program, fallback);
        assert_eq!(spec.path, Some(path));
        assert!(spec.setup.is_none());
        private.remove();
        let _ = std::fs::remove_dir_all(unsafe_dir);
    }

    #[test]
    fn windows_program_names_reject_npm_shell_and_powershell_shims() {
        let dir = folder("windows-shims");
        for name in ["npx", "npx.ps1", "npx.cmd", "agent.EXE"] {
            std::fs::write(dir.join(name), "shim").unwrap();
        }
        assert!(!super::windows_program_name(&dir.join("npx")));
        assert!(!super::windows_program_name(&dir.join("npx.ps1")));
        assert!(super::windows_program_name(&dir.join("npx.cmd")));
        assert!(super::windows_program_name(&dir.join("agent.EXE")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_agent_is_offered_with_what_to_install_and_never_started() {
        let dir = folder("missing");
        let host = host(&dir);
        let empty = OsString::new();
        let offers = offers_on(&host, Some(&empty));
        let ids: Vec<&str> = offers.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, ["acp:claude-code", "acp:codex", "acp:pi", CUSTOM_ID]);
        for o in &offers[..3] {
            assert!(matches!(&o.readiness, Readiness::Install { how } if how.contains("install")), "{o:?}");
        }
        assert!(matches!(&offers[0].readiness, Readiness::Install { how } if how.contains("@agentclientprotocol/claude-agent-acp")));
        assert!(matches!(&offers[3].readiness, Readiness::Unavailable { why } if why.contains("Options › Assistant")), "{:?}", offers[3]);
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
        assert_eq!(spec.setup.as_deref(), Some("@agentclientprotocol/codex-acp"));
        assert_eq!(spec.launch.args, [OsString::from("-y"), OsString::from("@agentclientprotocol/codex-acp")]);
        // The adapter itself, when installed.
        let adapter = program(&bin, "claude-agent-acp");
        let spec = spec_for("acp:claude-code", &host, Some(&path)).unwrap();
        assert!(spec.setup.is_none());
        assert_eq!((spec.launch.program.clone(), spec.launch.args.len()), (adapter, 0));
        assert_eq!((spec.id.as_str(), spec.label.as_str(), spec.author.as_str()), ("acp:claude-code", "Claude Code", "Claude Code"));
        assert_eq!(spec.reach, Reach::Mcp { cli: dir.join("wordcraft-cli"), room: SocketAddr::from((Ipv4Addr::LOCALHOST, 4567)) });
        // Pi's adapter runs `pi`, which npx doesn't bring: Pi is offered once it's there too, and
        // reaches the document through its shell.
        let pi = spec_for("acp:pi", &host, Some(&path));
        assert!(matches!(&pi, Err(Readiness::Install { how }) if how.contains("@earendil-works/pi-coding-agent")), "{:?}", pi.map(|s| s.id));
        program(&bin, "pi");
        let spec = spec_for("acp:pi", &host, Some(&path)).unwrap();
        assert_eq!(spec.launch.args, [OsString::from("-y"), OsString::from("pi-acp")]);
        assert_eq!(spec.reach, Reach::Shell { cli: dir.join("wordcraft-cli"), room: SocketAddr::from((Ipv4Addr::LOCALHOST, 4567)) });
        let offers = offers_on(&host, Some(&path));
        assert!(offers[..3].iter().all(|o| o.readiness == Readiness::Ready), "{offers:?}");
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

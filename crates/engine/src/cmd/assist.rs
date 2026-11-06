//! The built-in assistant (#15): the Assistant pane and its conversations (`assist.*`).
//!
//! A conversation is the window's. The assistant (`wordcraft-assist`) sits above the engine and
//! joins the shared document as a participant of its own, so these commands check what they are
//! given and hand the window a request ([`Session::push_ui_request`], an `assist` key naming what
//! to do), which the window acts on as the command returns. The window keeps
//! [`AssistantStatus`] up to date, and the commands are greyed out when there is no assistant to
//! ask. A session without a window (the MCP server, `wordcraft-cli run`) has none: there only the
//! pane toggle and the setting do anything.
//!
//! The setting `assist.defaultRole` is the person's (`Prefs::assistant`): the role assistants
//! join a shared document with, never owner.

use serde_json::{Value, json};

use crate::{AssistantPrefs, CmdError, CmdResult, CommandSpec, Session};

/// Most bytes of a prompt; the assistant refuses more.
pub const MAX_PROMPT: usize = 64 << 10;

/// Longest backend id taken, in characters.
const MAX_ID_CHARS: usize = 128;

/// What the window's assistant can do now. The window sets it every frame; a session without a
/// window keeps the default: no assistant.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AssistantStatus {
    /// A window takes the `assist` requests.
    pub window: bool,
    /// An assistant is set up: a prompt starts a turn.
    pub ready: bool,
    /// A turn is running.
    pub busy: bool,
}

/// Why `assist.*` does nothing here.
const NO_WINDOW: &str = "the assistant works in the WordCraft window, with the Assistant pane";
/// Why a prompt can't start a turn yet. The tooltip on the assistant's buttons.
pub const NOT_SET_UP: &str = "Set up an assistant first: open the Assistant pane (Home › Assistant) to sign in or set up an agent";
/// Why a prompt waits.
const ANSWERING: &str = "the assistant is still answering: stop it first";
/// Why there is nothing to stop.
const NOT_ANSWERING: &str = "the assistant isn't answering";

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("assist.pane", "Assistant", "Home › Assistant", pane)
            .params(r#"{"value"?: bool (omit to toggle)}"#)
            .key("Mod+Alt+Shift+A")
            .pure(),
        CommandSpec::new("assist.ask", "Ask the Assistant", "Home › Assistant", ask)
            .params(r#"{"prompt": string, "role"?: "viewer|commenter|suggester|editor" (this conversation's; another role than the conversation's starts a new one)}"#)
            .when(can_ask)
            .pure(),
        CommandSpec::new("assist.cancel", "Stop", "Home › Assistant", |s, _| {
            s.push_ui_request(json!({"assist": "cancel"}));
            Ok(json!({"stopping": true}))
        })
        .when(answering)
        .pure(),
        CommandSpec::new("assist.useDocument", "Use this document", "Home › Assistant", |s, _| {
            s.push_ui_request(json!({"assist": "useDocument"}));
            Ok(json!({"switched": true}))
        })
        .when(in_window)
        .pure(),
        CommandSpec::new("assist.backend", "Assistant", "Home › Assistant", backend)
            .params(r#"{"id"?: string (one the window offers; omit to list them)} → the assistants offered, whether each is ready, and the one chosen"#)
            .when(in_window)
            .pure(),
        CommandSpec::new("assist.defaultRole", "Assistants Join As", "File › Options › Assistant", default_role)
            .params(r#"{"value"?: "viewer|commenter|suggester|editor" (omit to read it; never owner)}"#)
            .pure(),
    ]
}

fn in_window(s: &Session) -> Option<&'static str> {
    (!s.assistant.window).then_some(NO_WINDOW)
}

/// A prompt or a writing action can start a turn: a window with an assistant set up and not
/// answering.
pub fn can_ask(s: &Session) -> Option<&'static str> {
    if !s.assistant.window {
        Some(NO_WINDOW)
    } else if !s.assistant.ready {
        Some(NOT_SET_UP)
    } else if s.assistant.busy {
        Some(ANSWERING)
    } else {
        None
    }
}

fn answering(s: &Session) -> Option<&'static str> {
    in_window(s).or((!s.assistant.busy).then_some(NOT_ANSWERING))
}

/// Show or hide the Assistant pane; the window puts the keyboard in its prompt box when it opens.
fn pane(s: &mut Session, v: &Value) -> CmdResult {
    let open = match v.get("value") {
        None | Some(Value::Null) => !s.view.assistant_pane,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(CmdError::Params("`value` must be true or false".into())),
    };
    s.view.assistant_pane = open;
    s.push_ui_request(json!({"assist": "pane", "open": open}));
    Ok(json!({"value": open}))
}

/// The prompt in `v["prompt"]`: trimmed, not empty, at most [`MAX_PROMPT`] bytes.
pub(crate) fn prompt_param(v: &Value, key: &str) -> Result<String, CmdError> {
    let text = match v.get(key) {
        Some(Value::String(t)) => t.trim(),
        None | Some(Value::Null) => return Err(CmdError::Params(format!("missing `{key}`: what the assistant should do"))),
        Some(_) => return Err(CmdError::Params(format!("`{key}` must be text"))),
    };
    if text.is_empty() {
        return Err(CmdError::Params(format!("`{key}` is empty: say what the assistant should do")));
    }
    if text.len() > MAX_PROMPT {
        return Err(CmdError::Params(format!("`{key}` is longer than {} KB", MAX_PROMPT >> 10)));
    }
    Ok(text.to_string())
}

/// The role in `v["role"]`, by its name: one an assistant may have, never owner.
fn role_param(v: &Value) -> Result<Option<&'static str>, CmdError> {
    match v.get("role") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(name)) => AssistantPrefs::role_named(name).map(|r| Some(r.name())).map_err(CmdError::Params),
        Some(_) => Err(CmdError::Params("`role`: viewer, commenter, suggester or editor".into())),
    }
}

/// Ask the window's assistant: it starts a conversation first when there is none (sharing the
/// document, if it isn't shared), or a new one for another role.
fn ask(s: &mut Session, v: &Value) -> CmdResult {
    let prompt = prompt_param(v, "prompt")?;
    let role = role_param(v)?;
    s.push_ui_request(json!({"assist": "ask", "prompt": prompt, "role": role}));
    Ok(json!({"asked": true}))
}

/// Choose the assistant new conversations use, or list them.
fn backend(s: &mut Session, v: &Value) -> CmdResult {
    let id = match v.get("id") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) => {
            let id = id.trim();
            if id.is_empty() || id.chars().count() > MAX_ID_CHARS || id.chars().any(char::is_control) {
                return Err(CmdError::Params(format!("`id`: an assistant's id, as `assist.backend` lists them (at most {MAX_ID_CHARS} characters)")));
            }
            Some(id.to_string())
        }
        Some(_) => return Err(CmdError::Params("`id` must be text".into())),
    };
    s.push_ui_request(json!({"assist": "backend", "id": id}));
    Ok(json!({"id": id}))
}

/// The role assistants join a shared document with (File › Options › Assistant).
fn default_role(s: &mut Session, v: &Value) -> CmdResult {
    match v.get("value") {
        None | Some(Value::Null) => {}
        Some(Value::String(name)) => s.prefs.assistant.default_role = AssistantPrefs::role_named(name).map_err(CmdError::Params)?,
        Some(_) => return Err(CmdError::Params("`value`: viewer, commenter, suggester or editor".into())),
    }
    Ok(json!({"value": s.prefs.assistant.default_role().name()}))
}

#[cfg(test)]
mod tests;

//! The built-in assistant (#15): the Assistant pane and its conversations (`assist.*`), and the
//! writing actions that ask it to work on the selection (#18): Draft, Rewrite, Make Shorter, Make
//! Longer, Proofread and Summarize.
//!
//! A conversation is the window's. The assistant (`wordcraft-assist`) sits above the engine and
//! joins the shared document as a participant of its own, so these commands check what they are
//! given and hand the window a request ([`Session::push_ui_request`], an `assist` key naming what
//! to do), which the window acts on as the command returns. The window keeps
//! [`AssistantStatus`] up to date, and the commands are greyed out when there is no assistant to
//! ask. A session without a window (the MCP server, `wordcraft-cli run`) has none: there only the
//! pane toggle and the setting do anything.
//!
//! The settings are the person's (`Prefs::assistant`): `assist.defaultRole`, the role
//! assistants join a shared document with, never owner; and `assist.allowUnattended`, whether an
//! assistant goes ahead when its agent asks permission in a turn a program asked for, which no
//! one is there to answer (refused unless the person allows it). In the person's own turns the
//! Assistant pane asks them.
//!
//! `assist.showChange` selects what one of the assistant's tool calls changed, by the version of
//! the shared document it made.
//!
//! A writing action is a prompt of ours ([`prompts`]) asked like any other, so its edits are the
//! assistant's: tracked changes in its name while it is a suggester. The window gives the
//! assistant's copy the person's selection, or the caret at its end (`"at": "end"`) for what is
//! written after it.

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
    /// No assistant runs in this app: the web app's backends would sign in or start agents,
    /// which take the desktop app (#26), so the buttons say that rather than how to set one up.
    pub desktop_only: bool,
}

/// Why `assist.*` does nothing here.
const NO_WINDOW: &str = "the assistant works in the WordCraft window, with the Assistant pane";
/// Why a prompt can't start a turn yet. The tooltip on the assistant's buttons.
pub const NOT_SET_UP: &str = "Set up an assistant first: open the Assistant pane (Home › Assistant) to sign in or set up an agent";
/// Why a prompt can't start a turn in the web app.
pub const NEEDS_DESKTOP: &str = "The assistant needs the WordCraft desktop app: a web page can't sign in to ChatGPT or start an agent";
/// Why a prompt waits.
const ANSWERING: &str = "the assistant is still answering: stop it first";
/// Why there is nothing to stop.
const NOT_ANSWERING: &str = "the assistant isn't answering";
/// Why an action on the selected text waits.
const SELECT_FIRST: &str = "select the text first";
/// Why the assistant cannot use a block selection.
const CONTINUOUS_SELECTION: &str = "select one continuous range of text before asking the assistant";

/// Longest tone `assist.rewrite` takes, in characters.
const MAX_TONE_CHARS: usize = 40;

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
        CommandSpec::new("assist.allowUnattended", "Allow Requests No One Can Answer", "File › Options › Assistant", allow_unattended)
            .params(r#"{"value"?: bool (omit to read it; true only while it is on: the person turns it on in File › Options › Assistant)} → whether an assistant goes ahead when its agent asks permission in a turn a program asked for (control channel, MCP), which no one is there to answer; false (the default): the request is refused, and the answer logged. Turning it off holds from a running turn's next request"#)
            .pure(),
        CommandSpec::new("assist.showChange", "Show the Change", "Home › Assistant", show_change)
            .params(r#"{"version": number (a tool call's last: the shared document's version once its changes were in), "from"?: number (its first; omit for one change)} → the text the conversation's assistant put in or changed in those versions is selected (others' changes among them left out), and the Review tab opens"#)
            .when(in_window)
            .pure(),
        CommandSpec::new("assist.draft", "Draft with the Assistant", "Home › Assistant", draft)
            .params(r#"{"about": string (what to write)} → the assistant writes it at the caret (after the selection, if there is one)"#)
            .when(can_ask)
            .pure(),
        CommandSpec::new("assist.rewrite", "Rewrite", "Home › Assistant", rewrite)
            .params(r#"{"tone"?: string (e.g. "formal", "friendly"; at most 40 characters)} → the selected text, rewritten as the assistant's changes"#)
            .when(can_ask_selection)
            .pure(),
        CommandSpec::new("assist.shorten", "Make Shorter", "Home › Assistant", |s, _| act(s, prompts::SHORTEN.to_string(), false))
            .when(can_ask_selection)
            .pure(),
        CommandSpec::new("assist.expand", "Make Longer", "Home › Assistant", |s, _| act(s, prompts::EXPAND.to_string(), false))
            .when(can_ask_selection)
            .pure(),
        CommandSpec::new("assist.proofread", "Proofread", "Home › Assistant", |s, _| act(s, prompts::PROOFREAD.to_string(), false))
            .when(can_ask_selection)
            .pure(),
        CommandSpec::new("assist.summarize", "Summarize", "Home › Assistant", summarize)
            .params(r#"{"insert"?: bool (write the summary after the selection instead of in a comment on it)} → a summary of the selected text, or of the document when nothing is selected"#)
            .when(can_ask)
            .pure(),
    ]
}

fn in_window(s: &Session) -> Option<&'static str> {
    (!s.assistant.window).then_some(NO_WINDOW)
}

/// A prompt or a writing action can start a turn: a window with an assistant set up and not
/// answering, with a selection the assistant's copy can represent.
pub fn can_ask(s: &Session) -> Option<&'static str> {
    if !s.assistant.window {
        Some(NO_WINDOW)
    } else if !s.assistant.ready {
        Some(if s.assistant.desktop_only { NEEDS_DESKTOP } else { NOT_SET_UP })
    } else if s.assistant.busy {
        Some(ANSWERING)
    } else if s.column_segments().is_some() {
        // shortcut: block segments need context and selection transport before assistant turns can use them.
        Some(CONTINUOUS_SELECTION)
    } else {
        None
    }
}

/// An action on the selected text can start: [`can_ask`], with text selected.
pub fn can_ask_selection(s: &Session) -> Option<&'static str> {
    can_ask(s).or_else(|| s.sel.is_collapsed().then_some(SELECT_FIRST))
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

/// Ask a writing action's `prompt`; `at_end`: the assistant starts with the caret at the end of
/// the selection instead of with the selection.
fn act(s: &mut Session, prompt: String, at_end: bool) -> CmdResult {
    if prompt.len() > MAX_PROMPT {
        return Err(CmdError::Params(format!("what to write is longer than {} KB", MAX_PROMPT >> 10)));
    }
    let request = if at_end {
        json!({"assist": "ask", "prompt": prompt, "role": null, "at": "end"})
    } else {
        json!({"assist": "ask", "prompt": prompt, "role": null})
    };
    s.push_ui_request(request);
    Ok(json!({"asked": true}))
}

/// Draft new text about `about` at the caret.
fn draft(s: &mut Session, v: &Value) -> CmdResult {
    let about = prompt_param(v, "about")?;
    act(s, prompts::DRAFT.replace("{about}", &about), true)
}

/// Rewrite the selected text, in the tone asked for if any.
fn rewrite(s: &mut Session, v: &Value) -> CmdResult {
    let tone = match v.get("tone") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(t)) => {
            let t = t.trim();
            if t.chars().count() > MAX_TONE_CHARS || t.chars().any(char::is_control) {
                return Err(CmdError::Params(format!("`tone`: a word or two, such as \"formal\" (at most {MAX_TONE_CHARS} characters)")));
            }
            if t.is_empty() { String::new() } else { prompts::TONE.replace("{tone}", t) }
        }
        Some(_) => return Err(CmdError::Params("`tone` must be text".into())),
    };
    act(s, prompts::REWRITE.replace("{tone}", &tone), false)
}

/// Summarize the selected text, or the document with nothing selected, in a comment or after it.
fn summarize(s: &mut Session, v: &Value) -> CmdResult {
    let insert = match v.get("insert") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(CmdError::Params("`insert` must be true or false".into())),
    };
    let what = if s.sel.is_collapsed() { prompts::THE_DOCUMENT } else { prompts::THE_SELECTION };
    let template = if insert { prompts::SUMMARIZE_HERE } else { prompts::SUMMARIZE };
    act(s, template.replace("{what}", what), insert)
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

/// Whether an assistant goes ahead when its agent asks permission in a turn no one can answer
/// (File › Options › Assistant). It is the person's consent: a command, which any script can
/// run, reads it and turns it off, never on; the window's checkbox turns it on for the person's
/// own click.
fn allow_unattended(s: &mut Session, v: &Value) -> CmdResult {
    match v.get("value") {
        None | Some(Value::Null) => {}
        Some(Value::Bool(false)) => s.prefs.assistant.allow_unattended = false,
        Some(Value::Bool(true)) if s.prefs.assistant.allow_unattended => {}
        Some(Value::Bool(true)) => {
            return Err(CmdError::Failed("only you can allow what agents ask when no one can answer: File › Options › Assistant".into()));
        }
        Some(_) => return Err(CmdError::Params("`value` must be true or false".into())),
    }
    Ok(json!({"value": s.prefs.assistant.allow_unattended}))
}

/// Show the change an assistant's tool call made: the window selects it in the shared document.
fn show_change(s: &mut Session, v: &Value) -> CmdResult {
    let version = match v.get("version") {
        Some(Value::Number(n)) => n.as_u64().filter(|v| *v > 0),
        None | Some(Value::Null) => return Err(CmdError::Params("missing `version`: a tool call's, as the conversation gives it".into())),
        Some(_) => None,
    };
    let version = version.ok_or_else(|| CmdError::Params("`version`: a tool call's, a whole number from 1".into()))?;
    let from = match v.get("from") {
        None | Some(Value::Null) => version,
        Some(f) => f
            .as_u64()
            .filter(|f| (1..=version).contains(f))
            .ok_or_else(|| CmdError::Params("`from`: a tool call's first version, from 1 to `version`".into()))?,
    };
    s.push_ui_request(json!({"assist": "show", "version": version, "from": from}));
    Ok(json!({"version": version, "from": from}))
}

/// What the writing actions ask the assistant (#18): our own words, in one place. The window
/// sends the document around the selection with each prompt (`wordcraft_assist::Context`) and
/// gives the assistant's copy the selection, so they speak of "the selected text" and "the
/// caret". The actions' names in menus are the interface's (translated); these stay English.
pub mod prompts {
    /// `assist.draft`: `{about}` is what to write.
    pub const DRAFT: &str = "Draft new text about this: {about}\n\nWrite it at the caret with type_text, in the document's language and style, as sentences or paragraphs that fit where the caret is. Don't change the text that is already there.";
    /// `assist.rewrite`: `{tone}` is [`TONE`] with the tone asked for, or nothing.
    pub const REWRITE: &str = "Rewrite the selected text so it reads better{tone}, keeping what it says and its language. It is selected in your copy of the document: replace it with type_text, and change nothing outside it.";
    /// The tone `assist.rewrite` was asked for, in [`REWRITE`].
    pub const TONE: &str = ", in a {tone} tone";
    /// `assist.shorten`.
    pub const SHORTEN: &str = "Make the selected text shorter: keep what it says and drop what it doesn't need, in its language and style. It is selected in your copy of the document: replace it with type_text, and change nothing outside it.";
    /// `assist.expand`.
    pub const EXPAND: &str = "Make the selected text longer: add the detail, examples or explanation it needs, keeping what it says, its language and its style. It is selected in your copy of the document: replace it with type_text, and change nothing outside it.";
    /// `assist.proofread`.
    pub const PROOFREAD: &str = "Proofread the selected text: correct its spelling, grammar and punctuation, and nothing else, keeping its wording and style. It is selected in your copy of the document: replace it with the corrected text with type_text, and change nothing outside it. If nothing needs correcting, change nothing and say so.";
    /// `assist.summarize`: `{what}` is [`THE_SELECTION`] or [`THE_DOCUMENT`].
    pub const SUMMARIZE: &str = "Summarize {what} in a few sentences, in its language; get_text reads what the context leaves out. Put the summary in a comment on it: call execute with the command review.newComment and the params {\"text\": the summary}. Don't change the text.";
    /// `assist.summarize {insert: true}`: the caret is at the end of the selection.
    pub const SUMMARIZE_HERE: &str = "Summarize {what} in a few sentences, in its language; get_text reads what the context leaves out. Write the summary at the caret with type_text, as a paragraph of its own: start a new paragraph first (execute text.newParagraph) unless the caret is in an empty one. Change nothing else.";
    /// What [`SUMMARIZE`] and [`SUMMARIZE_HERE`] summarize with text selected.
    pub const THE_SELECTION: &str = "the selected text";
    /// … and with nothing selected.
    pub const THE_DOCUMENT: &str = "the document";
}

#[cfg(test)]
mod tests;

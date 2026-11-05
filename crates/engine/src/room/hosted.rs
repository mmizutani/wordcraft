//! Copies the room's process hosts for participants who joined with a key ([`super::Lobby`]):
//! each answers its participant's requests, on their connection's thread, with the control
//! channel's methods for one document (`docs/control-protocol.md`, Shared documents).

use serde_json::{Value, json};

use super::{Replica, Role};
use crate::scope::Scope;

/// Most times [`call`] sends a command's edit to the hub: the hub sends one back when it
/// clashes with another participant's change accepted meanwhile, and the copy makes it again
/// after that change.
const MAX_SENDS: usize = 4;

/// Why `copy` doesn't answer any more, if it doesn't: the room closed, or its participant is
/// out of it (the copy left, or the invite they joined with was taken back). Their connection
/// ends.
pub fn ended(copy: &Replica) -> Option<String> {
    let room = copy.room();
    if room.is_closed() {
        return Some("the document isn't shared any more".into());
    }
    (!room.is_in(copy.id())).then(|| format!("{} isn't in the shared document any more: their invite was taken back", copy.participant().name))
}

/// Answer control-channel method `method`, with `params`, for `copy`, the hosted copy of a
/// participant who joined with a key. Each first gets what others changed since:
///
/// - `engine.execute` (or `command`): run `command` with `params` in the copy
///   ([`Replica::run`]: held to the participant's role) and send what it changed to the hub,
///   until the hub accepted it. The command's result; an error when the hub sent an edit of
///   the participant's back and it couldn't be made again after another participant's change,
///   which took its text away (the error names the command; a hosted copy has no edit the hub
///   didn't take before a request, unless the hub kept sending it back).
/// - `document.inspect`: the document as the copy has it.
/// - `engine.commands`: every command, as MCP lists them, `enabled` when the participant may run
///   it now.
/// - `room.participants`: who is in the room (`participants`), who the caller is (`you`), and
///   the version of the document the copy has (`version`).
///
/// A `doc` other than the copy's own document is refused (it holds that one only), and so is
/// everything else: the window's methods (`ui.*`, `app.quit`), and sharing and inviting, which
/// are the owner's.
pub fn call(copy: &mut Replica, method: &str, params: &Value) -> Result<Value, String> {
    if let Some(why) = ended(copy) {
        return Err(why);
    }
    match method {
        "engine.execute" | "command" => {
            own_document(copy, params)?;
            let id = params.get("command").or_else(|| params.get("id")).and_then(Value::as_str).ok_or("missing `command`")?;
            let args = params.get("params").cloned().filter(|v| !v.is_null()).unwrap_or_else(|| json!({}));
            let lost = copy.stats().lost;
            copy.receive();
            let result = copy.run(id, &args).map_err(|e| e.to_string());
            for _ in 0..MAX_SENDS {
                if !copy.has_unconfirmed() {
                    break;
                }
                copy.send();
                copy.receive();
            }
            if result.is_ok() && copy.stats().lost > lost {
                return Err(copy.session().status.clone());
            }
            result
        }
        "document.inspect" => {
            own_document(copy, params)?;
            let mut args = params.clone();
            if let Some(o) = args.as_object_mut() {
                o.remove("doc");
            }
            copy.receive();
            copy.run("document.inspect", &args).map_err(|e| e.to_string())
        }
        "engine.commands" => {
            copy.receive();
            let search = params.get("search").and_then(Value::as_str).unwrap_or("").to_lowercase();
            let registry = copy.session().registry.clone();
            let commands = registry
                .all()
                .iter()
                .map(|c| {
                    let mut command = c.describe();
                    if let Value::Object(m) = &mut command {
                        m.insert("enabled".into(), json!(may_run(copy, c.id, c.mutates)));
                    }
                    command
                })
                .filter(|c| {
                    search.is_empty()
                        || ["id", "label", "location", "params"]
                            .iter()
                            .any(|k| c.get(*k).and_then(Value::as_str).is_some_and(|t| t.to_lowercase().contains(&search)))
                })
                .collect();
            Ok(Value::Array(commands))
        }
        "room.participants" => {
            copy.receive();
            Ok(json!({"you": copy.participant(), "participants": copy.room().participants(), "version": copy.received()}))
        }
        "room.share" | "room.invite" | "room.revoke" | "room.stop" => {
            Err(format!("`{method}` is the owner's: {} joined the shared document as a {}", copy.participant().name, copy.participant().role))
        }
        "app.quit" => Err(no_window(method)),
        m if m.starts_with("ui.") => Err(no_window(m)),
        _ => Err(format!(
            "unknown method `{method}`: a participant's copy answers engine.execute, document.inspect, engine.commands and room.participants"
        )),
    }
}

/// Refuse a `doc` other than the copy's document.
fn own_document(copy: &Replica, params: &Value) -> Result<(), String> {
    match params.get("doc") {
        None | Some(Value::Null) => Ok(()),
        Some(d) if d.as_u64() == Some(copy.document()) => Ok(()),
        Some(_) => Err("`doc`: a participant's copy holds the shared document only".into()),
    }
}

fn no_window(method: &str) -> String {
    format!("`{method}` works on the app's window: a participant's copy has none (run document commands with engine.execute)")
}

/// Whether the participant may run command `id` now: its scope is their role's, a viewer's
/// commands change nothing, and the command is enabled.
fn may_run(copy: &Replica, id: &str, mutates: bool) -> bool {
    let role = copy.participant().role;
    Scope::of(id).is_some_and(|s| role.allows(s)) && !(mutates && role == Role::Viewer) && copy.session().why_disabled(id).is_none()
}

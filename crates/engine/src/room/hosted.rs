//! Copies the room's process hosts for participants who joined with a key ([`super::Lobby`]):
//! each answers its participant's requests, on their connection's thread, with the control
//! channel's methods for one document (`docs/control-protocol.md`, Shared documents).

use std::sync::{Arc, OnceLock};

use serde_json::{Value, json};

use super::{Replica, Role};
use crate::file_access::AutomationRoots;
use crate::scope::Scope;

/// Most sends in a row of a command's edits that the hub takes none of before [`call`] answers
/// that it hasn't taken them yet: the hub sends edits back when they clash with another
/// participant's change accepted meanwhile, and the copy makes them again after that change.
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
/// participant who joined with a key. Each first sends the participant's edits the hub hasn't
/// taken yet, which an earlier command left when others' changes kept sending them back, and
/// gets what others changed since: when one of those edits couldn't be made again, the request
/// answers that, naming its command, and does nothing else.
///
/// - `engine.execute` (or `command`): run `command` with `params` in the copy
///   ([`Replica::run`]: held to the participant's role) and send what it changed to the hub,
///   until the hub accepted all of it (a long macro's changes go in several batches). The
///   command's result; an error when the hub sent an edit of the participant's back and it
///   couldn't be made again after another participant's change, which took its text away (the
///   error names the command), or when others' changes kept sending its edits back and the hub
///   hasn't taken them yet (they stay in the copy and go first with the participant's next
///   request; a command doesn't run while the hub still hasn't taken edits an earlier one left).
/// - `document.inspect`: the document as the copy has it.
/// - `engine.commands`: every command, as MCP lists them, `enabled` when the participant may run
///   it now.
/// - `ui.render`: a page as base64 PNG ([`crate::Session::page_png`]); a copy writes no files, so
///   `path` is refused.
/// - `ui.parity`: the feature catalog's parity, as the window gives it.
/// - `room.participants`: who is in the room (`participants`), who the caller is (`you`), and
///   the version of the document the copy has (`version`).
/// - Any other method: a command id, run as `engine.execute` runs it, with `params` as its
///   params (as the window takes it).
///
/// A `doc` other than the copy's own document is refused (it holds that one only), and so are
/// the window's methods (`ui.*`, `app.quit`), and sharing and inviting, which are the owner's.
/// Their commands reach none of the computer's files, whatever their role: one that reads or
/// writes a file it names (mail merge's recipients, a picture) is refused
/// ([`crate::file_access::AutomationRoots::none`]).
pub fn call(copy: &mut Replica, method: &str, params: &Value) -> Result<Value, String> {
    static NO_FILES: OnceLock<Arc<AutomationRoots>> = OnceLock::new();
    let none = NO_FILES.get_or_init(AutomationRoots::none);
    crate::file_access::confine(Some(none), || answer(copy, method, params))
}

/// [`call`], with no file in reach.
fn answer(copy: &mut Replica, method: &str, params: &Value) -> Result<Value, String> {
    if let Some(why) = ended(copy) {
        return Err(why);
    }
    match method {
        "engine.execute" | "command" => {
            own_document(copy, params)?;
            let id = params.get("command").or_else(|| params.get("id")).and_then(Value::as_str).ok_or("missing `command`")?;
            let args = params.get("params").cloned().filter(|v| !v.is_null()).unwrap_or_else(|| json!({}));
            execute(copy, id, &args)
        }
        "document.inspect" => {
            own_document(copy, params)?;
            let mut args = params.clone();
            if let Some(o) = args.as_object_mut() {
                o.remove("doc");
            }
            catch_up(copy, method, false)?;
            copy.run("document.inspect", &args).map_err(|e| e.to_string())
        }
        "engine.commands" => {
            catch_up(copy, method, false)?;
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
        "ui.render" => {
            own_document(copy, params)?;
            if params.get("path").is_some_and(|p| !p.is_null()) {
                return Err("`path`: a participant's copy writes no files; without one, the page comes back as base64 `png`".into());
            }
            catch_up(copy, method, false)?;
            let page = params.get("page").and_then(Value::as_u64).unwrap_or(1);
            let scale = params.get("scale").and_then(Value::as_f64).map_or(2.0, |v| v as f32);
            copy.session().page_png(page, scale)
        }
        "ui.parity" => Ok(crate::catalog::parity(&copy.session().registry)),
        "room.participants" => {
            catch_up(copy, method, false)?;
            Ok(json!({"you": copy.participant(), "participants": copy.room().participants(), "version": copy.received()}))
        }
        "room.share" | "room.invite" | "room.revoke" | "room.stop" => {
            Err(format!("`{method}` is the owner's: {} joined the shared document as a {}", copy.participant().name, copy.participant().role))
        }
        "app.quit" => Err(no_window(method)),
        m if m.starts_with("ui.") => Err(no_window(m)),
        // Anything else: a command id.
        id => execute(copy, id, params),
    }
}

/// Run command `id` with `params` in the copy ([`Replica::run`]: held to the participant's
/// role), once the hub took the edits an earlier command left ([`catch_up`]), and send what it
/// changed to the hub until the hub took all of it ([`send_all`]). The command's result; an error
/// when the hub sent the edit back and another participant's change took its text away, or when
/// the hub hasn't taken all of it.
fn execute(copy: &mut Replica, id: &str, params: &Value) -> Result<Value, String> {
    catch_up(copy, id, true)?;
    let lost = copy.stats().lost;
    let result = copy.run(id, params).map_err(|e| e.to_string());
    send_all(copy)?;
    // Edits it couldn't make again, or the hub hasn't taken: this command's, as an earlier
    // command left none.
    let left = copy.unconfirmed();
    let not_taken = (left > 0).then(|| {
        format!(
            "{id} ran, but the shared document hasn't taken {left} of your changes yet: other participants' changes sent them back {MAX_SENDS} times in a row. {STAY}"
        )
    });
    let notes: Vec<String> = [lost_since(copy, lost), not_taken].into_iter().flatten().collect();
    match result {
        Ok(_) if !notes.is_empty() => Err(notes.join(". ")),
        Err(e) if !notes.is_empty() => Err(format!("{e}. {}", notes.join(". "))),
        r => r,
    }
}

/// What the errors about changes the hub hasn't taken yet say happens to them.
const STAY: &str = "They stay in your copy and go first with your next request (disconnecting drops them)";

/// Before request `what` (a method, or a command when `command`): send the participant's edits
/// the hub hasn't taken yet, which an earlier command left when others' changes kept sending them
/// back, and get what others changed since ([`send_all`]). An error, and `what` isn't done, when
/// one of those edits couldn't be made again (another participant's change took its text away),
/// or when the participant is out of the room; before a command, also when the hub still hasn't
/// taken them, so what a command answers is about its own edits.
fn catch_up(copy: &mut Replica, what: &str, command: bool) -> Result<(), String> {
    let lost = copy.stats().lost;
    send_all(copy)?;
    if let Some(why) = lost_since(copy, lost) {
        return Err(format!("{why}. That was an earlier edit of yours, so {what} didn't run: send it again"));
    }
    let left = copy.unconfirmed();
    if command && left > 0 {
        return Err(format!(
            "the shared document still hasn't taken {left} of your changes from before: other participants' changes sent them back {MAX_SENDS} times in a row, so {what} didn't run. {STAY}"
        ));
    }
    Ok(())
}

/// Get what others changed since, and send the copy's edits the hub hasn't taken until it took
/// all of them: up to [`super::MAX_BATCH`] changes at a time, so a long macro's go in several
/// sends, and made again after the changes the hub sends them back for. It stops after
/// [`MAX_SENDS`] sends in a row that the hub took none of, and with an error when the
/// participant is out of the room ([`ended`]).
fn send_all(copy: &mut Replica) -> Result<(), String> {
    copy.receive();
    // A send counts as taken when it leaves fewer changes than ever before, so this ends.
    let mut fewest = copy.unconfirmed();
    let mut idle = 0;
    while copy.has_unconfirmed() && idle < MAX_SENDS {
        copy.send();
        copy.receive();
        if let Some(why) = ended(copy) {
            return Err(why);
        }
        let left = copy.unconfirmed();
        if left < fewest {
            fewest = left;
            idle = 0;
        } else {
            idle += 1;
        }
    }
    Ok(())
}

/// Why the copy gave up edits since it had given up `lost` (the last one's reason, naming its
/// command), and how many when more than one; `None` when it gave up none.
fn lost_since(copy: &Replica, lost: u64) -> Option<String> {
    let n = copy.stats().lost.saturating_sub(lost);
    match n {
        0 => None,
        1 => Some(copy.last_lost().to_string()),
        n => Some(format!("{} ({n} edits in all)", copy.last_lost())),
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

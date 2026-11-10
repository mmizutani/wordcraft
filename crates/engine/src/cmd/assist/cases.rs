//! The person's cases (#107): `assist.memory.case.*` binds the shown document to a case of a
//! client, sets its facts and its power balance, keeps its plan and notes, and deletes it;
//! `assist.memory.wall.*` makes and deletes a client. Each checks the shape of what it is given
//! and hands the window a `memory.case.<verb>` or `memory.wall.<verb>` request, so every guard
//! that keys on the `assist.memory.` and `memory.` prefixes applies ([`super::person_only`], the
//! macro guard, the control channel's): a case outlasts the document, and a document could
//! otherwise plant one. The typed record lives above the engine (`wordcraft-assist`), so the
//! window parses the objects passed through here; this holds their size and the ids' form.

use serde_json::{Value, json};

use super::memory_here;
use crate::{CmdError, CmdResult, CommandSpec, Session};

/// Most characters of a title, a client's name, a note's main text, a plan's goal or a step.
pub const MAX_CASE_TEXT: usize = 400;

/// Most bytes of an object passed through (`facts`, `note`, a balance's `factors`).
const MAX_OBJECT_BYTES: usize = 16 << 10;

/// Most steps a plan lists.
const MAX_STEPS: usize = 16;

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("assist.memory.wall.new", "New Client", "Home › Assistant", wall_new)
            .params(r#"{"name": string (at most 400 characters)} → a client (a folder of cases) the next case can go in. Yours alone, as `assist.memory.add` is"#)
            .when(memory_here)
            .pure(),
        CommandSpec::new("assist.memory.wall.delete", "Delete a Client", "Home › Assistant", wall_delete)
            .params(r#"{"wall": string (a client's id, 16 hex digits)} → the client and every case in it go, and can't be brought back. Yours alone, as `assist.memory.add` is"#)
            .when(memory_here)
            .pure(),
        CommandSpec::new("assist.memory.case.new", "New Case", "Home › Assistant", case_new)
            .params(r#"{"title": string (at most 400 characters), "wall"?: string (a client's id), "client"?: string (a new client's name; neither: "My cases")} → a case for the open document, bound to it. Yours alone, as `assist.memory.add` is"#)
            .when(memory_here)
            .pure(),
        CommandSpec::new("assist.memory.case.bind", "Use a Case", "Home › Assistant", case_bind)
            .params(r#"{"wall": string, "case": string (ids of 16 hex digits)} → the open document's turns recall from this case; a conversation bound to another ends. Yours alone, as `assist.memory.add` is"#)
            .when(memory_here)
            .pure(),
        CommandSpec::new("assist.memory.case.unbind", "Stop Using the Case", "Home › Assistant", |s, _| {
            s.push_ui_request(json!({"assist": "memory.case.unbind"}));
            Ok(json!({"unbound": true}))
        })
        .params(r#"{} → the open document recalls nothing, and its conversation ends. Yours alone, as `assist.memory.add` is"#)
        .when(memory_here)
        .pure(),
        CommandSpec::new("assist.memory.case.facts", "Case Facts", "Home › Assistant", case_facts)
            .params(r#"{"facts": {"kind"?, "side"?, "paper"?, "theirs"?, "law"?, "size"?, "language"?, "stage"?, "parties"?: [string]}} → the bound case's facts, replaced whole; every field optional. Yours alone, as `assist.memory.add` is"#)
            .when(memory_here)
            .pure(),
        CommandSpec::new("assist.memory.case.balance", "Reassess the Balance", "Home › Assistant", case_balance)
            .params(r#"{"stance": "stronger|even|weaker", "factors"?: [{"kind", "favors": "us|them|neither", "why"?: string}] (at most 4)} → a dated assessment added to the bound case. Yours alone, as `assist.memory.add` is"#)
            .when(memory_here)
            .pure(),
        CommandSpec::new("assist.memory.case.plan", "Case Plan", "Home › Assistant", case_plan)
            .params(r#"{"goal": string, "next"?: [string] (at most 16, each at most 400 characters)} or {"clear": true} → the bound case's plan. Yours alone, as `assist.memory.add` is"#)
            .when(memory_here)
            .pure(),
        CommandSpec::new("assist.memory.case.note", "Add a Case Note", "Home › Assistant", |s, v| note(s, v, "memory.case.note", false))
            .params(r#"{"note": {"kind": "pattern|pitfall|lesson|leverage|position", …its fields}, "how"?: string} → a confirmed note in the bound case. Yours alone, as `assist.memory.add` is"#)
            .when(memory_here)
            .pure(),
        CommandSpec::new("assist.memory.case.noteEdit", "Edit a Case Note", "Home › Assistant", |s, v| note(s, v, "memory.case.noteEdit", true))
            .params(r#"{"id": string (n12), "note": {…}, "how"?: string} → the note replaced, confirmed. Yours alone, as `assist.memory.add` is"#)
            .when(memory_here)
            .pure(),
        CommandSpec::new("assist.memory.case.noteConfirm", "Confirm a Case Note", "Home › Assistant", |s, v| note_id_only(s, v, "memory.case.noteConfirm"))
            .params(r#"{"id": string (n12)} → the note stands as yours, and this client's other cases can recall it. Yours alone, as `assist.memory.add` is"#)
            .when(memory_here)
            .pure(),
        CommandSpec::new("assist.memory.case.noteRemove", "Remove a Case Note", "Home › Assistant", |s, v| note_id_only(s, v, "memory.case.noteRemove"))
            .params(r#"{"id": string (n12)} → the note goes; a hash of its text stays so a model doesn't note it again. Yours alone, as `assist.memory.add` is"#)
            .when(memory_here)
            .pure(),
        CommandSpec::new("assist.memory.case.cardDismiss", "Not This Case Card", "Home › Assistant", card_dismiss)
            .params(r#"{"card": number} → the conversation's card (facts or a plan the assistant proposed) goes, and nothing is stored. Yours alone, as `assist.memory.add` is"#)
            .when(memory_here)
            .pure(),
        CommandSpec::new("assist.memory.case.delete", "Delete the Case", "Home › Assistant", |s, _| {
            s.push_ui_request(json!({"assist": "memory.case.delete"}));
            Ok(json!({"deleted": true}))
        })
        .params(r#"{} → the bound case's folder and every note in it go, and can't be brought back; what a model provider or an agent already holds isn't reached. Yours alone, as `assist.memory.add` is"#)
        .when(memory_here)
        .pure(),
    ]
}

/// The text in `v[key]`: trimmed, not empty, at most [`MAX_CASE_TEXT`] characters.
fn text(v: &Value, key: &str) -> Result<String, CmdError> {
    match v.get(key) {
        Some(Value::String(t)) if !t.trim().is_empty() && t.chars().count() <= MAX_CASE_TEXT => Ok(t.trim().to_string()),
        _ => Err(CmdError::Params(format!("`{key}`: text of at most {MAX_CASE_TEXT} characters"))),
    }
}

/// A wall's or a case's id in `v[key]`: 16 hex digits, so it can't name a path.
fn hex_id(v: &Value, key: &str) -> Result<String, CmdError> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|id| id.len() == 16 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        .map(str::to_string)
        .ok_or_else(|| CmdError::Params(format!("`{key}`: an id of 16 hex digits, as the Assistant pane's Details › Case lists them")))
}

/// A note's id in `v["id"]`: `n` and up to 16 digits.
fn note_id(v: &Value) -> Result<String, CmdError> {
    v.get("id")
        .and_then(Value::as_str)
        .filter(|id| id.strip_prefix('n').is_some_and(|n| !n.is_empty() && n.len() <= 16 && n.bytes().all(|b| b.is_ascii_digit())))
        .map(str::to_string)
        .ok_or_else(|| CmdError::Params("`id`: a note's, such as n12".into()))
}

/// An object in `v[key]` of at most [`MAX_OBJECT_BYTES`], passed through for the window to parse.
fn object(v: &Value, key: &str) -> Result<Value, CmdError> {
    match v.get(key) {
        Some(o @ Value::Object(_)) if o.to_string().len() <= MAX_OBJECT_BYTES => Ok(o.clone()),
        _ => Err(CmdError::Params(format!("`{key}`: an object of at most {} KiB", MAX_OBJECT_BYTES >> 10))),
    }
}

/// An optional list of texts in `v[key]`, at most `max` of them.
fn texts(v: &Value, key: &str, max: usize) -> Result<Vec<String>, CmdError> {
    let err = || CmdError::Params(format!("`{key}`: at most {max} texts of at most {MAX_CASE_TEXT} characters"));
    match v.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(list)) if list.len() <= max => list
            .iter()
            .map(|t| match t {
                Value::String(t) if t.chars().count() <= MAX_CASE_TEXT => Ok(t.trim().to_string()),
                _ => Err(err()),
            })
            .filter(|t| !matches!(t, Ok(t) if t.is_empty()))
            .collect(),
        Some(_) => Err(err()),
    }
}

fn wall_new(s: &mut Session, v: &Value) -> CmdResult {
    let name = text(v, "name")?;
    s.push_ui_request(json!({"assist": "memory.wall.new", "name": name}));
    Ok(json!({"created": true}))
}

fn wall_delete(s: &mut Session, v: &Value) -> CmdResult {
    let wall = hex_id(v, "wall")?;
    s.push_ui_request(json!({"assist": "memory.wall.delete", "wall": wall}));
    Ok(json!({"deleted": true}))
}

fn case_new(s: &mut Session, v: &Value) -> CmdResult {
    let title = text(v, "title")?;
    let wall = if v.get("wall").is_some_and(|w| !w.is_null()) { Some(hex_id(v, "wall")?) } else { None };
    let client = if v.get("client").is_some_and(|c| !c.is_null()) { Some(text(v, "client")?) } else { None };
    if wall.is_some() && client.is_some() {
        return Err(CmdError::Params("`wall` or `client`, not both".into()));
    }
    s.push_ui_request(json!({"assist": "memory.case.new", "title": title, "wall": wall, "client": client}));
    Ok(json!({"created": true}))
}

fn case_bind(s: &mut Session, v: &Value) -> CmdResult {
    let wall = hex_id(v, "wall")?;
    let case = hex_id(v, "case")?;
    s.push_ui_request(json!({"assist": "memory.case.bind", "wall": wall, "case": case}));
    Ok(json!({"bound": true}))
}

fn case_facts(s: &mut Session, v: &Value) -> CmdResult {
    let facts = object(v, "facts")?;
    s.push_ui_request(json!({"assist": "memory.case.facts", "facts": facts}));
    Ok(json!({"set": true}))
}

fn case_balance(s: &mut Session, v: &Value) -> CmdResult {
    let stance = match v.get("stance").and_then(Value::as_str) {
        Some(stance @ ("stronger" | "even" | "weaker")) => stance,
        _ => return Err(CmdError::Params("`stance`: stronger, even or weaker".into())),
    };
    let factors = match v.get("factors") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(list))
            if list.len() <= 4 && list.iter().all(Value::is_object) && Value::Array(list.clone()).to_string().len() <= MAX_OBJECT_BYTES =>
        {
            list.clone()
        }
        Some(_) => return Err(CmdError::Params("`factors`: at most 4 objects, each {\"kind\", \"favors\", \"why\"?}".into())),
    };
    s.push_ui_request(json!({"assist": "memory.case.balance", "stance": stance, "factors": factors}));
    Ok(json!({"assessed": true}))
}

fn case_plan(s: &mut Session, v: &Value) -> CmdResult {
    if v.get("clear") == Some(&Value::Bool(true)) {
        s.push_ui_request(json!({"assist": "memory.case.plan", "clear": true}));
        return Ok(json!({"cleared": true}));
    }
    let goal = text(v, "goal")?;
    let next = texts(v, "next", MAX_STEPS)?;
    s.push_ui_request(json!({"assist": "memory.case.plan", "goal": goal, "next": next}));
    Ok(json!({"set": true}))
}

/// `assist.memory.case.note` and `.noteEdit`: the note as an object, and how it was known.
fn note(s: &mut Session, v: &Value, what: &str, with_id: bool) -> CmdResult {
    let id = if with_id { Some(note_id(v)?) } else { None };
    let note = object(v, "note")?;
    let how = match v.get("how") {
        None | Some(Value::Null) => None,
        Some(Value::String(h)) if h.trim().is_empty() => None,
        Some(Value::String(h)) if h.chars().count() <= MAX_CASE_TEXT => Some(h.trim().to_string()),
        Some(_) => return Err(CmdError::Params(format!("`how`: text of at most {MAX_CASE_TEXT} characters"))),
    };
    s.push_ui_request(json!({"assist": what, "id": id, "note": note, "how": how}));
    Ok(json!({"noted": true}))
}

/// The card in `v["card"]`: a number from 1, as the conversation gives it.
fn card_dismiss(s: &mut Session, v: &Value) -> CmdResult {
    let card = v
        .get("card")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .ok_or_else(|| CmdError::Params("`card`: the conversation's, a whole number from 1".into()))?;
    s.push_ui_request(json!({"assist": "memory.case.cardDismiss", "card": card}));
    Ok(json!({"dismissed": true}))
}

fn note_id_only(s: &mut Session, v: &Value, what: &str) -> CmdResult {
    let id = note_id(v)?;
    s.push_ui_request(json!({"assist": what, "id": id}));
    Ok(json!({"id": id}))
}

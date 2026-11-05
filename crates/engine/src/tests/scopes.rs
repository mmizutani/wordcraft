//! Command scopes and positions (`crate::scope`): every command has a scope that agrees with
//! what it does, and declares the params that are places in the document.

use std::collections::HashSet;

use serde_json::{Value, json};

use crate::Session;
use crate::sample::sample_document;
use crate::scope::{self, OVERRIDES, POSITIONS, Place, Scope};

#[test]
fn every_command_has_a_scope() {
    let reg = crate::cmd::registry();
    let missing: Vec<&str> = reg.all().iter().filter(|c| Scope::of(c.id).is_none()).map(|c| c.id).collect();
    assert!(
        missing.is_empty(),
        "these commands have no scope: give their prefix one in Scope::by_prefix, or each command one in scope::OVERRIDES: {missing:?}"
    );
}

#[test]
fn every_override_names_a_command_and_changes_its_scope() {
    let reg = crate::cmd::registry();
    let mut seen = HashSet::new();
    for (id, scope) in OVERRIDES {
        assert!(reg.get(id).is_some(), "scope::OVERRIDES names {id}, which is no command");
        assert_ne!(Scope::by_prefix(id), Some(*scope), "{id}'s override is its prefix's scope already");
        assert!(seen.insert(id), "{id} is in scope::OVERRIDES twice");
    }
}

/// A command scoped to a participant's selection or view changes nothing everyone sees: run from
/// a few places in the sample document, with no params and with params commands commonly take,
/// none of them makes a change (`document.changes` would carry it to every copy).
#[test]
fn selection_and_view_commands_never_change_the_document() {
    let reg = crate::cmd::registry();
    let params = [
        json!({}),
        json!({"text": "Ab", "value": true, "style": "Heading 1", "name": "x", "index": 0, "level": 2, "page": 0, "x": 100, "y": 100, "points": [[90, 90], [200, 90], [200, 200]]}),
    ];
    let places = [("caret.docStart", json!({})), ("select.text", json!({"text": "Studio"})), ("caret.docEnd", json!({})), ("select.all", json!({}))];
    let mut s = Session::new(sample_document());
    let mut changed = Vec::new();
    let mut ran = 0;
    for spec in reg.all().iter().filter(|c| matches!(c.scope(), Scope::Selection | Scope::View | Scope::Ui)) {
        for v in &params {
            for (place, at) in &places {
                let _ = s.run(place, at);
                let version = s.changes().version();
                let _ = s.run(spec.id, v);
                ran += 1;
                if s.changes().version() != version {
                    changed.push(format!("{} {v} after {place}", spec.id));
                    s = Session::new(sample_document());
                }
            }
        }
    }
    assert!(ran > 1000, "only {ran} runs");
    assert!(changed.is_empty(), "commands scoped to a participant's selection or view changed the document:\n{}", changed.join("\n"));
}

/// The params a params doc gives as places: `"key": Pos` (or `pos`), `"key"?: [Pos | n]` (a
/// list) and `"path": [n]` (a block). A key is a quoted name followed by `:` or `?:`, so quoted
/// values and stray quotes (`6"`) are skipped.
fn documented_places(doc: &str) -> Vec<(String, Place)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(quote) = doc.get(from..).and_then(|r| r.find('"')) {
        from += quote + 1;
        let rest = doc.get(from..).unwrap_or("");
        let len = rest.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(rest.len());
        let (key, after) = (rest.get(..len).unwrap_or(""), rest.get(len..).unwrap_or(""));
        let Some(after) = after.strip_prefix('"').filter(|_| !key.is_empty()) else { continue };
        let Some(value) = after.strip_prefix('?').unwrap_or(after).strip_prefix(':') else { continue };
        let value = value.trim_start();
        let (list, value) = match value.strip_prefix('[') {
            Some(v) => (true, v.trim_start()),
            None => (false, value),
        };
        let pos = value.strip_prefix("Pos").or_else(|| value.strip_prefix("pos")).is_some_and(|r| !r.starts_with(|c: char| c.is_alphanumeric()));
        if pos {
            out.push((key.to_string(), if list { Place::List } else { Place::Pos }));
        } else if key == "path" && list && value.starts_with("n]") {
            out.push((key.to_string(), Place::Path));
        }
    }
    out
}

#[test]
fn the_params_docs_are_read_for_places() {
    assert_eq!(documented_places(r#"{"anchor": Pos, "focus": Pos}"#), [("anchor".into(), Place::Pos), ("focus".into(), Place::Pos)]);
    assert_eq!(documented_places(r#"{"control"?: tag or title, "at"?: pos}"#), [("at".into(), Place::Pos)]);
    assert_eq!(
        documented_places(r#"{"pos"?: Pos, "index"?: n, "objects"?: [Pos | n]}"#),
        [("pos".into(), Place::Pos), ("objects".into(), Place::List)]
    );
    assert_eq!(documented_places(r#"{"path": [n], "story"?: Story}"#), [("path".into(), Place::Path)]);
    // A legend's position, a tab stop and an equation's own place aren't places in the text.
    assert!(documented_places(r#"{"pos"?: "right|left|top|bottom|none"}"#).is_empty());
    assert!(documented_places(r#"{"tabs"?: [TabStop]} where TabStop = {"pos": pt, "align"?: "left"}"#).is_empty());
    assert!(documented_places(r#"{"pos"?: {"path": [[node, child]…], "off": n}}"#).is_empty());
    assert_eq!(documented_places(r#"{"unit": "word|sentence", "start": Pos, "position"?: "top|bottom"}"#), [("start".into(), Place::Pos)]);
    assert_eq!(documented_places(r#"{"width"?: pt (default 432 = 6"), "to"?: Pos}"#), [("to".into(), Place::Pos)]);
}

/// Every param a command's params doc gives as a place is in `scope::POSITIONS`, so whatever
/// carries a command across others' edits (the hub) moves it; and every one listed there is
/// one of the command's params.
#[test]
fn every_command_declares_the_params_that_are_places() {
    let reg = crate::cmd::registry();
    let mut seen = HashSet::new();
    for (id, places) in POSITIONS {
        assert!(reg.get(id).is_some(), "scope::POSITIONS names {id}, which is no command");
        assert!(seen.insert(id), "{id} is in scope::POSITIONS twice");
        assert!(!places.is_empty(), "{id} is in scope::POSITIONS with no params");
    }
    let mut undeclared = Vec::new();
    for c in reg.all() {
        let declared = c.positions();
        for (key, place) in documented_places(c.params) {
            if !declared.contains(&(key.as_str(), place)) {
                undeclared.push(format!("{}: `{key}` ({place:?})", c.id));
            }
        }
        for (key, _) in declared {
            assert!(c.params.contains(&format!("\"{key}\"")), "{}: scope::POSITIONS lists `{key}`, which its params doc doesn't name", c.id);
        }
    }
    assert!(undeclared.is_empty(), "params docs give these as places, but scope::POSITIONS doesn't list them:\n{}", undeclared.join("\n"));
}

#[test]
fn the_command_list_gives_each_command_s_scope_and_places() {
    let list = crate::cmd::registry().describe();
    let get = |id: &str| list.as_array().and_then(|l| l.iter().find(|c| c["id"] == id)).cloned().unwrap_or(Value::Null);
    assert_eq!(get("format.bold")["scope"], "document");
    assert_eq!(get("document.changes")["scope"], "document");
    assert_eq!(get("review.comments")["scope"], "document");
    assert_eq!(get("caret.set")["scope"], "selection");
    assert_eq!(get("edit.find")["scope"], "selection");
    assert_eq!(get("view.zoom")["scope"], "view");
    assert_eq!(get("file.save")["scope"], "file");
    assert_eq!(get("view.switchWindows")["scope"], "file");
    assert_eq!(get("file.options")["scope"], "app");
    assert_eq!(get("file.properties")["scope"], "document");
    assert_eq!(get("caret.set")["positions"], json!({"pos": "pos"}));
    assert_eq!(get("select.addObject")["positions"], json!({"pos": "pos", "objects": "list"}));
    assert_eq!(get("document.paragraph")["positions"], json!({"path": "path"}));
    assert!(get("format.bold").get("positions").is_none());
    assert_eq!(get("format.bold")["mutates"], true);
    assert_eq!(scope::positions("no.such"), &[]);
    assert_eq!(Scope::of("ui.backstage"), Some(Scope::Ui));
    assert_eq!(Scope::of("nonsense"), None);
}

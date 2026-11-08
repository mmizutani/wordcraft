//! The models a ChatGPT account may use, and which one runs.
//!
//! [`models_with`] reads the account's list. [`resolve`] decides what a turn runs from that list
//! and the person's choice, as a table:
//!
//! | | Model | Effort |
//! |---|---|---|
//! | 1 | The choice, when the list has it | The choice, when this model offers it |
//! | 2 | [`DEFAULT_MODEL`], when the list has it | [`DEFAULT_EFFORT`], when this model offers it |
//! | 3 | The list's first | This model's default |
//! | 4 | | Nothing sent |
//!
//! A model whose list entry names no levels takes any level: the list may be the API's usual
//! `data` of ids, which says nothing about reasoning, and a level it refuses is dropped at the
//! request ([`super::backend`]).

use std::path::Path;

use serde_json::Value;

use super::auth::{AuthError, access_token};
use super::http::{self, Reply};
use super::{DEFAULT_EFFORT, DEFAULT_MODEL, Endpoints};
use crate::backend::{ModelOption, label};

/// Most models read from the list.
const MAX_MODELS: usize = 64;

/// Most reasoning levels read for one model.
const MAX_LEVELS: usize = 16;

/// Most characters of a model's name.
const MAX_NAME: usize = 80;

/// Most characters of a model's description.
const MAX_DESCRIPTION: usize = 300;

/// The models the signed-in account may use, from OpenAI's API: [`models_with`] with OpenAI's
/// endpoints. It waits for the network: call it off the window's thread.
pub fn models(settings: &Path) -> Result<Vec<ModelOption>, AuthError> {
    models_with(settings, &Endpoints::default())
}

/// The models the signed-in account may use, in ChatGPT's order. It waits for the network (and a
/// refresh, when the token is old or turned away).
pub fn models_with(settings: &Path, endpoints: &Endpoints) -> Result<Vec<ModelOption>, AuthError> {
    endpoints.check().map_err(AuthError::Failed)?;
    let agent = http::agent(endpoints.is_local(), false);
    let url = format!("{}/models", endpoints.api.trim_end_matches('/'));
    let ask = |refused: Option<&str>| -> Result<(String, Reply), AuthError> {
        let c = access_token(settings, endpoints, refused)?;
        if !c.plan_usage() {
            return Err(AuthError::Denied);
        }
        let reply = http::get_json(&agent, &url, Some(&c.access_token)).map_err(AuthError::Network)?;
        Ok((c.access_token, reply))
    };
    let (token, mut reply) = ask(None)?;
    if reply.status == 401 {
        // Turned away though it looks fresh here: refreshed once (the sign-in ends only when
        // the refresh says so), as a turn's token is.
        log::info!("ChatGPT: the models list turned the token away; refreshing it once");
        reply = ask(Some(&token))?.1;
        if reply.status == 401 {
            return Err(AuthError::Failed(
                "ChatGPT didn't accept your sign-in: try again, or sign out and sign in again in Options › Account".into(),
            ));
        }
    }
    if !reply.ok() {
        return Err(AuthError::Failed(format!("ChatGPT's models couldn't be listed (HTTP {})", reply.status)));
    }
    Ok(models_in(&reply.json()))
}

/// The models a models list names: `models` (`slug`, `display_name`, shown when `visibility` is
/// `list` or absent), or the API's usual `data` (`id`). Each entry's levels come from
/// `supported_reasoning_levels`, as strings or as objects with an `effort`.
pub(crate) fn models_in(json: &Value) -> Vec<ModelOption> {
    let list = json.get("models").or_else(|| json.get("data")).and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    list.iter()
        .filter(|m| m.get("visibility").and_then(Value::as_str).is_none_or(|v| v == "list"))
        .filter_map(|m| {
            let id = m.get("slug").or_else(|| m.get("id")).and_then(Value::as_str).filter(|s| valid_model(s))?;
            let name: String = m.get("display_name").and_then(Value::as_str).map(|n| label(n).chars().take(MAX_NAME).collect()).unwrap_or_default();
            let name = if name.trim().is_empty() { id.to_string() } else { name };
            let description = m.get("description").and_then(Value::as_str).map(|d| label(d).chars().take(MAX_DESCRIPTION).collect());
            let efforts = levels_in(m);
            let offered = |level: &String| efforts.is_empty() || efforts.contains(level);
            let default_effort = m.get("default_reasoning_level").and_then(level_in).filter(offered);
            Some(ModelOption { id: id.to_string(), name, description, efforts, default_effort })
        })
        .take(MAX_MODELS)
        .collect()
}

/// A model's reasoning levels, as listed: junk left out, each once, at most [`MAX_LEVELS`].
fn levels_in(model: &Value) -> Vec<String> {
    let listed = model.get("supported_reasoning_levels").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let mut levels: Vec<String> = Vec::new();
    for level in listed.iter().filter_map(level_in) {
        if levels.len() < MAX_LEVELS && !levels.contains(&level) {
            levels.push(level);
        }
    }
    levels
}

/// One reasoning level: a string, or an object with an `effort`.
fn level_in(level: &Value) -> Option<String> {
    let level = level.as_str().or_else(|| level.get("effort").and_then(Value::as_str))?;
    (!level.is_empty() && level.len() <= 32 && level.chars().all(|c| c.is_ascii_graphic())).then(|| level.to_string())
}

/// Whether `slug` can name a model: 1 to 128 printable ASCII characters, no spaces.
pub(crate) fn valid_model(slug: &str) -> bool {
    !slug.is_empty() && slug.len() <= 128 && slug.chars().all(|c| c.is_ascii_graphic())
}

/// What a turn runs ([`resolve`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// What the request names the model.
    pub id: String,
    /// Its name in the pane.
    pub name: String,
    /// The reasoning level the request asks for; `None` sends none.
    pub effort: Option<String>,
    /// The chosen model isn't in the list: another one runs.
    pub model_gone: bool,
    /// This model doesn't offer the chosen level: another one runs.
    pub effort_gone: bool,
}

/// What runs with the choice `model` and `effort`, by the table in this module's documentation.
/// `None` for an empty list.
pub fn resolve(list: &[ModelOption], model: Option<&str>, effort: Option<&str>) -> Option<Resolved> {
    let chosen = model.and_then(|id| list.iter().find(|m| m.id == id));
    let runs = chosen.or_else(|| list.iter().find(|m| m.id == DEFAULT_MODEL)).or_else(|| list.first())?;
    let offered = |level: &&str| runs.efforts.is_empty() || runs.efforts.iter().any(|e| e == level);
    let level =
        effort.filter(offered).or(Some(DEFAULT_EFFORT).filter(offered)).or(runs.default_effort.as_deref().filter(offered)).map(str::to_string);
    Some(Resolved {
        id: runs.id.clone(),
        name: runs.name.clone(),
        effort: level,
        model_gone: model.is_some() && chosen.is_none(),
        effort_gone: effort.is_some_and(|e| !offered(&e)),
    })
}

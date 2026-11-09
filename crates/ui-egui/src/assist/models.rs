//! What the Assistant pane's Model and Effort menus offer (#84): worked out from what the backend
//! in use offers and what the person chose ([`menus`]), so the pane, `assist.model` and
//! `assist.effort` say the same.
//!
//! ChatGPT's offer is the account's list, which waits for the network: [`tick`] reads it on a
//! thread of its own when the pane is open with someone signed in and none is held, and again
//! after a sign-in, and no frame waits for it. An agent's offer is what its conversation last said
//! ([`Assistant::offered`]), known once it started.

use serde::Serialize;
use wordcraft_assist::{Assistant, ModelOption, Offered};
use wordcraft_engine::ModelChoice;

use super::{backend_in_use, desktop};
use crate::WordApp;

/// What an effort menu offers when the model's list entry names no levels.
#[cfg(not(target_arch = "wasm32"))]
const LEVELS_UNKNOWN: [&str; 3] = ["low", "medium", "high"];

/// A row of a menu. `name`: what the backend calls it; `None`: its id, which the pane words itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Row {
    pub(crate) id: String,
    pub(crate) name: Option<String>,
    pub(crate) description: Option<String>,
    /// What runs when nothing is chosen: choosing it clears the choice.
    pub(crate) is_default: bool,
}

/// What the two menus offer now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Menus {
    /// The models offered, besides the Default row.
    pub(crate) models: Vec<Row>,
    /// What the Default row resolves to, when that is known.
    pub(crate) default_model: Option<String>,
    /// The chosen model, while it is offered: the row ticked. `None` ticks the Default row.
    pub(crate) chosen_model: Option<String>,
    /// What the model menu's button says: what the next turn runs, when that is known.
    pub(crate) running_model: Option<String>,
    /// The levels offered for that model.
    pub(crate) efforts: Vec<Row>,
    /// The level the next turn runs at, when that is known: the row ticked.
    pub(crate) effort: Option<String>,
    /// The models are what the backend said: a choice outside them is refused. While they aren't
    /// known, any choice is kept, and the turn says if it doesn't resolve.
    pub(crate) known: bool,
    /// The same for the levels. A model whose list entry names none takes any.
    pub(crate) efforts_known: bool,
    /// Why the model menu has only the Default row, or that reading the list failed.
    pub(crate) unknown_because: Option<String>,
    /// That the list couldn't be read, for beside the menu.
    pub(crate) failed: Option<String>,
}

impl Menus {
    /// Whether there is a model menu: an agent may offer only a level.
    pub(crate) fn has_models(&self) -> bool {
        !self.models.is_empty() || !self.known
    }

    /// `id` is offered as a model, or nothing is known yet to refuse it by.
    pub(crate) fn offers_model(&self, id: &str) -> bool {
        !self.known || self.models.iter().any(|m| m.id == id)
    }

    /// `level` is offered for the model, or nothing is known yet to refuse it by.
    pub(crate) fn offers_effort(&self, level: &str) -> bool {
        !self.efforts_known || self.efforts.iter().any(|e| e.id == level)
    }
}

/// What the menus offer for the backend in use, with what the person chose for it. `None`: no
/// menus. The web app runs no backend, nobody is signed in to ChatGPT, or the agent offers neither.
pub(crate) fn menus(app: &WordApp) -> Option<Menus> {
    if !desktop(app) {
        return None;
    }
    let backend = backend_in_use(app)?;
    let saved = app.session.prefs.assistant.models.get(&backend).cloned().unwrap_or_default();
    #[cfg(not(target_arch = "wasm32"))]
    if backend == wordcraft_assist::chatgpt::ID {
        let may_use = super::chatgpt::account(app).is_some_and(|a| a.plan_usage);
        return may_use.then(|| chatgpt_menus(&app.assist.chatgpt.list, &saved));
    }
    match app.conversation().filter(|a| !a.has_ended() && a.info().id == backend).and_then(Assistant::offered) {
        Some(o) if o.models.is_empty() && o.efforts.is_empty() => None,
        Some(o) => Some(offered_menus(o, &saved)),
        None if backend.starts_with("acp:") => Some(Menus {
            models: saved.model.iter().map(|id| plain_row(id)).collect(),
            chosen_model: saved.model.clone(),
            running_model: saved.model,
            efforts: saved.effort.iter().map(|id| plain_row(id)).collect(),
            effort: saved.effort,
            unknown_because: Some("An agent says what it offers once it has started, with your first prompt.".into()),
            ..Menus::default()
        }),
        None => None,
    }
}

fn plain_row(id: &str) -> Row {
    Row { id: id.to_string(), name: None, description: None, is_default: false }
}

fn model_row(m: &ModelOption) -> Row {
    Row { id: m.id.clone(), name: Some(m.name.clone()), description: m.description.clone(), is_default: false }
}

/// The rows for `levels`, the one that runs when nothing is chosen tagged.
fn level_rows<'a>(levels: impl IntoIterator<Item = &'a str>, default: Option<&str>) -> Vec<Row> {
    levels.into_iter().map(|id| Row { is_default: default == Some(id), ..plain_row(id) }).collect()
}

/// What an agent's conversation says it offers. Its level belongs to the session unless a model
/// names its own.
fn offered_menus(o: &Offered, saved: &ModelChoice) -> Menus {
    let chosen = saved.model.as_ref().filter(|id| o.models.iter().any(|m| m.id == **id)).cloned();
    let running = chosen.as_ref().or(o.model.as_ref()).and_then(|id| o.models.iter().find(|m| m.id == *id));
    let efforts = match running.filter(|m| !m.efforts.is_empty()) {
        Some(m) => level_rows(m.efforts.iter().map(String::as_str), m.default_effort.as_deref()),
        None => o.efforts.iter().map(|l| Row { name: Some(l.name.clone()), ..plain_row(&l.id) }).collect(),
    };
    // Of what this model takes: what is chosen, else its default, else what the session has.
    let listed = |id: &&String| efforts.iter().any(|e: &Row| e.id == **id);
    let tagged = efforts.iter().find(|e| e.is_default).map(|e| &e.id);
    let effort = saved.effort.as_ref().filter(listed).or(tagged).or(o.effort.as_ref().filter(listed)).cloned();
    Menus {
        models: o.models.iter().map(model_row).collect(),
        chosen_model: chosen,
        running_model: running.map(|m| m.name.clone()),
        efforts,
        effort,
        known: true,
        efforts_known: true,
        ..Menus::default()
    }
}

/// The account's list as the window holds it.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
pub(crate) enum ModelList {
    /// Not read, or to be read again.
    #[default]
    Unread,
    Reading(std::sync::mpsc::Receiver<Result<Vec<ModelOption>, String>>),
    Read(Vec<ModelOption>),
    Failed(String),
}

/// What ChatGPT's list and the choice make of the menus, by the table `chatgpt::resolve` follows.
/// Until the list is read: the Default row and what is chosen, and the usual levels.
#[cfg(not(target_arch = "wasm32"))]
fn chatgpt_menus(list: &ModelList, saved: &ModelChoice) -> Menus {
    use wordcraft_assist::chatgpt::{DEFAULT_EFFORT, resolve};
    let (model, effort) = (saved.model.as_deref(), saved.effort.as_deref());
    if let ModelList::Read(list) = list
        && let (Some(runs), Some(default)) = (resolve(list, model, effort), resolve(list, None, None))
    {
        let levels = list.iter().find(|m| m.id == runs.id).map(|m| m.efforts.clone()).unwrap_or_default();
        let tagged = resolve(list, Some(&runs.id), None).and_then(|r| r.effort);
        let efforts = if levels.is_empty() {
            usual_levels(effort, tagged.as_deref())
        } else {
            level_rows(levels.iter().map(String::as_str), tagged.as_deref())
        };
        let efforts_known = !levels.is_empty();
        return Menus {
            models: list.iter().map(model_row).collect(),
            default_model: Some(default.name),
            chosen_model: model.filter(|_| !runs.model_gone).map(str::to_string),
            running_model: Some(runs.name),
            efforts,
            effort: runs.effort,
            known: true,
            efforts_known,
            ..Menus::default()
        };
    }
    Menus {
        models: saved.model.iter().map(|id| plain_row(id)).collect(),
        chosen_model: saved.model.clone(),
        running_model: saved.model.clone(),
        efforts: usual_levels(effort, Some(DEFAULT_EFFORT)),
        effort: Some(effort.unwrap_or(DEFAULT_EFFORT).to_string()),
        unknown_because: (!matches!(list, ModelList::Failed(_))).then(|| "WordCraft is reading the models this account may use.".into()),
        failed: if let ModelList::Failed(why) = list { Some(why.clone()) } else { None },
        ..Menus::default()
    }
}

/// The usual levels for a model that names none, and the one chosen if it is another.
#[cfg(not(target_arch = "wasm32"))]
fn usual_levels(chosen: Option<&str>, default: Option<&str>) -> Vec<Row> {
    level_rows(LEVELS_UNKNOWN.into_iter().chain(chosen.filter(|e| !LEVELS_UNKNOWN.contains(e))), default)
}

/// Every frame: take the list when its read ended; start a read when the pane is open on ChatGPT
/// with someone signed in and none is held. `account_changed`: what is held was someone else's.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn tick(app: &mut WordApp, account_changed: bool) {
    use std::sync::mpsc::TryRecvError;
    use wordcraft_assist::chatgpt::{ID, models_with};
    if account_changed {
        app.assist.chatgpt.list = ModelList::Unread;
    }
    if let ModelList::Reading(read) = &app.assist.chatgpt.list {
        let done = match read.try_recv() {
            Ok(Ok(list)) if !list.is_empty() => Some(ModelList::Read(list)),
            Ok(Ok(_)) => Some(ModelList::Failed("ChatGPT offers no model to this account".into())),
            Ok(Err(why)) => Some(ModelList::Failed(why)),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(ModelList::Failed("the models couldn't be read".into())),
        };
        if let Some(done) = done {
            app.assist.chatgpt.list = done;
        }
    }
    let wanted = matches!(app.assist.chatgpt.list, ModelList::Unread)
        && app.session.view.assistant_pane
        && desktop(app)
        && backend_in_use(app).as_deref() == Some(ID)
        && super::chatgpt::account(app).is_some_and(|a| a.plan_usage);
    let Some(settings) = app.services.settings_dir.clone().filter(|_| wanted) else { return };
    let endpoints = app.assist.chatgpt.endpoints.clone();
    let ctx = app.ctx.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("chatgpt models".into()).spawn(move || {
        let _ = tx.send(models_with(&settings, &endpoints).map_err(|e| e.to_string()));
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    });
    app.assist.chatgpt.list = match spawned {
        Ok(_) => ModelList::Reading(rx),
        Err(e) => ModelList::Failed(format!("the models couldn't be read: {e}")),
    };
}

/// The pane opened: a read that failed is tried again.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn retry_failed(app: &mut WordApp) {
    if matches!(app.assist.chatgpt.list, ModelList::Failed(_)) {
        app.assist.chatgpt.list = ModelList::Unread;
    }
}

//! An agent's session config options, as far as WordCraft uses them: the one for its model and the
//! one for how hard it thinks (categories `model` and `thought_level`). Other categories and
//! `modes` are left alone, as the protocol asks of a client that reads config options, and
//! booleans stay off, since WordCraft doesn't advertise them. All of it is the agent's word:
//! hostile input, with junk skipped and sizes capped.

use serde_json::Value;
use wordcraft_engine::ModelChoice;

use crate::backend::{Level, MAX_OFFERED_LEVELS, MAX_OFFERED_MODELS, ModelOption, Offered, label};

/// Most config options read from one answer, and most values read from one option's list or group.
const MAX_READ: usize = 256;

/// A value of a config option.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Choice {
    pub(super) value: String,
    pub(super) name: String,
    pub(super) description: Option<String>,
}

/// A config option with a list of values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Select {
    pub(super) id: String,
    pub(super) current: String,
    pub(super) choices: Vec<Choice>,
}

impl Select {
    pub(super) fn offers(&self, value: &str) -> bool {
        self.choices.iter().any(|c| c.value == value)
    }

    fn current_name(&self) -> Option<String> {
        self.choices.iter().find(|c| c.value == self.current).map(|c| c.name.clone())
    }
}

/// The two config options WordCraft has controls for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Config {
    pub(super) model: Option<Select>,
    pub(super) thought: Option<Select>,
}

impl Config {
    /// What `answer`'s `configOptions` say: the first option of each category. The protocol puts
    /// the ones to prefer first.
    pub(super) fn parse(answer: &Value) -> Config {
        let options = answer.get("configOptions").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
        let first = |category: &str, cap: usize| {
            options.iter().take(MAX_READ).filter(|o| o.get("category").and_then(Value::as_str) == Some(category)).find_map(|o| select(o, cap))
        };
        Config { model: first("model", MAX_OFFERED_MODELS), thought: first("thought_level", MAX_OFFERED_LEVELS) }
    }

    /// What the menus offer. A level belongs to the session, so it isn't a model's.
    pub(super) fn offered(&self) -> Offered {
        let choices = |o: &Option<Select>| o.as_ref().map(|o| o.choices.clone()).unwrap_or_default();
        Offered {
            models: choices(&self.model)
                .into_iter()
                .map(|c| ModelOption { id: c.value, name: c.name, description: c.description, ..ModelOption::default() })
                .collect(),
            efforts: choices(&self.thought).into_iter().map(|c| Level { id: c.value, name: c.name }).collect(),
            model: self.model.as_ref().map(|o| o.current.clone()),
            effort: self.thought.as_ref().map(|o| o.current.clone()),
        }
    }

    /// The pane's line: the current model's name and level's name. Without a model, nothing.
    pub(super) fn line(&self) -> Option<(String, Option<String>)> {
        let model = self.model.as_ref()?.current_name()?;
        Some((model, self.thought.as_ref().and_then(Select::current_name)))
    }
}

/// `option` as a list of values: `None` for another type (a boolean, one the protocol adds later),
/// or without an id, a current value or any value WordCraft can name.
fn select(option: &Value, cap: usize) -> Option<Select> {
    if option.get("type").and_then(Value::as_str).is_some_and(|t| t != "select") {
        return None;
    }
    let text = |key: &str| option.get(key).and_then(Value::as_str).filter(|v| ModelChoice::valid(v)).map(str::to_string);
    let (id, current) = (text("id")?, text("currentValue")?);
    let listed = option.get("options").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    // Values, or groups of them: the protocol doesn't mix the two, and neither reading minds.
    let flat = listed.iter().take(MAX_READ).flat_map(|entry| match entry.get("options").and_then(Value::as_array) {
        Some(group) => group.iter().take(MAX_READ).collect::<Vec<_>>(),
        None => vec![entry],
    });
    let mut choices: Vec<Choice> = Vec::new();
    for choice in flat.filter_map(choice_in) {
        if choices.len() < cap && !choices.iter().any(|c| c.value == choice.value) {
            choices.push(choice);
        }
    }
    (!choices.is_empty()).then_some(Select { id, current, choices })
}

fn choice_in(entry: &Value) -> Option<Choice> {
    let value = entry.get("value").and_then(Value::as_str).filter(|v| ModelChoice::valid(v))?;
    let text = |key: &str| entry.get(key).and_then(Value::as_str).map(label).filter(|t| !t.trim().is_empty());
    Some(Choice { value: value.to_string(), name: text("name").unwrap_or_else(|| label(value)), description: text("description") })
}

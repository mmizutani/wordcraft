//! Which model and effort run: every row of the table in [`super::models`], and what the list
//! read keeps of a hostile answer.

use serde_json::{Value, json};

use super::models::{Resolved, models_in, resolve};
use crate::backend::ModelOption;

fn model(id: &str, efforts: &[&str], default: Option<&str>) -> ModelOption {
    ModelOption {
        id: id.into(),
        name: id.to_uppercase(),
        description: None,
        efforts: efforts.iter().map(|e| e.to_string()).collect(),
        default_effort: default.map(str::to_string),
    }
}

/// `gpt-6.1-sol` first, `gpt-6-luna` further down, as the list has them.
fn list() -> Vec<ModelOption> {
    vec![
        model("gpt-6.1-sol", &["low", "medium", "high", "xhigh", "max", "ultra"], Some("low")),
        model("gpt-other", &["low", "high"], Some("high")),
        model("gpt-6-luna", &["low", "medium", "high", "xhigh", "max"], Some("medium")),
        model("gpt-nothing-medium", &["low", "high"], None),
        model("gpt-unlisted-levels", &[], None),
    ]
}

fn runs(list: &[ModelOption], model: Option<&str>, effort: Option<&str>) -> (String, Option<String>, bool, bool) {
    let Resolved { id, effort, model_gone, effort_gone, .. } = resolve(list, model, effort).unwrap();
    (id, effort, model_gone, effort_gone)
}

fn expect(id: &str, effort: Option<&str>, model_gone: bool, effort_gone: bool) -> (String, Option<String>, bool, bool) {
    (id.to_string(), effort.map(str::to_string), model_gone, effort_gone)
}

#[test]
fn nothing_chosen_runs_gpt_6_luna_at_medium_wherever_the_list_has_it() {
    assert_eq!(runs(&list(), None, None), expect("gpt-6-luna", Some("medium"), false, false));
}

#[test]
fn a_choice_the_list_has_runs_at_a_level_it_offers() {
    assert_eq!(runs(&list(), Some("gpt-other"), Some("low")), expect("gpt-other", Some("low"), false, false));
    assert_eq!(runs(&list(), Some("gpt-6.1-sol"), Some("ultra")), expect("gpt-6.1-sol", Some("ultra"), false, false));
}

#[test]
fn a_chosen_model_that_left_the_list_falls_back_and_says_so() {
    assert_eq!(runs(&list(), Some("gpt-gone"), None), expect("gpt-6-luna", Some("medium"), true, false));
}

#[test]
fn a_chosen_level_the_model_lacks_falls_back_and_says_so() {
    assert_eq!(runs(&list(), None, Some("ultra")), expect("gpt-6-luna", Some("medium"), false, true));
    // The level is checked against the model that runs, not the one chosen.
    assert_eq!(runs(&list(), Some("gpt-gone"), Some("ultra")), expect("gpt-6-luna", Some("medium"), true, true));
}

#[test]
fn without_gpt_6_luna_the_first_runs_at_medium_when_offered() {
    let without: Vec<ModelOption> = list().into_iter().filter(|m| m.id != "gpt-6-luna").collect();
    assert_eq!(runs(&without, None, None), expect("gpt-6.1-sol", Some("medium"), false, false));
}

#[test]
fn a_model_without_medium_runs_at_its_default_and_without_one_sends_nothing() {
    assert_eq!(runs(&list(), Some("gpt-other"), None), expect("gpt-other", Some("high"), false, false));
    assert_eq!(runs(&list(), Some("gpt-nothing-medium"), None), expect("gpt-nothing-medium", None, false, false));
}

#[test]
fn a_model_with_no_levels_listed_takes_any_level() {
    assert_eq!(runs(&list(), Some("gpt-unlisted-levels"), None), expect("gpt-unlisted-levels", Some("medium"), false, false));
    assert_eq!(runs(&list(), Some("gpt-unlisted-levels"), Some("high")), expect("gpt-unlisted-levels", Some("high"), false, false));
}

#[test]
fn an_empty_list_runs_nothing() {
    assert_eq!(resolve(&[], Some("gpt-6-luna"), Some("medium")), None);
}

#[test]
fn the_list_gives_each_model_its_levels_as_strings_or_as_objects() {
    let listed = json!({"models": [
        {"slug": "a", "display_name": "A", "description": "Says\u{202E} what it is.", "default_reasoning_level": "low",
         "supported_reasoning_levels": ["low", "medium"]},
        {"slug": "b", "supported_reasoning_levels": [{"effort": "high", "description": "x"}], "default_reasoning_level": "low"},
        {"id": "c", "object": "model"},
    ]});
    let models = models_in(&listed);
    assert_eq!(models[0].efforts, ["low", "medium"]);
    assert_eq!(models[0].default_effort.as_deref(), Some("low"));
    assert_eq!(models[0].description.as_deref(), Some("Says what it is."));
    assert_eq!(models[1].efforts, ["high"]);
    assert_eq!(models[1].default_effort, None, "a default it doesn't offer isn't its default");
    assert_eq!((models[2].efforts.len(), models[2].default_effort.clone()), (0, None));
}

#[test]
fn the_list_survives_junk_levels_huge_lists_and_control_characters() {
    let junk_levels = json!([7, null, "", "two words", "x\n", {"effort": 3}, {"no": "effort"}, [], "x".repeat(33), "low", "low", {"effort": "high"}]);
    let many: Vec<Value> = (0..1000).map(|i| json!(format!("level{i}"))).collect();
    let models = models_in(&json!({"models": [
        {"slug": "a", "display_name": "A\u{0}\u{7}\u{202E}!", "supported_reasoning_levels": junk_levels, "default_reasoning_level": {"effort": []}},
        {"slug": "b", "display_name": "n".repeat(10_000), "description": "d".repeat(10_000), "supported_reasoning_levels": many},
        {"slug": "c", "display_name": 7, "description": [], "supported_reasoning_levels": "low", "default_reasoning_level": 9},
        {"slug": "d", "display_name": " \u{0}\u{202E}"},
    ]}));
    assert_eq!(models[0].name, "A!");
    assert_eq!(models[0].efforts, ["low", "high"]);
    assert_eq!(models[0].default_effort, None);
    assert_eq!(models[1].efforts.len(), 16);
    assert_eq!((models[1].name.chars().count(), models[1].description.as_ref().map(|d| d.chars().count())), (80, Some(300)));
    assert_eq!((models[2].name.as_str(), models[2].efforts.len(), models[2].description.clone()), ("c", 0, None));
    assert_eq!(models[3].name, "d", "a name with nothing to show is its id");
}

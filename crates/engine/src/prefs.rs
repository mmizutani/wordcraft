use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::room::Role;

/// Editing preferences that persist between runs (the front end saves and restores them).
/// File › Options sets most of them (`file.options`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Prefs {
    /// Word Count includes text boxes, footnotes and endnotes (Word's default).
    pub count_notes: bool,
    /// Track Changes Options: what markup shows and how revisions are drawn (per user, as in
    /// Word).
    pub markup: wordcraft_layout::display::MarkupOptions,
    /// File › Print / Export: Print Markup. PDFs, prints and page images show comments in
    /// balloons beside each page and tracked changes as marked up; HTML, Markdown and text
    /// copies end with the comments (`file.printMarkup`).
    pub print_markup: bool,
    /// Language › Detect language automatically (per user, as in Word).
    pub detect_language: bool,
    /// Set Proofing Language › Set As Default: overrides the interface language for future
    /// blank documents and plain text. Older preferences keep following the interface.
    #[serde(deserialize_with = "lenient_default_language")]
    pub default_language: Option<String>,
    /// Typing over a selection replaces it; off, the typed text goes in front of it.
    pub typing_replaces_selection: bool,
    /// The Insert key switches overtype on and off.
    pub insert_key_overtype: bool,
    /// Overtype: typed characters replace the ones after the caret.
    pub overtype: bool,
    /// The unit rulers and dialogs show lengths in, when the user chose one (File › Options ›
    /// Advanced › Measurement units); `None` follows the region ([`Session::units`]). Saved under
    /// a new key; an older `units` value other than inches (the old default) carries over on load.
    #[serde(rename = "measurementUnits")]
    pub units: Option<wordcraft_geom::Unit>,
    /// The region the user chose for new documents, units and the decimal separator (a locale
    /// tag like `pl-PL`, File › Options › Advanced › Region); `None` follows the system.
    pub region: Option<String>,
    /// A folder searched for Hunspell dictionaries before the system locations (File › Options ›
    /// Language, #1063): an app started from Finder or the Dock doesn't see
    /// `WORDCRAFT_DICTIONARIES`. `None` searches the system locations only.
    pub dictionaries_folder: Option<String>,
    /// The user templates folder File › New lists personal templates from and Save as Word
    /// Template starts in (File › Options › Advanced › File locations, #2852). `None`: the app's
    /// own `Templates` folder in its settings folder.
    pub user_templates_folder: Option<String>,
    /// Formatting marks shown even while Show/Hide ¶ is off.
    pub marks: wordcraft_layout::display::FormattingMarks,
    /// Mark grammar errors as you type (with Check spelling as you type on).
    pub mark_grammar: bool,
    /// Check spelling as you type (File › Options › Proofing). The live value is
    /// `ViewState::proofing`; this is the copy that is saved and restored (#1792).
    pub check_spelling: bool,
    /// Read Mode's Page Color. The live value is `ViewState::read_page_color`.
    #[serde(deserialize_with = "lenient_read_page_color")]
    pub read_page_color: crate::cmd::view::ReadPageColor,
    pub ignore_uppercase: bool,
    pub ignore_numbers: bool,
    pub ignore_internet: bool,
    /// AutoCorrect Options: replacements, capitalization and AutoFormat As You Type (per user, as
    /// in Word). A damaged saved value reads as the defaults.
    #[serde(deserialize_with = "crate::cmd::tools::lenient_autocorrect")]
    pub autocorrect: crate::cmd::tools::AutoCorrectPrefs,
    /// The built-in assistant's settings (File › Options › Assistant). A damaged saved value
    /// reads as the defaults.
    #[serde(deserialize_with = "lenient_assistant")]
    pub assistant: AssistantPrefs,
    /// File › Options › Agents, "Files agents can use" (#31): which files the commands of agents
    /// in a shared document can use, unless their invite chose for them. A damaged saved value
    /// reads as none.
    #[serde(deserialize_with = "lenient_agent_files")]
    pub agent_files: crate::agent_files::AgentFiles,
    /// The folders "Only these folders" gives agents, each read-only or read and write; kept
    /// while another choice is made. Damaged entries are dropped
    /// ([`crate::agent_files::cleaned`]).
    #[serde(deserialize_with = "lenient_agent_folders")]
    pub agent_folders: Vec<crate::agent_files::AgentFolder>,
}

/// The built-in assistant's settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AssistantPrefs {
    /// The role assistants join a shared document with (`assistant.defaultRole`): suggester
    /// unless the person changes it, so every edit an assistant makes is a tracked change in its
    /// name. Viewer, commenter, suggester or editor; never owner, which would let an assistant
    /// change the application's settings. A conversation may choose its own.
    #[serde(deserialize_with = "lenient_assistant_role")]
    pub default_role: Role,
    /// Whether an assistant goes ahead when its agent asks permission in a turn no one can
    /// answer (`assistant.allowUnattended`): one a program asked for (the control channel, MCP)
    /// rather than the person in the Assistant pane. Off unless the person turns it on: such
    /// requests are refused, and each answer is logged.
    #[serde(deserialize_with = "lenient_flag")]
    pub allow_unattended: bool,
    /// The model and reasoning effort the person chose, one choice per backend id
    /// (`assist.model`, `assist.effort`). The window copies the entry into each turn. An entry
    /// that no longer resolves stays: the pane says what ran instead.
    #[serde(deserialize_with = "lenient_models", skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<String, ModelChoice>,
}

/// The model and reasoning effort a person chose for one backend; `None`: its default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelChoice {
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl ModelChoice {
    /// Most backends a saved file keeps a choice for.
    const MAX_SAVED: usize = 64;

    /// Whether `value` can name a backend, a model or a level: not empty, at most
    /// [`MAX_CHOICE_CHARS`] characters, no control characters.
    pub fn valid(value: &str) -> bool {
        !value.trim().is_empty() && value.chars().count() <= MAX_CHOICE_CHARS && !value.chars().any(char::is_control)
    }
}

/// Longest id of a backend, a model or a level taken, in characters.
pub const MAX_CHOICE_CHARS: usize = 128;

impl Default for AssistantPrefs {
    fn default() -> Self {
        AssistantPrefs { default_role: Role::Suggester, allow_unattended: false, models: BTreeMap::new() }
    }
}

impl AssistantPrefs {
    /// The roles an assistant may join with, from the one that may do least: every role but
    /// owner.
    pub const ROLES: [Role; 4] = [Role::Viewer, Role::Commenter, Role::Suggester, Role::Editor];

    /// The role assistants join with: the setting, read as suggester if it is one an assistant
    /// may not have (owner).
    pub fn default_role(&self) -> Role {
        if Self::ROLES.contains(&self.default_role) { self.default_role } else { Role::Suggester }
    }

    /// The role called `name` that an assistant may have: viewer, commenter, suggester or
    /// editor. Owner and anything else are refused, with why.
    pub fn role_named(name: &str) -> Result<Role, String> {
        match Role::named(name.trim()) {
            Some(Role::Owner) => Err("an assistant can't be an owner: owners change the application's settings and share the document".into()),
            Some(role) => Ok(role),
            None => Err(format!("`{}`: viewer, commenter, suggester or editor", name.chars().take(64).collect::<String>())),
        }
    }
}

/// Read the assistant's settings, the defaults for a damaged value, without discarding the other
/// preferences.
fn lenient_assistant<'de, D: serde::Deserializer<'de>>(d: D) -> Result<AssistantPrefs, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// The saved choices, junk left out: a value that is damaged reads as no choice, and an entry
/// that isn't a choice is dropped, without discarding the rest.
fn lenient_models<'de, D: serde::Deserializer<'de>>(d: D) -> Result<BTreeMap<String, ModelChoice>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    let entries = value.as_object().into_iter().flatten();
    let saved =
        |v: &serde_json::Value, key: &str| v.get(key).and_then(serde_json::Value::as_str).filter(|s| ModelChoice::valid(s)).map(str::to_string);
    Ok(entries
        .filter(|(id, v)| ModelChoice::valid(id) && v.is_object())
        .map(|(id, v)| (id.clone(), ModelChoice { model: saved(v, "model"), effort: saved(v, "effort") }))
        .take(ModelChoice::MAX_SAVED)
        .collect())
}

/// A saved switch, or off for junk.
fn lenient_flag<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(value.as_bool().unwrap_or(false))
}

/// A saved role an assistant may have, or suggester for owner, an unknown name or junk.
fn lenient_assistant_role<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Role, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(value.as_str().and_then(|name| AssistantPrefs::role_named(name).ok()).unwrap_or(Role::Suggester))
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            count_notes: true,
            markup: Default::default(),
            print_markup: false,
            typing_replaces_selection: true,
            insert_key_overtype: false,
            overtype: false,
            units: Default::default(),
            region: None,
            dictionaries_folder: None,
            user_templates_folder: None,
            marks: Default::default(),
            mark_grammar: true,
            check_spelling: true,
            read_page_color: Default::default(),
            ignore_uppercase: true,
            ignore_numbers: true,
            ignore_internet: true,
            detect_language: true,
            default_language: None,
            autocorrect: Default::default(),
            assistant: Default::default(),
            agent_files: Default::default(),
            agent_folders: Vec::new(),
        }
    }
}

impl Prefs {
    /// What the spelling check skips.
    pub fn spelling(&self) -> wordcraft_proof::Options {
        wordcraft_proof::Options {
            ignore_uppercase: self.ignore_uppercase,
            ignore_numbers: self.ignore_numbers,
            ignore_internet: self.ignore_internet,
        }
    }
}

/// Ignore malformed saved language tags without discarding the other preferences.
fn lenient_default_language<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(value.as_str().and_then(wordcraft_proof::lang::normalize_tag))
}

/// Read a damaged saved choice of the files agents can use as none, keeping the other
/// preferences.
fn lenient_agent_files<'de, D: serde::Deserializer<'de>>(d: D) -> Result<crate::agent_files::AgentFiles, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// Keep the saved folders agents can use that are still good ones, dropping the rest.
fn lenient_agent_folders<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<crate::agent_files::AgentFolder>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    let folders = match value {
        serde_json::Value::Array(items) => items.into_iter().filter_map(|f| serde_json::from_value(f).ok()).collect(),
        _ => Vec::new(),
    };
    Ok(crate::agent_files::cleaned(folders))
}

/// Ignore an invalid saved Page Color without discarding other preferences.
fn lenient_read_page_color<'de, D: serde::Deserializer<'de>>(d: D) -> Result<crate::cmd::view::ReadPageColor, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::{AssistantPrefs, Prefs};
    use super::{MAX_CHOICE_CHARS, ModelChoice};
    use crate::agent_files::{AgentFiles, AgentFolder};
    use crate::cmd::view::ReadPageColor;
    use crate::room::Role;

    #[test]
    fn the_files_agents_can_use_load_from_old_and_damaged_prefs() {
        let old: Prefs = serde_json::from_str(r#"{"checkSpelling":false}"#).unwrap();
        assert_eq!((old.agent_files, old.agent_folders.len()), (AgentFiles::None, 0), "none, as before the setting");
        let root = if cfg!(windows) { r"C:\\work" } else { "/work" };
        let saved: Prefs = serde_json::from_str(&format!(
            r#"{{"agentFiles":"folders","agentFolders":[{{"path":"{root}","write":true}},{{"path":"relative"}},7,{{"write":true}}],"checkSpelling":false}}"#
        ))
        .unwrap();
        assert_eq!(saved.agent_files, AgentFiles::Folders);
        assert_eq!(saved.agent_folders, [AgentFolder { path: root.replace(r"\\", r"\"), write: true }], "damaged entries are dropped");
        assert!(!saved.check_spelling);
        for bad in [
            r#"{"agentFiles":"ask","agentFolders":{"path":"/x"},"checkSpelling":false}"#,
            r#"{"agentFiles":3,"agentFolders":"/x","checkSpelling":false}"#,
        ] {
            let prefs: Prefs = serde_json::from_str(bad).unwrap();
            assert_eq!((prefs.agent_files, prefs.agent_folders.len()), (AgentFiles::None, 0), "{bad}");
            assert!(!prefs.check_spelling, "the other preferences stay: {bad}");
        }
        let again: Prefs = serde_json::from_value(serde_json::to_value(&saved).unwrap()).unwrap();
        assert_eq!(again, saved);
        assert_eq!(serde_json::to_value(&saved).unwrap()["agentFiles"], "folders");
    }

    #[test]
    fn read_page_color_loads_from_old_and_damaged_prefs() {
        let old: Prefs = serde_json::from_str(r#"{"checkSpelling":false}"#).unwrap();
        assert_eq!(old.read_page_color, ReadPageColor::None);
        assert!(!old.check_spelling);
        for bad in [r#"{"readPageColor":"unknown","checkSpelling":false}"#, r#"{"readPageColor":42,"checkSpelling":false}"#] {
            let prefs: Prefs = serde_json::from_str(bad).unwrap();
            assert_eq!(prefs.read_page_color, ReadPageColor::None);
            assert!(!prefs.check_spelling);
        }
        let saved: Prefs = serde_json::from_str(r#"{"readPageColor":"sepia"}"#).unwrap();
        assert_eq!(saved.read_page_color, ReadPageColor::Sepia);
        assert_eq!(serde_json::to_value(saved).unwrap()["readPageColor"], "sepia");
    }

    /// Assistants join as suggesters unless the person chose another role; a saved owner, an
    /// unknown role or junk reads as suggester, and keeps the other preferences.
    #[test]
    fn the_model_choices_survive_a_save_and_junk_reads_as_no_choice() {
        let saved: Prefs =
            serde_json::from_str(r#"{"assistant":{"models":{"chatgpt":{"model":"gpt-x","effort":"high"},"acp:a":{"effort":"low"}}}}"#).unwrap();
        let again: Prefs = serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        assert_eq!(again.assistant.models, saved.assistant.models);
        assert_eq!(saved.assistant.models["chatgpt"], ModelChoice { model: Some("gpt-x".into()), effort: Some("high".into()) });
        assert_eq!(saved.assistant.models["acp:a"], ModelChoice { model: None, effort: Some("low".into()) });
        // Damaged: the rest of the preferences stay, and so do the good choices.
        let long = "m".repeat(MAX_CHOICE_CHARS + 1);
        let damaged = format!(
            r#"{{"assistant":{{"defaultRole":"editor","models":{{"chatgpt":{{"model":7,"effort":"two\nlines"}},"junk":3,"":{{"model":"x"}},"good":{{"model":"gpt-y"}},"long":{{"model":"{long}"}}}}}},"checkSpelling":false}}"#
        );
        let read: Prefs = serde_json::from_str(&damaged).unwrap();
        assert_eq!(read.assistant.default_role, Role::Editor);
        assert!(!read.check_spelling);
        assert_eq!(read.assistant.models["chatgpt"], ModelChoice::default());
        assert_eq!(read.assistant.models["good"].model.as_deref(), Some("gpt-y"));
        assert_eq!(read.assistant.models["long"], ModelChoice::default());
        assert_eq!(read.assistant.models.len(), 3);
        let not_a_map: Prefs = serde_json::from_str(r#"{"assistant":{"models":[1,2]}}"#).unwrap();
        assert!(not_a_map.assistant.models.is_empty());
        let many: serde_json::Map<String, serde_json::Value> = (0..1000).map(|i| (format!("b{i}"), serde_json::json!({}))).collect();
        let huge: Prefs = serde_json::from_value(serde_json::json!({"assistant": {"models": many}})).unwrap();
        assert_eq!(huge.assistant.models.len(), 64);
    }

    #[test]
    fn the_assistant_role_is_suggester_unless_chosen_and_never_owner() {
        assert_eq!(Prefs::default().assistant.default_role(), Role::Suggester);
        let old: Prefs = serde_json::from_str(r#"{"checkSpelling":false}"#).unwrap();
        assert_eq!(old.assistant.default_role(), Role::Suggester);
        let editor: Prefs = serde_json::from_str(r#"{"assistant":{"defaultRole":"editor"}}"#).unwrap();
        assert_eq!(editor.assistant.default_role(), Role::Editor);
        assert_eq!(serde_json::to_value(&editor).unwrap()["assistant"], serde_json::json!({"defaultRole": "editor", "allowUnattended": false}));
        for bad in [
            r#"{"assistant":{"defaultRole":"owner"},"checkSpelling":false}"#,
            r#"{"assistant":{"defaultRole":"admin"},"checkSpelling":false}"#,
            r#"{"assistant":{"defaultRole":42},"checkSpelling":false}"#,
            r#"{"assistant":"editor","checkSpelling":false}"#,
            r#"{"assistant":null,"checkSpelling":false}"#,
        ] {
            let prefs: Prefs = serde_json::from_str(bad).unwrap();
            assert_eq!(prefs.assistant.default_role(), Role::Suggester, "{bad}");
            assert!(!prefs.check_spelling, "{bad}: the other preferences are kept");
        }
        // Set in code to owner, it still reads as suggester.
        let owner = AssistantPrefs { default_role: Role::Owner, ..AssistantPrefs::default() };
        assert_eq!(owner.default_role(), Role::Suggester);
        assert_eq!(AssistantPrefs::role_named(" editor ").unwrap(), Role::Editor);
        assert!(AssistantPrefs::role_named("owner").unwrap_err().contains("owner"));
        assert!(AssistantPrefs::role_named(&"x".repeat(10_000)).unwrap_err().len() < 200);
    }

    /// Requests no one can answer are refused unless the person allowed them; junk reads as
    /// refused, and keeps the role and the other preferences.
    #[test]
    fn unattended_requests_are_refused_unless_allowed() {
        assert!(!Prefs::default().assistant.allow_unattended);
        let old: Prefs = serde_json::from_str(r#"{"assistant":{"defaultRole":"editor"}}"#).unwrap();
        assert!(!old.assistant.allow_unattended);
        let allowed: Prefs = serde_json::from_str(r#"{"assistant":{"allowUnattended":true}}"#).unwrap();
        assert!(allowed.assistant.allow_unattended);
        assert_eq!(allowed.assistant.default_role(), Role::Suggester);
        assert_eq!(serde_json::to_value(&allowed).unwrap()["assistant"]["allowUnattended"], true);
        for bad in [
            r#"{"assistant":{"defaultRole":"editor","allowUnattended":"yes"},"checkSpelling":false}"#,
            r#"{"assistant":{"defaultRole":"editor","allowUnattended":1},"checkSpelling":false}"#,
        ] {
            let prefs: Prefs = serde_json::from_str(bad).unwrap();
            assert!(!prefs.assistant.allow_unattended, "{bad}");
            assert_eq!(prefs.assistant.default_role(), Role::Editor, "{bad}: the role is kept");
            assert!(!prefs.check_spelling, "{bad}: the other preferences are kept");
        }
    }
}

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
}

impl Default for AssistantPrefs {
    fn default() -> Self {
        AssistantPrefs { default_role: Role::Suggester }
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

/// Ignore an invalid saved Page Color without discarding other preferences.
fn lenient_read_page_color<'de, D: serde::Deserializer<'de>>(d: D) -> Result<crate::cmd::view::ReadPageColor, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::{AssistantPrefs, Prefs};
    use crate::cmd::view::ReadPageColor;
    use crate::room::Role;

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
    fn the_assistant_role_is_suggester_unless_chosen_and_never_owner() {
        assert_eq!(Prefs::default().assistant.default_role(), Role::Suggester);
        let old: Prefs = serde_json::from_str(r#"{"checkSpelling":false}"#).unwrap();
        assert_eq!(old.assistant.default_role(), Role::Suggester);
        let editor: Prefs = serde_json::from_str(r#"{"assistant":{"defaultRole":"editor"}}"#).unwrap();
        assert_eq!(editor.assistant.default_role(), Role::Editor);
        assert_eq!(serde_json::to_value(&editor).unwrap()["assistant"], serde_json::json!({"defaultRole": "editor"}));
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
        let owner = AssistantPrefs { default_role: Role::Owner };
        assert_eq!(owner.default_role(), Role::Suggester);
        assert_eq!(AssistantPrefs::role_named(" editor ").unwrap(), Role::Editor);
        assert!(AssistantPrefs::role_named("owner").unwrap_err().contains("owner"));
        assert!(AssistantPrefs::role_named(&"x".repeat(10_000)).unwrap_err().len() < 200);
    }
}

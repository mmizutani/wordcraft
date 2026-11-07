//! File › Options (#485): the settings agents can read and change without the window — user
//! name and initials, typing (replace selection, overtype), measurement units, formatting marks
//! and proofing. The Options page in the app sets them through the same command.

use serde_json::{Value, json};

use crate::agent_files::{AgentFiles, AgentFolder, MAX_AGENT_FOLDERS};
use crate::{CmdError, CmdResult, CommandSpec, Prefs, Session, p};

/// Options panes, in the order the page lists them (the `pane` param of `file.options`).
pub const PANES: [&str; 10] = ["general", "display", "proofing", "save", "language", "accessibility", "advanced", "agents", "account", "assistant"];

/// Longest initials kept, in characters.
pub const MAX_INITIALS_CHARS: usize = 9;
/// Longest folder path (dictionaries, user templates, agents' folders) taken, in characters.
const MAX_FOLDER_CHARS: usize = crate::agent_files::MAX_FOLDER_CHARS;
/// Longest word the custom dictionary takes, in characters.
pub const MAX_WORD_CHARS: usize = 64;
/// Most words one call may add or remove.
const MAX_WORDS_PER_CALL: usize = 1000;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("file.options", "Options", "File", options)
            .params(
                r#"{"pane"?: "general|display|proofing|save|language|accessibility|advanced|agents|account|assistant", "author"?: string, "initials"?: string, "units"?: "inches|centimeters|millimeters|points|picas|auto" (auto: the region's), "region"?: string (a locale tag like pl-PL, saved: the paper File › New starts on, the default units and the decimal separator; auto: the system's), "typingReplacesSelection"?: bool, "insertKeyOvertype"?: bool, "overtype"?: bool, "marks"?: {"tabs"?: bool, "spaces"?: bool, "paragraphs"?: bool, "hidden"?: bool}, "checkSpelling"?: bool, "markGrammar"?: bool, "ignoreUppercase"?: bool, "ignoreNumbers"?: bool, "ignoreInternet"?: bool, "dictionariesFolder"?: string (a folder of Hunspell .aff/.dic files searched first, saved; auto: the system locations only), "userTemplatesFolder"?: string (the folder File › New lists personal templates from, saved; auto: the app's own Templates folder; not a network folder), "addWords"?: [string], "removeWords"?: [string], "agentFiles"?: "none|folders|all" (which files the commands of agents invited to a shared document can use: none, only agentFolders, or any file; the most any of them gets, as an invite may give its agent fewer), "agentFolders"?: [{"path": string (a full folder path, not a network folder), "write"?: bool (they can write in it too)}] (at most 32; replaces the list)} — with any setting it changes them and opens nothing; without, it opens File › Options (at `pane`). Returns the settings. A participant's copy of a shared document doesn't run it."#,
            )
            .pure(),
        CommandSpec::new("text.overtype", "Overtype", "File › Options › Advanced", |s, v| {
            s.prefs.overtype = p::bool(v, "value").unwrap_or(!s.prefs.overtype);
            Ok(json!({"value": s.prefs.overtype}))
        })
        .params(r#"{"value"?: bool (default: toggle)}"#)
        .pure(),
    ]
}

/// The on/off settings by their `file.options` name.
fn flags() -> [(&'static str, fn(&mut Prefs) -> &mut bool); 7] {
    [
        ("typingReplacesSelection", |p| &mut p.typing_replaces_selection),
        ("insertKeyOvertype", |p| &mut p.insert_key_overtype),
        ("overtype", |p| &mut p.overtype),
        ("markGrammar", |p| &mut p.mark_grammar),
        ("ignoreUppercase", |p| &mut p.ignore_uppercase),
        ("ignoreNumbers", |p| &mut p.ignore_numbers),
        ("ignoreInternet", |p| &mut p.ignore_internet),
    ]
}

fn mark_flags() -> [(&'static str, fn(&mut wordcraft_layout::display::FormattingMarks) -> &mut bool); 4] {
    [("tabs", |m| &mut m.tabs), ("spaces", |m| &mut m.spaces), ("paragraphs", |m| &mut m.paragraphs), ("hidden", |m| &mut m.hidden)]
}

fn bool_param(v: &Value, k: &str) -> Result<Option<bool>, CmdError> {
    match v.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(CmdError::Params(format!("`{k}` must be true or false"))),
    }
}

/// Words for the custom dictionary: trimmed, single words, at most [`MAX_WORD_CHARS`] long.
fn words_param(v: &Value, k: &str) -> Result<Vec<String>, CmdError> {
    let Some(x) = v.get(k).filter(|x| !x.is_null()) else { return Ok(Vec::new()) };
    let bad = || CmdError::Params(format!("`{k}` must be a list of words"));
    let list = x.as_array().ok_or_else(bad)?;
    if list.len() > MAX_WORDS_PER_CALL {
        return Err(CmdError::Params(format!("`{k}` takes at most {MAX_WORDS_PER_CALL} words")));
    }
    let mut out = Vec::new();
    for w in list {
        let w = w.as_str().ok_or_else(bad)?.trim();
        if w.is_empty() || w.chars().count() > MAX_WORD_CHARS || w.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(CmdError::Params(format!("`{k}`: {w:?} isn't a single word of at most {MAX_WORD_CHARS} characters")));
        }
        out.push(w.to_string());
    }
    Ok(out)
}

fn options(s: &mut Session, v: &Value) -> CmdResult {
    let pane = match p::str(v, "pane") {
        Some(name) if PANES.contains(&name) => name,
        Some(name) => return Err(CmdError::Params(format!("unknown pane `{name}`; one of {}", PANES.join(", ")))),
        None => "general",
    };
    // Read every setting first, so a bad one changes nothing.
    let author = match v.get("author") {
        None | Some(Value::Null) => None,
        Some(Value::String(n)) => Some(n.clone()),
        Some(_) => return Err(CmdError::Params("`author` must be a string".into())),
    };
    let initials = match v.get("initials") {
        None | Some(Value::Null) => None,
        Some(Value::String(i)) => Some(i.trim().chars().filter(|c| !c.is_control()).take(MAX_INITIALS_CHARS).collect::<String>()),
        Some(_) => return Err(CmdError::Params("`initials` must be a string".into())),
    };
    let units = match v.get("units") {
        None | Some(Value::Null) => None,
        // `auto` forgets the choice: lengths follow the region again (#838).
        Some(Value::String(a)) if a == "auto" => Some(None),
        Some(u) => Some(Some(
            serde_json::from_value::<wordcraft_geom::Unit>(u.clone())
                .map_err(|_| CmdError::Params("`units` is one of inches, centimeters, millimeters, points, picas, auto".into()))?,
        )),
    };
    let region = match v.get("region") {
        None | Some(Value::Null) => None,
        // `auto` (or an empty tag) forgets the choice: the system's region again.
        Some(Value::String(t)) if t.trim().is_empty() || t.trim().eq_ignore_ascii_case("auto") => Some(None),
        // Kept as the tag (normalised, short), so it is saved and read back as chosen.
        Some(Value::String(t)) => {
            let r = wordcraft_geom::Region::from_tag(t);
            Some(Some(if r.territory.is_empty() { r.language } else { format!("{}-{}", r.language, r.territory) }))
        }
        Some(_) => return Err(CmdError::Params("`region` must be a locale tag like pl-PL, or auto".into())),
    };
    let folder = match v.get("dictionariesFolder") {
        None | Some(Value::Null) => None,
        Some(Value::String(f)) => Some(folder_setting("dictionariesFolder", f, false)?),
        Some(_) => return Err(CmdError::Params("`dictionariesFolder` must be a folder path, or auto".into())),
    };
    let templates_folder = match v.get("userTemplatesFolder") {
        None | Some(Value::Null) => None,
        // Never a folder on another machine (#2852): listing it would make Windows authenticate there.
        Some(Value::String(f)) => Some(folder_setting("userTemplatesFolder", f, true)?),
        Some(_) => return Err(CmdError::Params("`userTemplatesFolder` must be a folder path, or auto".into())),
    };
    let mut flag_values = Vec::new();
    for (k, f) in flags() {
        if let Some(b) = bool_param(v, k)? {
            flag_values.push((f, b));
        }
    }
    let mut mark_values = Vec::new();
    match v.get("marks") {
        None | Some(Value::Null) => {}
        Some(m @ Value::Object(_)) => {
            for (k, f) in mark_flags() {
                if let Some(b) = bool_param(m, k)? {
                    mark_values.push((f, b));
                }
            }
        }
        Some(_) => return Err(CmdError::Params("`marks` must be an object of true/false".into())),
    }
    let check_spelling = bool_param(v, "checkSpelling")?;
    let add = words_param(v, "addWords")?;
    let remove = words_param(v, "removeWords")?;
    let agent_files = match v.get("agentFiles") {
        None | Some(Value::Null) => None,
        Some(Value::String(f)) => {
            Some(AgentFiles::named(f).ok_or_else(|| CmdError::Params(format!("`agentFiles` is none, folders or all, not {:?}", echo(f))))?)
        }
        Some(_) => return Err(CmdError::Params("`agentFiles` is none, folders or all".into())),
    };
    let agent_folders = match v.get("agentFolders") {
        None | Some(Value::Null) => None,
        Some(Value::Array(items)) => Some(agent_folders(items, &s.prefs.agent_folders)?),
        Some(_) => return Err(CmdError::Params("`agentFolders` must be a list of {\"path\", \"write\"?}".into())),
    };

    let setting = author.is_some()
        || initials.is_some()
        || units.is_some()
        || region.is_some()
        || folder.is_some()
        || templates_folder.is_some()
        || !flag_values.is_empty()
        || !mark_values.is_empty()
        || check_spelling.is_some()
        || !add.is_empty()
        || !remove.is_empty()
        || agent_files.is_some()
        || agent_folders.is_some();
    if let Some(name) = author {
        super::file::set_author(s, &json!({"name": name}))?;
    }
    if let Some(i) = initials {
        s.initials = i;
    }
    if let Some(u) = units {
        s.prefs.units = u;
    }
    if let Some(r) = region {
        s.prefs.region = r;
    }
    if let Some(f) = folder {
        use_dictionaries_folder(f.as_deref());
        s.prefs.dictionaries_folder = f;
    }
    if let Some(f) = templates_folder {
        s.prefs.user_templates_folder = f;
    }
    for (f, b) in flag_values {
        *f(&mut s.prefs) = b;
    }
    for (f, b) in mark_values {
        *f(&mut s.prefs.marks) = b;
    }
    if let Some(b) = check_spelling {
        s.view.proofing = b;
        s.prefs.check_spelling = b;
    }
    for w in &add {
        wordcraft_proof::add_word(w);
    }
    for w in &remove {
        wordcraft_proof::remove_word(w);
    }
    if !add.is_empty() || !remove.is_empty() {
        s.persist_user_dictionary(&remove);
    }
    let agents = agent_files.is_some() || agent_folders.is_some();
    if let Some(f) = agent_files {
        s.prefs.agent_files = f;
    }
    if let Some(f) = agent_folders {
        s.prefs.agent_folders = f;
    }
    if agents {
        // Agents already in a shared document reach what it says from their next request on.
        s.tell_room_agent_files();
    }
    if setting {
        // Marks, hidden text and proofing change what is laid out.
        s.relayout();
    } else {
        s.push_ui_request(json!({"open": "options", "pane": pane}));
    }
    let mut r = current(s);
    if let Some(o) = r.as_object_mut() {
        o.insert("opened".into(), json!(!setting));
    }
    Ok(r)
}

/// The settings `file.options` reads and sets.
pub fn current(s: &Session) -> Value {
    let pr = &s.prefs;
    let region = s.region();
    json!({
        "author": s.author,
        "initials": s.user_initials(),
        "units": s.units(),
        "unitsFromRegion": pr.units.is_none(),
        "region": {"language": region.language, "territory": region.territory, "paper": region.paper().0, "decimal": region.decimal().to_string()},
        "regionFromSystem": pr.region.is_none(),
        "typingReplacesSelection": pr.typing_replaces_selection,
        "insertKeyOvertype": pr.insert_key_overtype,
        "overtype": pr.overtype,
        "marks": pr.marks,
        "checkSpelling": s.view.proofing,
        "markGrammar": pr.mark_grammar,
        "ignoreUppercase": pr.ignore_uppercase,
        "ignoreNumbers": pr.ignore_numbers,
        "ignoreInternet": pr.ignore_internet,
        "dictionary": wordcraft_proof::user_dictionary(),
        "dictionariesFolder": pr.dictionaries_folder,
        "userTemplatesFolder": pr.user_templates_folder,
        "agentFiles": pr.agent_files,
        "agentFolders": pr.agent_folders,
        "dictionaries": wordcraft_proof::installed_dictionaries()
            .into_iter()
            .map(|(lang, dic)| json!({"language": lang.tag(), "file": dic.map(|d| d.to_string_lossy().into_owned())}))
            .collect::<Vec<_>>(),
    })
}

/// A folder setting (`dictionariesFolder`, `userTemplatesFolder`): `auto` (or nothing) forgets
/// it; otherwise an existing folder, given as an absolute path or one starting with `~/`. With
/// `local_only`, a network folder is refused before anything looks at it (#2852): even checking
/// that it exists would make Windows authenticate to that host.
fn folder_setting(key: &str, f: &str, local_only: bool) -> Result<Option<String>, CmdError> {
    let f = f.trim();
    if f.is_empty() || f.eq_ignore_ascii_case("auto") {
        return Ok(None);
    }
    if f.chars().count() > MAX_FOLDER_CHARS || f.chars().any(char::is_control) {
        return Err(CmdError::Params(format!("`{key}` isn't a folder path")));
    }
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(std::path::PathBuf::from);
    let path = match (f.strip_prefix("~/"), home) {
        (Some(rest), Some(h)) => h.join(rest),
        _ => std::path::PathBuf::from(f),
    };
    if local_only && (super::templates::is_network_path(f) || super::templates::is_network_path(&path.to_string_lossy())) {
        return Err(CmdError::Params(format!("`{key}` can't be a network folder")));
    }
    if !path.is_absolute() {
        return Err(CmdError::Params(format!("`{key}`: {f:?} isn't a full path")));
    }
    // Its files are read from now on, and on every start: only from inside the read root (#3737).
    crate::file_access::check_read(&path.to_string_lossy()).map_err(CmdError::Failed)?;
    if !path.is_dir() {
        return Err(CmdError::Failed(format!("no folder at {}", path.display())));
    }
    Ok(Some(path.to_string_lossy().into_owned()))
}

/// `agentFolders`: each a full path to an existing folder on this computer (or `{path, write}`),
/// stored with its links followed; at most [`MAX_AGENT_FOLDERS`], each once. A folder the list
/// has already (`stored`) is kept as it is, without looking at it, so taking out a folder that
/// went, or changing another's `write`, never fails on it.
fn agent_folders(items: &[Value], stored: &[AgentFolder]) -> Result<Vec<AgentFolder>, CmdError> {
    if items.len() > MAX_AGENT_FOLDERS {
        return Err(CmdError::Params(format!("`agentFolders`: at most {MAX_AGENT_FOLDERS} folders")));
    }
    let mut folders: Vec<AgentFolder> = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let key = format!("agentFolders[{i}]");
        let (path, write) = match item {
            Value::String(path) => (path.as_str(), false),
            Value::Object(o) => {
                let path = o.get("path").and_then(Value::as_str).ok_or_else(|| CmdError::Params(format!("`{key}` needs a `path`")))?;
                let write = match o.get("write") {
                    None | Some(Value::Null) => false,
                    Some(Value::Bool(b)) => *b,
                    Some(_) => return Err(CmdError::Params(format!("`{key}.write` must be true or false"))),
                };
                (path, write)
            }
            _ => return Err(CmdError::Params(format!("`{key}` must be {{\"path\", \"write\"?}}"))),
        };
        let path = match stored.iter().find(|f| f.path == path.trim()) {
            Some(kept) => kept.path.clone(),
            None => agent_folder(&key, path, std::env::var_os("HOME").map(std::path::PathBuf::from))?,
        };
        if folders.iter().any(|f| f.path == path) {
            return Err(CmdError::Params(format!("`{key}`: {} is in the list already", echo(&path))));
        }
        folders.push(AgentFolder { path, write });
    }
    Ok(folders)
}

/// One of `agentFolders`: a full path (or one starting with `~/`, in the `home` folder) to an
/// existing folder on this computer, returned with its links followed, so `..` or a link can't
/// stand for another folder. A network folder is refused before anything looks at it (#2852).
pub(crate) fn agent_folder(key: &str, f: &str, home: Option<std::path::PathBuf>) -> Result<String, CmdError> {
    let f = f.trim();
    let home = home.filter(|h| !h.as_os_str().is_empty());
    let path = match (f.strip_prefix("~/"), home) {
        (Some(rest), Some(h)) => h.join(rest),
        _ => std::path::PathBuf::from(f),
    };
    // Judged as it will be used: `~/` is the home folder's full path by now (or still refused
    // as not a full one, with no home folder).
    let given = path.to_string_lossy().into_owned();
    if let Some(why) = crate::agent_files::unusable(&given) {
        return Err(CmdError::Params(format!("`{key}`: {:?} can't be a folder agents use: {why}", echo(f))));
    }
    let no_folder = || CmdError::Failed(format!("`{key}`: no folder at {}", echo(&given)));
    let found = crate::io::canonical(&path).filter(|p| p.is_dir()).ok_or_else(no_folder)?;
    let found = crate::file_access::plain(found).to_string_lossy().into_owned();
    // A drive mapped to another machine is only seen to be one once its links are followed.
    if let Some(why) = crate::agent_files::unusable(&found) {
        return Err(CmdError::Params(format!("`{key}`: {:?} can't be a folder agents use: {why}", echo(f))));
    }
    Ok(found)
}

/// At most the first 200 characters of `text`, for an error message: what was given can be
/// anything.
fn echo(text: &str) -> String {
    let mut shown: String = text.chars().take(200).collect();
    if shown.len() < text.len() {
        shown.push('…');
    }
    shown
}

/// Search `folder` for Hunspell dictionaries before the system locations, from now on (the saved
/// setting, applied when the app starts and when it changes).
pub fn use_dictionaries_folder(folder: Option<&str>) {
    wordcraft_proof::set_dictionaries_folder(folder.map(std::path::PathBuf::from));
}

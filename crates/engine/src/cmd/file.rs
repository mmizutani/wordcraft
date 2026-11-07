//! File tab (Backstage): new, open, save, export, print, properties; document inspection.

use serde_json::{Value, json};
use std::sync::Arc;
use wordcraft_doc::change::{FieldChange, PartChange};
use wordcraft_doc::{Block, ChangeSet, Document, StoryRef};

use super::{pos_json, sel_result};
use crate::{CmdError, CmdResult, CommandSpec, MAX_CHANGES, Session, p};

/// Longest user name kept, in characters; longer names are cut, not refused.
/// `format` of the save commands for an `.xml` file: Word 2003 XML Document (WordprocessingML 2003).
pub const FORMAT_WORD_2003: &str = "wordml2003";
/// `format` for an `.xml` file as a Word XML Document (Flat OPC), the default.
pub const FORMAT_FLAT_OPC: &str = "flatopc";

pub const MAX_AUTHOR_CHARS: usize = 255;

/// The user name as it is stored: trimmed, stripped of control characters (they can't go into
/// the saved XML) and cut to [`MAX_AUTHOR_CHARS`]; a blank name is refused so revisions always
/// carry an author.
pub fn normalize_author(name: &str) -> Result<String, CmdError> {
    let name: String = name.trim().chars().filter(|c| !c.is_control()).take(MAX_AUTHOR_CHARS).collect();
    let name = name.trim_end();
    if name.is_empty() {
        return Err(CmdError::Params("`name` must not be blank".into()));
    }
    Ok(name.to_string())
}

/// Sets the user name that new tracked changes and comments are recorded under. The name is
/// normalised by [`normalize_author`].
/// A protected document refuses it: the name is the identity its exceptions are checked against.
pub(crate) fn set_author(s: &mut Session, v: &Value) -> CmdResult {
    super::protect::check_user_name(s)?;
    s.author = normalize_author(p::req_str(v, "name")?)?;
    let mut r = sel_result(s)?;
    if let Some(o) = r.as_object_mut() {
        o.insert("name".into(), json!(s.author));
    }
    Ok(r)
}

/// Rename the current file in its directory, or name a document before its first save.
fn rename(s: &mut Session, v: &Value) -> CmdResult {
    let name = p::req_str(v, "name")?.trim();
    if name.is_empty()
        || name.chars().count() > 200
        || name.ends_with(['.', ' '])
        || name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        || matches!(name, "." | "..")
    {
        return Err(CmdError::Params("Enter a valid document name (up to 200 characters, without path separators).".into()));
    }
    if let Some(path) = &s.path {
        let mut destination = path.with_file_name(name);
        // The field edits the stem; retain the original format, including macro/template files.
        // A name that already ends in that extension is taken as the whole file name, since a
        // script or an agent passes one ("renamed.docx" became "renamed.docx.docx", #2983). The
        // extension alone (".docx") is a stem, not a file name, so it still gets one.
        if let Some(ext) = path.extension() {
            let ext = ext.to_string_lossy();
            // Split at the name's own last dot rather than counting bytes back from its end: a
            // name whose characters are more than one byte each ("résumé.docm") would be cut
            // inside one of them, and slicing there panics (#3128 review).
            let given = name.rsplit_once('.').is_some_and(|(stem, suffix)| !stem.is_empty() && suffix.eq_ignore_ascii_case(ext.as_ref()));
            destination.set_file_name(if given { name.to_string() } else { format!("{name}.{ext}") });
        }
        if destination != *path {
            #[cfg(not(target_arch = "wasm32"))]
            {
                crate::io::rename_file(path, &destination).map_err(CmdError::Failed)?;
            }
            s.path = Some(destination);
            s.relayout();
        }
    } else {
        s.file_name = Some(name.to_string());
        s.relayout();
    }
    Ok(json!({"name": name, "path": s.path}))
}

/// Builds the file and document command table: rename, new, open, save, save as and encryption,
/// PDF/PNG/print output and print settings, close, properties and info, author and privacy
/// settings, and document inspection and text access. Recovery, compatibility, sharing, autosave,
/// `file.newFromTemplate` and `file.saveTemplate` are registered in [`super::tools::specs`].
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("file.rename", "Rename Document", "File › Info", rename).params(r#"{"name": string (document name; its extension is kept, so the name may be given with or without it)}"#).pure(),
        CommandSpec::new("file.new", "New", "File", new).key("Mod+N").params(r#"{"template"?: "blank|sample|letter|resume|report"}"#).pure(),
        CommandSpec::new("file.open", "Open", "File", open)
            .key("Mod+O / Mod+F12 / Mod+Alt+F2")
            .params(r#"{"path": string, "data"?: base64 (the file's bytes; `path` then only names it), "password"?: string (for a password-protected Word document)}"#)
            .pure(),
        CommandSpec::new("file.save", "Save", "File", save)
            .key("Mod+S / Shift+F12 / Alt+Shift+F2")
            .params(r#"{"path"?: string, "password"?: string (encrypt this and later saves with it, as file.encrypt does), "markup"?: bool (Print Markup for a PDF, PNG, HTML, Markdown or text copy: comments shown; default file.printMarkup), "format"?: "wordml2003" (an .xml file as a Word 2003 XML Document) | "flatopc" (as a Word XML Document, the default; saving over a Word 2003 XML file keeps that format)}"#)
            .pure(),
        CommandSpec::new("file.saveAs", "Save As", "File", save_as).key(super::mac_or("F12 / Mod+Shift+S", "F12 / Alt+F2")).params(r#"{"path": string, "password"?: string, "markup"?: bool, "format"?: string (as file.save)}"#).pure(),
        // Pure: the password must not land in Repeat or a recorded macro, and it isn't an edit
        // to undo; it marks the document as changed itself.
        CommandSpec::new("file.encrypt", "Encrypt with Password", "File › Info › Protect Document", encrypt)
            .params(r#"{"password": string (Word documents are saved encrypted with it) | null or "" (saved unencrypted)}"#)
            .pure(),
        CommandSpec::new("file.exportPdf", "Export PDF", "File › Export", |s, v| {
            // No path (the command search, say): the app asks where to save, as File › Export
            // does, instead of failing on the required parameter (#2313).
            let Some(path) = p::str(v, "path") else {
                s.push_ui_request(json!({"open": "exportPdf"}));
                return Ok(json!({"exported": false}));
            };
            let path = if path.to_ascii_lowercase().ends_with(".pdf") { path.to_string() } else { format!("{path}.pdf") };
            s.exports_to_new_file(&path)?;
            let opts = export_options(s, v);
            crate::io::save_path_with(std::path::Path::new(&path), &s.doc, None, &opts).map_err(CmdError::Failed)?;
            Ok(json!({"path": path, "markup": opts.markup}))
        })
        .params(r#"{"path": string, "markup"?: bool (Print Markup: tracked changes marked up and each page scaled down beside the comments in balloons; default file.printMarkup)}"#)
        .pure(),
        CommandSpec::new("file.exportPng", "Export Page as PNG", "File › Export", export_png)
            .params(r#"{"path": string, "page"?: n (1-based), "scale"?: px per pt, "markup"?: bool (the page with its comments in balloons beside it; default file.printMarkup)}"#)
            .pure(),
        // A print setting, like Word's: not part of the document, so no undo step.
        CommandSpec::new("file.printMarkup", "Print Markup", "File › Print", |s, v| {
            s.prefs.print_markup = p::bool(v, "value").unwrap_or(!s.prefs.print_markup);
            Ok(json!({"value": s.prefs.print_markup}))
        })
        .params(r#"{"value"?: bool (omit to toggle)} — PDFs, prints and copies show comments (balloons beside each page; a list at the end of HTML, Markdown and text) and tracked changes"#)
        .pure(),
        CommandSpec::new("file.printPdf", "Print to PDF", "File › Print", crate::print::print_pdf_to_file)
            .params(
                r#"{"path": string, "pagesPerSheet"?: 1 | 2 | 4 | 6 | 9 | 16 | "booklet" | "bookletReverse", "range"?: "all" | "current" | "selection" (the pages the selection touches) | "custom", "pages"?: "1-3,5" (implies custom), "parity"?: "all" | "odd" | "even", "copies"?: 1-999, "collate"?: bool (default true), "paper"?: "none" | "Letter" | "Legal" | "A4" | … (a Layout › Size name: scale every page to this paper, centred; the document keeps its size)}"#,
            )
            .pure(),
        CommandSpec::new("file.print", "Print", "File", |s, _| {
            s.push_ui_request(json!({"open": "print"}));
            sel_result(s)
        })
        .key("Mod+P / Mod+Shift+F12 / Mod+F2 / Mod+Alt+I")
        .pure(),
        CommandSpec::new("file.close", "Close", "File", |s, _| {
            s.push_ui_request(json!({"close": true}));
            sel_result(s)
        })
        .key("Mod+W / Mod+F4")
        .params(r#"{"force"?: bool (desktop app's control channel and MCP bridge only: without it a document with unsaved changes is an error and stays open; with it the changes are discarded. Headless sessions never ask; file.closeDocument closes a document there)}"#)
        .pure(),
        CommandSpec::new("file.closeDocument", "Close Document", "Agents", close_document)
            .params(r#"{"id"?: n (an open document, see view.switchWindows; default the active one), "force"?: bool (discard unsaved changes; without it a document with unsaved changes is an error and stays open)} → {"closed": id, "active": id, "documents": [{"id", "title", "path", "dirty", "active"}]}; closing the last document leaves a new blank one; a shared document (room.share) is refused until sharing stops, which the app's own close does first"#)
            .pure(),
        CommandSpec::new("file.properties", "Properties", "File › Info", properties)
            .params(r#"{"title"?, "subject"?, "author"?, "keywords"?, "comments"?, "category"?, "custom"?: {name: string | {"value": string, "linkTarget"?: string | null} | null (removes)}}; text values and bookmark targets are at most 1 MiB; a string keeps an existing target, an object replaces it (omitted/null target unlinks)"#),
        CommandSpec::new("file.info", "Info", "File", info).pure(),
        CommandSpec::new("file.setAuthor", "User Name", "File › Options › General", set_author)
            .params(r#"{"name": string (author of new tracked changes and comments)}"#)
            .pure(),
        // Trust Center › Privacy Options: the document's `w:removePersonalInformation` and
        // `w:removeDateAndTime` settings, honoured when it is saved as .docx (#1062).
        // Changing them is a document change (undoable, refused on a protected document); reading
        // them is `file.privacy`, which changes nothing.
        CommandSpec::new("file.privacyOptions", "Privacy Options", "File › Options › Save", |s, v| {
            let personal = p::bool(v, "removePersonalInfo");
            let date_time = p::bool(v, "removeDateTime");
            if personal.is_none() && date_time.is_none() {
                return Err(CmdError::Params("give removePersonalInfo or removeDateTime (file.privacy reads them)".into()));
            }
            if let Some(on) = personal {
                s.doc.settings.remove_personal_info = on;
            }
            if let Some(on) = date_time {
                s.doc.settings.remove_date_time = on;
            }
            Ok(privacy(s))
        })
        .params(r#"{"removePersonalInfo"?: bool (save comment and revision authors as "Author", no creator or last-modified-by), "removeDateTime"?: bool (save comments and tracked changes without dates)} — at least one; the open document keeps its real names and dates"#),
        CommandSpec::new("file.privacy", "Privacy Settings", "File › Options › Save", |s, _| Ok(privacy(s)))
            .params(r#"{} → {"removePersonalInfo": bool, "removeDateTime": bool} (file.privacyOptions changes them)"#)
            .pure(),
        CommandSpec::new("document.inspect", "Inspect Document", "Agents", inspect)
            .params(r#"{"text"?: bool, "maxInputBytes"?: number (refuse oversized read inputs before copying them)}"#).pure(),
        CommandSpec::new("document.changes", "Document Changes", "Agents", changes)
            .params(
                r#"{"maxInputBytes"?: number (refuse oversized read inputs before copying them), "since"?: number (a version of this document, as document.inspect or an earlier call gave it; the current one when left out), "limit"?: number (most changes to give, oldest first: 100 when left out, at most 1024), "full"?: bool (each change whole, as another copy of the document applies it)} → {"document": id, "version": number (the current one), "changes": [{"version", "edits": [{"in", "after"?, "before"?, "removed": [block id], "inserted": [block id], "at", "deleted", "text"}], "tables"?: [table id], "parts"?: [{"added"|"removed"|"changed": id}], "fields"?: [name], "change"?: ChangeSet}], "more"?: true (ask again from the last version given)}; an error when `since` is ahead of the document or older than the changes it keeps (read the document again)"#,
            )
            .pure(),
        CommandSpec::new("document.text", "Document Text", "Agents", |s, v| {
            let story = super::story_param(s, v);
            check_read_size(v, &s.doc.story(story))?;
            Ok(json!({"text": s.doc.final_text(story)}))
        })
        .params(r#"{"story"?: Story, "maxInputBytes"?: number (refuse oversized read inputs before copying them)}"#)
        .pure(),
        CommandSpec::new("document.paragraph", "Paragraph Details", "Agents", paragraph)
            .params(r#"{"path": [n], "story"?: Story, "maxInputBytes"?: number (refuse oversized read inputs before copying them)}"#).pure(),
        CommandSpec::new("document.selection", "Selection", "Agents", |s, v| {
            check_selection_size(s, v)?;
            let mut r = sel_result(s)?;
            if let Some(o) = r.as_object_mut() {
                o.insert("text".into(), json!(s.selected_text()));
                o.insert("format".into(), super::format::state(s));
            }
            Ok(r)
        })
        .params(r#"{"maxInputBytes"?: number (refuse oversized read inputs before copying them)}"#)
        .pure(),
        CommandSpec::new("document.layout", "Layout Summary", "Agents", layout_summary).pure(),
        CommandSpec::new("document.setText", "Replace Document Text", "Agents", |s, v| {
            let text = p::req_str(v, "text")?;
            let d = Document::from_text(text);
            s.doc.body = d.body;
            // The body is new even though the document id stays: a live IME composition (or an
            // equation editing session at the same offset) belongs to the old content.
            s.note_body_replaced();
            s.math = None;
            s.sel = crate::Selection::caret(s.doc.start_of(StoryRef::Body));
            sel_result(s)
        })
        .params(r#"{"text": string}"#),
    ]
}

/// The document's privacy settings, as `file.privacy` and `file.privacyOptions` return them.
fn privacy(s: &Session) -> Value {
    json!({"removePersonalInfo": s.doc.settings.remove_personal_info, "removeDateTime": s.doc.settings.remove_date_time})
}

/// Open a new document beside the open ones and make it active: a blank one in the saved
/// proofing language, or the requested template.
fn new(s: &mut Session, v: &Value) -> CmdResult {
    let doc = match p::str(v, "template").unwrap_or("blank") {
        "sample" => crate::sample::sample_document(),
        "letter" => crate::sample::letter(),
        "resume" => crate::sample::resume(),
        "report" => crate::sample::report(),
        _ => normal_document(s),
    };
    s.add_document(doc);
    sel_result(s)
}

/// A blank document on the Normal template: the region's paper (#838), written in the editing
/// language (the templates' text is English).
pub(crate) fn normal_document(s: &Session) -> Document {
    let mut d = s.blank_document();
    if let Some(lang) = s.prefs.default_language.as_ref().or(s.editing_lang.as_ref()) {
        Arc::make_mut(&mut d.styles).default_chr.lang = Some(lang.clone());
    }
    d
}

/// Open a document, using the saved proofing default only for plain text and Markdown. A file
/// that is already open becomes the active document instead (`"alreadyOpen": true`). The file
/// replaces an untouched blank document; otherwise it opens beside the open documents.
fn open(s: &mut Session, v: &Value) -> CmdResult {
    let Some(path) = p::str(v, "path") else {
        s.push_ui_request(json!({"open": "openFile"}));
        return sel_result(s);
    };
    if p::str(v, "data").is_none()
        && let Some(id) = s.find_open(std::path::Path::new(path))
    {
        s.activate(id)?;
        return Ok(
            json!({"path": s.path, "paragraphs": s.doc.paragraph_count(), "words": s.doc.word_count(), "encrypted": s.password.is_some(), "alreadyOpen": true, "id": id}),
        );
    }
    let password = p::str(v, "password");
    let (mut doc, encrypted) = if let Some(data) = p::str(v, "data") {
        let bytes = super::insert::base64_decode(data).ok_or_else(|| CmdError::Params("bad base64".into()))?;
        // An empty file (File Explorer's New › Microsoft Word Document makes one) opens as a new
        // blank document that saves back to it (#1384).
        if bytes.is_empty() {
            (normal_document(s), false)
        } else {
            crate::io::open_bytes_in(path, &bytes, password, &s.region()).map_err(CmdError::Failed)?
        }
    } else if crate::io::is_empty_file(std::path::Path::new(path)) {
        (normal_document(s), false)
    } else {
        crate::io::open_path_in(std::path::Path::new(path), password, &s.region()).map_err(CmdError::Failed)?
    };
    // Plain text and Markdown carry no language: they are in the editing language.
    let ext = std::path::Path::new(path).extension().and_then(std::ffi::OsStr::to_str).unwrap_or("").to_ascii_lowercase();
    if let Some(lang) = s.prefs.default_language.as_ref().or(s.editing_lang.as_ref())
        && matches!(ext.as_str(), "txt" | "text" | "md" | "markdown")
    {
        Arc::make_mut(&mut doc.styles).default_chr.lang = Some(lang.clone());
    }
    match s.used_because() {
        None => s.set_document(doc),
        Some(why) => {
            log::info!("file.open: opened beside document {}, not in its place: {why}", s.document_id());
            s.add_opened_document(doc);
        }
    }
    if let Some(z) = s.doc.settings.zoom {
        super::view::restore_zoom(s, z);
    }
    s.path = Some(path.into());
    // Like Word, a document opened with a password is saved with it again.
    if encrypted {
        s.password = password.map(crate::io::Password::new);
    }
    Ok(
        json!({"path": path, "paragraphs": s.doc.paragraph_count(), "words": s.doc.word_count(), "encrypted": encrypted, "alreadyOpen": false, "id": s.document_id()}),
    )
}

/// Close a document with no window to ask about it (agents, headless sessions).
fn close_document(s: &mut Session, v: &Value) -> CmdResult {
    let id = match v.get("id").filter(|x| !x.is_null()) {
        None => s.document_id(),
        Some(x) => x.as_u64().ok_or_else(|| CmdError::Params("`id` must be an open document's id".into()))?,
    };
    if !s.is_open(id) {
        return Err(CmdError::Params(format!("no open document has id {id}")));
    }
    let dirty = s.documents().iter().any(|d| d.id == id && d.dirty);
    if dirty && p::bool(v, "force") != Some(true) {
        return Err(CmdError::Failed("the document has unsaved changes: save it first, or pass \"force\": true to discard them".into()));
    }
    // What the room takes after this would be lost with it: the window stops sharing first when
    // it closes the document itself, so this is a step of a macro, say.
    if s.shared_room().is_some_and(|(shared, _)| shared == id) {
        return Err(CmdError::Failed("the document is shared: stop sharing it first (room.stop), or close it in the window".into()));
    }
    s.close_document(id)?;
    Ok(json!({"closed": id, "active": s.document_id(), "documents": s.documents()}))
}

/// File › Info › Protect Document › Encrypt with Password: the password Word documents are saved
/// with from now on, or none.
fn encrypt(s: &mut Session, v: &Value) -> CmdResult {
    let password = match v.get("password") {
        Some(Value::Null) => None,
        Some(Value::String(pw)) if pw.is_empty() => None,
        Some(Value::String(pw)) => {
            wordcraft_docx::check_password(pw).map_err(|e| CmdError::Params(e.to_string()))?;
            Some(crate::io::Password::new(pw))
        }
        _ => return Err(CmdError::Params("`password` must be a string, or null to remove the password".into())),
    };
    if password != s.password {
        if password.is_some() {
            s.clear_recovery_before_password()?;
        }
        s.password = password;
        s.dirty = true;
    }
    Ok(json!({"encrypted": s.password.is_some()}))
}

fn save(s: &mut Session, v: &Value) -> CmdResult {
    let path = match p::str(v, "path").map(std::path::PathBuf::from).or_else(|| s.path.clone()) {
        Some(p) => p,
        None => {
            s.push_ui_request(json!({"open": "saveAs"}));
            return Ok(json!({"saved": false}));
        }
    };
    if let Some(pw) = v.get("password").filter(|pw| !pw.is_null()) {
        let Some(pw) = pw.as_str().filter(|pw| !pw.is_empty()) else {
            return Err(CmdError::Params("`password` must be a non-empty string (file.encrypt removes a password)".into()));
        };
        if !crate::io::can_encrypt(&path.to_string_lossy()) {
            return Err(CmdError::Params(
                "only Word documents (.docx, .docm, .dotx, .dotm) and OpenDocument text files (.odt, .ott) can be saved with a password".into(),
            ));
        }
        wordcraft_docx::check_password(pw).map_err(|e| CmdError::Params(e.to_string()))?;
        s.clear_recovery_before_password()?;
        s.password = Some(crate::io::Password::new(pw));
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let mut opts = export_options(s, v);
    match v.get("format").filter(|f| !f.is_null()) {
        None => {
            // Saving over a Word 2003 XML file (one opened as such, say) keeps its format.
            opts.word2003 = ext == "xml" && is_word_2003_file(&path);
        }
        Some(f) if matches!(f.as_str(), Some(FORMAT_WORD_2003 | FORMAT_FLAT_OPC)) && ext == "xml" => {}
        Some(_) => {
            return Err(CmdError::Params(format!("`format` is \"{FORMAT_WORD_2003}\" or \"{FORMAT_FLAT_OPC}\", and only for an .xml file")));
        }
    }
    // Undone below if the write fails: a failed save (no permission, disk full, a bad path)
    // must not leave the document's own save stamps — zoom, modified/created, author, revision —
    // changed, or a later save would persist them for a write that never actually happened.
    let before_stamp =
        (s.doc.settings.zoom, s.doc.core.last_modified_by.clone(), s.doc.core.created.clone(), s.doc.core.modified.clone(), s.doc.core.revision);
    s.stamp_save();
    let password = s.password.as_ref().map(crate::io::Password::as_str);
    // A header/footer just made and left empty isn't saved (#960).
    let copy = super::insert::saved_copy(s);
    if let Err(e) = crate::io::save_path_with(&path, copy.as_ref().unwrap_or(&s.doc), password, &opts) {
        (s.doc.settings.zoom, s.doc.core.last_modified_by, s.doc.core.created, s.doc.core.modified, s.doc.core.revision) = before_stamp;
        return Err(CmdError::Failed(e));
    }
    let encrypted = password.is_some() && crate::io::can_encrypt(&path.to_string_lossy());
    // A save in a format that doesn't keep everything (plain text, Markdown, a page image) is a
    // copy, except over the document's own file: a note.txt opened and saved is saved (#2376).
    let own_file = s.path.as_deref() == Some(path.as_path());
    if own_file || ["docx", "docm", "dotx", "dotm", "odt", "ott", "fodt", "rtf", "json", "xml"].contains(&ext.as_str()) {
        s.path = Some(path.clone());
        s.dirty = false;
        s.relayout();
    }
    Ok(json!({"saved": true, "path": path.to_string_lossy(), "encrypted": encrypted}))
}

/// Does `path` hold a Word 2003 XML Document already (so a plain save keeps that format)?
#[cfg(not(target_arch = "wasm32"))]
fn is_word_2003_file(path: &std::path::Path) -> bool {
    // The root element and its namespaces come first.
    let Ok(head) = crate::io::read_file_head(&path.to_string_lossy(), 64 * 1024) else { return false };
    wordcraft_docx::sniff_xml(&head) == wordcraft_docx::XmlKind::WordMl2003
}

#[cfg(target_arch = "wasm32")]
fn is_word_2003_file(_path: &std::path::Path) -> bool {
    false
}

fn save_as(s: &mut Session, v: &Value) -> CmdResult {
    if p::str(v, "path").is_none() {
        s.push_ui_request(json!({"open": "saveAs"}));
        return Ok(json!({"saved": false}));
    }
    save(s, v)
}

/// How a copy is written: Print Markup from the command's `markup`, else the File › Print
/// setting (`file.printMarkup`); comments as Track Changes Options show them; digits as the
/// Numeral setting shows them on screen.
pub fn export_options(s: &Session, v: &Value) -> crate::io::ExportOptions {
    let markup = p::bool(v, "markup").unwrap_or(s.prefs.print_markup);
    crate::io::ExportOptions {
        markup,
        comments: markup && s.prefs.markup.comments,
        word2003: p::str(v, "format") == Some(FORMAT_WORD_2003),
        numeral: s.view.numeral.resolve(crate::session::system_locale()),
    }
}

fn export_png(s: &mut Session, v: &Value) -> CmdResult {
    // No path: the app asks where to save, as file.exportPdf does (#2313).
    let Some(path) = p::str(v, "path") else {
        s.push_ui_request(json!({"open": "exportPng"}));
        return Ok(json!({"exported": false}));
    };
    let page = crate::p::page_index(p::u64(v, "page").unwrap_or(1));
    let scale = p::f32(v, "scale").unwrap_or(2.0).clamp(0.1, 8.0);
    let l = s.export_layout_with(export_options(s, v).markup);
    let pg = l.pages.get(page).ok_or_else(|| CmdError::Params(format!("no page {}", page + 1)))?;
    // A page image is the printed page: headers and footers at full strength. Dimming them is
    // the on-screen cue while editing the body, not part of the page.
    let mut opts = wordcraft_render::RenderOptions::default();
    opts.display.dim_header = false;
    if !path.to_ascii_lowercase().ends_with(".png") {
        return Err(CmdError::Params("path must end in .png".into()));
    }
    s.exports_to_new_file(path)?;
    let img = wordcraft_render::render_page(&s.doc, pg, scale, &opts);
    let png = img.to_png();
    crate::io::write_file(path, &png).map_err(CmdError::Failed)?;
    Ok(json!({"path": path, "width": img.width, "height": img.height, "bytes": png.len()}))
}

/// Text and its bookmark association cross the dialog/command boundary together on rename.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LinkedCustomText<'a> {
    value: &'a str,
    #[serde(borrow)]
    link_target: Option<&'a str>,
}

fn properties(s: &mut Session, v: &Value) -> CmdResult {
    let c = &mut s.doc.core;
    let set = |dst: &mut String, k: &str| {
        if let Some(x) = p::str(v, k) {
            *dst = x.to_string();
        }
    };
    set(&mut c.title, "title");
    set(&mut c.subject, "subject");
    set(&mut c.creator, "author");
    set(&mut c.keywords, "keywords");
    set(&mut c.description, "comments");
    set(&mut c.category, "category");
    if let Some(custom) = v.get("custom") {
        let Some(m) = custom.as_object() else {
            return Err(CmdError::Params("`custom` must be an object of name → text, linked text value, or null".into()));
        };
        // Removals first: a case-only rename sends the old spelling as null and the new one
        // with its value, and `set_custom_prop` matches names case-insensitively, so setting
        // first would keep the old spelling (object keys are sorted, either spelling may lead).
        for (name, val) in m {
            if val.is_null() {
                s.doc.remove_custom_prop(name);
            }
        }
        for (name, val) in m {
            let bad_value =
                || CmdError::Params(format!("custom property `{name}` must be text (≤ {MAX_CUSTOM_VALUE} bytes), a linked text value, or null"));
            let (value, target) = match val {
                // Removal also accepts names longer than today's limit (#2982).
                Value::Null => continue,
                Value::String(value) => (value.as_str(), None),
                Value::Object(_) => {
                    let linked: LinkedCustomText<'_> = serde::Deserialize::deserialize(val).map_err(|_| bad_value())?;
                    (linked.value, Some(linked.link_target))
                }
                _ => return Err(bad_value()),
            };
            if value.len() > MAX_CUSTOM_VALUE {
                return Err(bad_value());
            }
            if name.trim().is_empty() || name.chars().count() > MAX_CUSTOM_NAME {
                return Err(CmdError::Params(format!("custom property names are 1–{MAX_CUSTOM_NAME} characters")));
            }
            if target.flatten().is_some_and(|t| t.len() > MAX_CUSTOM_VALUE) {
                return Err(CmdError::Params(format!("custom property bookmark targets are at most {MAX_CUSTOM_VALUE} bytes")));
            }
            s.doc.set_custom_prop(name, value);
            if let Some(target) = target
                && let Some(prop) = s.doc.custom_props.iter_mut().find(|p| p.name.eq_ignore_ascii_case(name))
            {
                prop.link_target = target.map(str::to_string);
            }
        }
    }
    let mut out = serde_json::to_value(&s.doc.core).unwrap_or(Value::Null);
    if let Some(o) = out.as_object_mut() {
        o.insert("custom".into(), serde_json::to_value(&s.doc.custom_props).unwrap_or(Value::Null));
    }
    Ok(out)
}

/// Longest custom property name (Word's limit). The Custom Properties dialog validates with it
/// too (#2982).
pub const MAX_CUSTOM_NAME: usize = 255;
/// Largest custom property value or bookmark target we accept from a command. The Custom Properties dialog
/// validates with it too (#2982).
pub const MAX_CUSTOM_VALUE: usize = 1024 * 1024;

fn info(s: &mut Session, _: &Value) -> CmdResult {
    let l = s.layout();
    Ok(json!({
        "path": s.path.as_ref().map(|p| p.to_string_lossy().to_string()),
        "dirty": s.dirty,
        "pages": l.pages.len(),
        "words": s.doc.word_count(),
        "paragraphs": s.doc.paragraph_count(),
        "properties": s.doc.core,
        "trackChanges": s.doc.settings.track_changes,
        "comments": s.doc.comments.len(),
        "sections": s.doc.sections().len(),
        "encrypted": s.password.is_some(),
        "layoutMs": l.ms,
    }))
}

/// Structure for agents: blocks with text and formatting summary, selection, stats.
fn inspect(s: &mut Session, v: &Value) -> CmdResult {
    check_read_size(v, &(&s.doc.body, &s.doc.parts, &s.doc.core, &s.doc.last_section))?;
    let with_text = p::bool(v, "text").unwrap_or(true);
    let mut blocks = Vec::new();
    for (i, b) in s.doc.body.iter().enumerate().take(2000) {
        match &**b {
            Block::Para(p) => {
                let style = p.props.style.clone().unwrap_or_else(|| "Normal".into());
                let mut o = json!({"index": i, "id": p.id, "type": "paragraph", "style": style, "len": p.len()});
                if with_text {
                    o["text"] = json!(p.plain_text());
                }
                if let Some(n) = p.props.numbering.filter(|n| n.num != 0) {
                    o["list"] = json!({"num": n.num, "level": n.level});
                }
                if !p.props.is_empty() {
                    o["props"] = serde_json::to_value(&p.props).unwrap_or(Value::Null);
                }
                let runs: Vec<Value> = p
                    .run_ranges()
                    .filter(|(_, c)| !c.is_empty())
                    .map(|(r, c)| json!({"start": r.start, "end": r.end, "props": c}))
                    .take(50)
                    .collect();
                if !runs.is_empty() {
                    o["runs"] = json!(runs);
                }
                if !p.objects.is_empty() {
                    o["objects"] = serde_json::to_value(&p.objects).unwrap_or(Value::Null);
                }
                if p.section.is_some() {
                    o["sectionBreak"] = json!(true);
                }
                blocks.push(o);
            }
            Block::Table(t) => {
                let cells: Vec<Vec<String>> = t
                    .rows
                    .iter()
                    .map(|r| {
                        r.cells
                            .iter()
                            .map(|c| c.blocks.iter().filter_map(|b| b.as_para().map(|p| p.plain_text())).collect::<Vec<_>>().join("\n"))
                            .collect()
                    })
                    .collect();
                blocks.push(
                    json!({"index": i, "id": t.id, "type": "table", "rows": t.rows.len(), "cols": t.cols(), "style": t.props.style, "cells": cells}),
                );
            }
        }
    }
    let l = s.layout();
    let parts: Vec<Value> =
        s.doc.parts.iter().map(|(id, p)| json!({"id": id, "kind": p.kind, "text": s.doc.plain_text(StoryRef::Part(*id))})).collect();
    Ok(json!({
        "blocks": blocks,
        "parts": parts,
        "selection": {"anchor": pos_json(&s.sel.anchor), "focus": pos_json(&s.sel.focus), "text": s.selected_text()},
        "pages": l.pages.len(),
        "words": s.doc.word_count(),
        "sections": s.doc.sections().iter().map(|(end, sp)| json!({"endBlock": end, "props": sp})).collect::<Vec<_>>(),
        "comments": s.doc.comments.len(),
        "trackChanges": s.doc.settings.track_changes,
        "properties": s.doc.core,
        "canUndo": s.can_undo(),
        "canRedo": s.can_redo(),
        "version": s.changes().version(),
    }))
}

/// `document.changes`: the changes the document had after version `since`, oldest first, each
/// told in short (see [`change_summary`]) and, with `full`, whole.
fn changes(s: &mut Session, v: &Value) -> CmdResult {
    let log = s.changes();
    let whole = |k: &str| match v.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(n) => n.as_u64().map(Some).ok_or_else(|| CmdError::Params(format!("`{k}` is a whole number from 0"))),
    };
    let since = whole("since")?.unwrap_or(log.version());
    let limit = whole("limit")?.map_or(100, |n| usize::try_from(n).unwrap_or(usize::MAX)).clamp(1, MAX_CHANGES);
    let full = p::bool(v, "full").unwrap_or(false);
    check_changes_size(v, log, since, limit, full)?;
    let after = log.since(since).map_err(CmdError::Params)?;
    let mut out = Vec::with_capacity(after.len().min(limit));
    for (version, cs) in after.iter().take(limit) {
        let mut o = change_summary(cs);
        o["version"] = json!(version);
        if full {
            o["change"] = serde_json::to_value(&**cs).map_err(|e| CmdError::Failed(format!("change {version} can't be written: {e}")))?;
        }
        out.push(o);
    }
    let mut r = json!({"document": s.document_id(), "version": log.version(), "changes": out});
    if after.len() > limit {
        r["more"] = json!(true);
    }
    Ok(r)
}

/// Change `cs` in short. First, for each paragraph edited in place, where it is (`in`: a story,
/// or a cell of a table), its id (`paragraph`) and the characters replaced (byte offsets in its
/// text): `deleted` bytes from `at`, `text` put there (U+FFFC for an object); `properties` when
/// its own properties changed. Then, for each list of blocks it edited, where the list is, the
/// blocks around the edit, the ids of the blocks it took away and of those it put in, and the
/// text splice on their text (U+2029 between paragraphs, U+FFFC for a table). Then the tables
/// whose properties changed, the parts (notes, comments, text boxes, headers…) added, removed or
/// changed, and the names of the other things that changed.
fn change_summary(cs: &ChangeSet) -> Value {
    let in_place = cs.edits.iter().map(|e| {
        let mut o = json!({"in": e.container, "paragraph": e.id});
        if let Some(c) = &e.chars {
            o["at"] = json!(c.at);
            o["deleted"] = json!(c.removed);
            o["text"] = json!(c.inserted.text);
        }
        if e.shell.is_some() {
            o["properties"] = json!(true);
        }
        o
    });
    let edits: Vec<Value> = in_place
        .chain(cs.splices.iter().map(|sp| {
            let mut o = json!({
                "in": sp.container,
                "removed": sp.removed.iter().map(|b| b.id).collect::<Vec<_>>(),
                "inserted": sp.inserted.iter().map(|b| b.0.id()).collect::<Vec<_>>(),
                "at": sp.text.at,
                "deleted": sp.text.removed,
                "text": sp.text.inserted,
            });
            if let Some(a) = sp.after {
                o["after"] = json!(a);
            }
            if let Some(b) = sp.before {
                o["before"] = json!(b);
            }
            o
        }))
        .collect();
    let mut o = json!({"edits": edits});
    if !cs.tables.is_empty() {
        o["tables"] = json!(cs.tables.iter().map(|t| t.id).collect::<Vec<_>>());
    }
    if !cs.parts.is_empty() {
        let parts: Vec<Value> = cs
            .parts
            .iter()
            .map(|p| match p {
                PartChange::Added { id, .. } => json!({"added": id}),
                PartChange::Removed { id, .. } => json!({"removed": id}),
                PartChange::Changed { id, .. } => json!({"changed": id}),
            })
            .collect();
        o["parts"] = json!(parts);
    }
    if !cs.fields.is_empty() {
        let fields: Vec<String> = cs
            .fields
            .iter()
            .map(|f| match f {
                FieldChange::LastSection(_) => "lastSection".into(),
                FieldChange::Styles(_) => "styles".into(),
                FieldChange::Numbering(_) => "numbering".into(),
                FieldChange::Comment { id, .. } => format!("comment {id}"),
                FieldChange::Revisions { .. } => "revisions".into(),
                FieldChange::Settings(_) => "settings".into(),
                FieldChange::Core(_) => "core".into(),
                FieldChange::Sources(_) => "sources".into(),
                FieldChange::CustomProps(_) => "customProps".into(),
                FieldChange::FootnoteSeparators(_) => "footnoteSeparators".into(),
                FieldChange::EndnoteSeparators(_) => "endnoteSeparators".into(),
                FieldChange::Media { key, .. } => format!("media {key}"),
                FieldChange::Passthrough { key, .. } => format!("passthrough {key}"),
                FieldChange::Glossary(_) => "glossary".into(),
            })
            .collect();
        o["fields"] = json!(fields);
    }
    o
}

fn paragraph(s: &mut Session, v: &Value) -> CmdResult {
    let path: Vec<u32> = serde_json::from_value(v.get("path").cloned().unwrap_or(json!([0]))).map_err(|e| CmdError::Params(e.to_string()))?;
    let story = super::story_param(s, v);
    let p = s.doc.para(story, &wordcraft_doc::Path(path)).ok_or_else(|| CmdError::Params("no paragraph there".into()))?;
    check_read_size(v, p)?;
    let resolved = s.doc.styles.resolve_para(&p.props);
    Ok(json!({"paragraph": p, "resolved": resolved}))
}

fn layout_summary(s: &mut Session, _: &Value) -> CmdResult {
    let l = s.layout();
    let pages: Vec<Value> = l
        .pages
        .iter()
        .map(|p| {
            let lines: usize = p.items.iter().map(|it| if let wordcraft_layout::Placed::Lines { l0, l1, .. } = it { l1 - l0 } else { 0 }).sum();
            json!({"number": p.number, "w": p.w, "h": p.h, "section": p.section, "lines": lines, "firstBlock": p.first_block, "header": p.header_story, "footer": p.footer_story})
        })
        .collect();
    let caret = l.caret_on(&s.sel.focus, s.page_hint).map(|c| json!({"page": c.page, "x": c.x, "y": c.top, "h": c.height}));
    Ok(json!({"pages": pages, "caret": caret, "ms": l.ms}))
}

/// Refuse a bounded read before building its result, on the same synchronized session.
fn check_read_size(params: &Value, input: &impl serde::Serialize) -> Result<(), CmdError> {
    match ReadBudget::from_params(params)? {
        Some(mut budget) => budget.check(input),
        None => Ok(()),
    }
}

/// Count the blocks a fragment copies, including the text-box stories it follows.
fn check_selection_size(s: &Session, params: &Value) -> Result<(), CmdError> {
    let Some(mut budget) = ReadBudget::from_params(params)? else { return Ok(()) };
    if s.sel.is_collapsed() && s.column.is_none() {
        return Ok(());
    }
    let (a, b) = s.sel.ordered();
    let Some(story) = s.doc.story(a.story).filter(|_| a.story == b.story) else { return Ok(()) };
    let same_container = a.path.parent() == b.path.parent();
    let source = if same_container { s.doc.container(a.story, &a.path).unwrap_or(story) } else { story };
    let (from, to) = if same_container {
        (a.path.last(), b.path.last().saturating_add(1))
    } else {
        (a.path.0.first().copied().unwrap_or(0) as usize, (b.path.0.first().copied().unwrap_or(0) as usize).saturating_add(1))
    };
    // Copying a slice clones its whole edge paragraphs before trimming them.
    let blocks = if s.column.is_some() { story.as_slice() } else { source.get(from..to.min(source.len())).unwrap_or_default() };
    budget.check(&blocks)?;
    let mut boxes = Vec::new();
    box_references(blocks, &mut boxes, 0);
    let mut seen = std::collections::BTreeSet::new();
    while let Some(id) = boxes.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let Some(part) = s.doc.parts.get(&id).filter(|p| p.kind == wordcraft_doc::PartKind::TextBox) {
            budget.check(&(id, part))?;
            box_references(&part.blocks, &mut boxes, 0);
        }
    }
    Ok(())
}

fn box_references(blocks: &[Arc<Block>], out: &mut Vec<u32>, depth: usize) {
    if depth > 16 {
        return;
    }
    for block in blocks {
        match &**block {
            Block::Para(p) => out.extend(p.objects.iter().flat_map(wordcraft_doc::InlineObject::text_boxes)),
            Block::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        box_references(&cell.blocks, out, depth + 1);
                    }
                }
            }
        }
    }
}

/// Count borrowed entries before `since` clones them and summaries copy their text.
fn check_changes_size(params: &Value, log: &crate::ChangeLog, since: u64, limit: usize, full: bool) -> Result<(), CmdError> {
    let Some(mut budget) = ReadBudget::from_params(params)? else { return Ok(()) };
    for (version, cs) in log.borrowed_since(since).map_err(CmdError::Params)?.take(limit) {
        budget.check(version)?;
        if full {
            // NewBlock collects descendant ids: bound its blocks before invoking that serializer.
            for splice in &cs.splices {
                for block in &splice.inserted {
                    budget.check(&block.0)?;
                }
            }
            for part in &cs.parts {
                if let PartChange::Added { blocks, .. } = part {
                    for block in blocks {
                        budget.check(&block.0)?;
                    }
                }
            }
            budget.check(&**cs)?;
            continue;
        }
        for edit in &cs.edits {
            budget.check(&(&edit.container, edit.id, edit.chars.as_ref().map(|c| (c.at, c.removed, &c.inserted.text))))?;
        }
        for splice in &cs.splices {
            budget.check(&(&splice.container, &splice.text, splice.after, splice.before))?;
            for block in &splice.removed {
                budget.check(&block.id)?;
            }
            for block in &splice.inserted {
                budget.check(&block.0.id())?;
            }
        }
        for table in &cs.tables {
            budget.check(&table.id)?;
        }
        for part in &cs.parts {
            let (PartChange::Added { id, .. } | PartChange::Removed { id, .. } | PartChange::Changed { id, .. }) = part;
            budget.check(&("changed", id))?;
        }
        for field in &cs.fields {
            match field {
                FieldChange::Media { key, .. } | FieldChange::Passthrough { key, .. } => budget.check(&("passthrough", key))?,
                // Summaries keep only fixed field names, never their payloads.
                _ => budget.check(&"footnoteSeparators")?,
            }
        }
    }
    Ok(())
}

/// Count borrowed serialized input without keeping bytes; stop at the first oversized write.
struct ReadBudget {
    left: usize,
}

impl ReadBudget {
    fn from_params(params: &Value) -> Result<Option<Self>, CmdError> {
        let Some(limit) = params.get("maxInputBytes") else { return Ok(None) };
        let limit = limit.as_u64().ok_or_else(|| CmdError::Params("`maxInputBytes` must be a nonnegative whole number".into()))?;
        Ok(Some(Self { left: usize::try_from(limit).unwrap_or(usize::MAX) }))
    }

    fn check(&mut self, input: &impl serde::Serialize) -> Result<(), CmdError> {
        serde_json::to_writer(self, input).map_err(|_| {
            CmdError::Failed("This read's input is too large; use execute with document.paragraph and a specific path to read less.".into())
        })
    }
}

impl std::io::Write for ReadBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.left = self.left.checked_sub(bytes.len()).ok_or_else(|| std::io::Error::other("read input budget exceeded"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod read_budget_tests {
    use super::*;
    use serde::ser::SerializeSeq;
    use std::cell::Cell;

    #[test]
    fn change_summaries_do_not_budget_unreturned_field_payloads() {
        let mut s = Session::new(Document::from_text("small"));
        s.run("file.properties", &json!({"title": "x".repeat(1024)})).unwrap();
        let out = s.run("document.changes", &json!({"since": 0, "maxInputBytes": 128})).unwrap();
        assert_eq!(out["changes"][0]["fields"], json!(["core"]));
        assert!(s.run("document.changes", &json!({"since": 0, "maxInputBytes": 128, "full": true})).is_err());
    }

    #[test]
    fn a_capped_read_stops_serializing_before_the_tail() {
        struct Probe<'a> {
            text: &'a str,
            tail: &'a Cell<bool>,
        }
        impl serde::Serialize for Probe<'_> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut seq = serializer.serialize_seq(Some(2))?;
                seq.serialize_element(self.text)?;
                self.tail.set(true);
                seq.serialize_element(&"tail")?;
                seq.end()
            }
        }
        let text = "x".repeat(1024);
        let tail = Cell::new(false);
        let probe = Probe { text: &text, tail: &tail };
        let result = check_read_size(&json!({"maxInputBytes": 32}), &probe);
        assert!(!tail.get(), "the oversized first field must stop traversal before later fields");
        assert!(result.is_err());
        assert!(check_read_size(&json!({"maxInputBytes": 2048}), &probe).is_ok());
        assert!(tail.get());
    }
}

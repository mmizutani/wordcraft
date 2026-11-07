//! Command scopes: what each command touches, which decides who may run it when people and
//! agents share a document; and which of its params are places in the document, which an edit
//! made meanwhile moves.
//!
//! A command's scope is its id prefix's ([`Scope::by_prefix`]) unless [`OVERRIDES`] gives it
//! another, and covers everything the command can do: one that can change the document is never
//! the selection's or the view's, and one that can make another document or switch to one is
//! [`Scope::File`]'s. Agents in a shared document get [`Scope::Document`] and
//! [`Scope::Selection`] commands; files, other documents and the application's settings stay with
//! people unless the owner allows them. Paths in params are limited apart from scopes, by each
//! participant's [`crate::file_access`] roots.
//!
//! The tables live here rather than in each command, so the commands keep their shape. The tests
//! (`tests/scopes.rs`) check that every command has a scope, that a command scoped to the
//! selection or the view never changes the document, and that every param a command's params
//! doc gives as a place is in [`POSITIONS`].

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// What a command touches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Scope {
    /// The document everyone sharing it sees: its text, formatting, objects, styles, settings
    /// and properties; reading any of it too.
    Document,
    /// What is the participant's own while they edit: the caret and selection, Find, the
    /// clipboard, the pen in hand; and what is read through them.
    Selection,
    /// How the participant sees the document: views, zoom, panes and dialogs, Read Aloud.
    View,
    /// Files and the other open documents: opening, saving, exporting, printing, closing,
    /// versions and recovery; and commands that make a new document or switch to another.
    File,
    /// The application's settings and data, which every document shares: options, AutoCorrect,
    /// dictionaries, fonts, keyboard and ribbon, macros, language packs.
    App,
    /// The front end's own commands (`ui.*`), which act on its windows.
    Ui,
}

impl Scope {
    /// Whether a participant's copy of a shared document runs commands of this scope: not those
    /// on files, other documents and windows.
    pub fn in_shared_document(self) -> bool {
        matches!(self, Scope::Document | Scope::Selection | Scope::View | Scope::App)
    }

    /// The scope of the commands whose id has this prefix (the part before the first `.`),
    /// unless [`OVERRIDES`] gives one another; `None` for a prefix with no default, under which
    /// every command must be in [`OVERRIDES`].
    pub fn by_prefix(id: &str) -> Option<Scope> {
        let prefix = id.split('.').next().unwrap_or(id);
        Some(match prefix {
            "caret" | "select" => Scope::Selection,
            "view" | "readAloud" => Scope::View,
            "file" => Scope::File,
            // The assistant is the person's: a conversation is started and driven from their window.
            "font" | "proof" | "tools" | "assist" => Scope::App,
            // Sharing the document is the app window's (#33).
            "ui" | "room" => Scope::Ui,
            "arrange" | "canvas" | "chart" | "design" | "developer" | "document" | "draw" | "edit" | "equation" | "fields" | "format" | "hf"
            | "insert" | "layout" | "link" | "list" | "mailings" | "object" | "outline" | "para" | "picture" | "references" | "review" | "shape"
            | "smartArt" | "styles" | "table" | "text" | "wordArt" => Scope::Document,
            _ => return None,
        })
    }

    /// The scope of command `id`: its own in [`OVERRIDES`], else its prefix's.
    pub fn of(id: &str) -> Option<Scope> {
        overrides().get(id).copied().or_else(|| Scope::by_prefix(id))
    }
}

/// The [`Scope::File`] commands that only write a copy of the document to the file they name,
/// changing neither it nor which document is open: every participant's copy runs them
/// ([`crate::room::Role::refuses_command`]), held to the files the participant can use
/// ([`crate::room::Room::reach`]). Saving isn't one: it stamps the document (author, times,
/// revision) and gives it a path.
pub const EXPORTS: &[&str] = &["file.exportPdf", "file.exportPng", "file.printPdf"];

/// The commands whose scope isn't their prefix's.
pub const OVERRIDES: &[(&str, Scope)] = &[
    // The participant's own caret, selection, Find, clipboard and pen, and what is read through
    // them.
    ("edit.copy", Scope::Selection),
    ("edit.copyFormat", Scope::Selection),
    ("edit.formatPainter", Scope::Selection),
    ("edit.find", Scope::Selection),
    ("edit.advancedFind", Scope::Selection),
    ("edit.findNext", Scope::Selection),
    ("edit.findPrevious", Scope::Selection),
    ("edit.goto", Scope::Selection),
    ("edit.goBack", Scope::Selection),
    ("edit.clipboardItems", Scope::Selection),
    ("edit.deleteClipboardItem", Scope::Selection),
    ("edit.clearClipboard", Scope::Selection),
    ("table.selectTable", Scope::Selection),
    ("table.selectRow", Scope::Selection),
    ("table.selectColumn", Scope::Selection),
    ("table.selectCell", Scope::Selection),
    ("table.selectAt", Scope::Selection),
    ("review.nextComment", Scope::Selection),
    ("review.previousComment", Scope::Selection),
    ("review.nextChange", Scope::Selection),
    ("review.previousChange", Scope::Selection),
    ("review.spelling", Scope::Selection),
    ("review.editor", Scope::Selection),
    ("references.nextFootnote", Scope::Selection),
    ("fields.next", Scope::Selection),
    ("fields.previous", Scope::Selection),
    ("fields.selected", Scope::Selection),
    ("hf.next", Scope::Selection),
    ("hf.previous", Scope::Selection),
    ("insert.closeHeader", Scope::Selection),
    ("outline.selectSubtree", Scope::Selection),
    ("equation.edit", Scope::Selection),
    ("equation.exit", Scope::Selection),
    ("equation.inputFormat", Scope::Selection),
    ("equation.normalText", Scope::Selection),
    ("format.state", Scope::Selection),
    ("styles.inspect", Scope::Selection),
    ("developer.control", Scope::Selection),
    ("draw.select", Scope::Selection),
    ("draw.lasso", Scope::Selection),
    ("draw.pen", Scope::Selection),
    ("draw.pencil", Scope::Selection),
    ("draw.highlighter", Scope::Selection),
    ("document.selection", Scope::Selection),
    // How the participant sees the document: its panes, dialogs and view settings.
    ("edit.findReplace", Scope::View),
    ("edit.clipboardPane", Scope::View),
    ("format.fontDialog", Scope::View),
    ("para.dialog", Scope::View),
    ("review.markup", Scope::View),
    ("review.showMarkup", Scope::View),
    ("review.readAloud", Scope::View),
    ("review.hideInk", Scope::View),
    ("references.researcher", Scope::View),
    ("outline.expand", Scope::View),
    ("outline.collapse", Scope::View),
    ("outline.toggle", Scope::View),
    ("outline.showLevel", Scope::View),
    ("outline.showLevel1", Scope::View),
    ("outline.showLevel2", Scope::View),
    ("outline.showLevel3", Scope::View),
    ("outline.showLevel4", Scope::View),
    ("outline.showLevel5", Scope::View),
    ("outline.showLevel6", Scope::View),
    ("outline.showLevel7", Scope::View),
    ("outline.showLevel8", Scope::View),
    ("outline.showLevel9", Scope::View),
    ("outline.showAll", Scope::View),
    ("outline.firstLineOnly", Scope::View),
    ("outline.showFormatting", Scope::View),
    ("outline.close", Scope::View),
    ("outline.state", Scope::View),
    ("styles.pane", Scope::View),
    ("styles.inspector", Scope::View),
    ("smartArt.textPane", Scope::View),
    ("table.viewGridlines", Scope::View),
    ("developer.designMode", Scope::View),
    ("draw.replay", Scope::View),
    ("mailings.highlightFields", Scope::View),
    // Other documents: these can make a new one or switch to another.
    ("view.switchWindows", Scope::File),
    ("review.compare", Scope::File),
    ("review.combine", Scope::File),
    ("mailings.finish", Scope::File),
    ("mailings.printDocuments", Scope::File),
    ("mailings.envelopes", Scope::File),
    ("mailings.labels", Scope::File),
    // The application's settings and data.
    ("file.options", Scope::App),
    ("file.setAuthor", Scope::App),
    ("view.interfaceTheme", Scope::App),
    ("review.proofing", Scope::App),
    ("review.addToDictionary", Scope::App),
    ("review.ignoreAll", Scope::App),
    ("review.language.preferred", Scope::App),
    ("text.overtype", Scope::App),
    ("design.setDefault", Scope::App),
    ("draw.addPen", Scope::App),
    ("draw.removePen", Scope::App),
    // The document, under a prefix that isn't.
    ("file.properties", Scope::Document),
    ("file.privacy", Scope::Document),
    ("file.privacyOptions", Scope::Document),
    ("file.info", Scope::Document),
    ("file.inspect", Scope::Document),
    ("file.protect", Scope::Document),
    ("file.accessibility", Scope::Document),
    ("file.compatibility", Scope::Document),
    ("font.missing", Scope::Document),
    ("tools.templates", Scope::Document),
    ("tools.attachTemplate", Scope::Document),
    ("tools.detachTemplate", Scope::Document),
    ("tools.linkStyles", Scope::Document),
    ("tools.updateStyles", Scope::Document),
    // Showing the Assistant pane is the participant's own view.
    ("assist.pane", Scope::View),
];

fn overrides() -> &'static HashMap<&'static str, Scope> {
    static MAP: OnceLock<HashMap<&'static str, Scope>> = OnceLock::new();
    MAP.get_or_init(|| OVERRIDES.iter().copied().collect())
}

/// How a param names a place in the document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Place {
    /// A place in the text: `{"story", "path", "off"}`, or `{"block": index, "off"}` in the body.
    Pos,
    /// A list of places (among other things, such as Selection Pane indexes).
    List,
    /// A block: its path in its story (`story` beside it, else the caret's).
    Path,
}

/// The params of each command that name places in the document, and how. Commands not listed
/// take none. Page coordinates (`{"page", "x", "y"}`) aren't places in the text: they belong to
/// one copy's layout.
pub const POSITIONS: &[(&str, &[(&str, Place)])] = &[
    ("caret.set", &[("pos", Place::Pos)]),
    ("select.range", &[("anchor", Place::Pos), ("focus", Place::Pos)]),
    ("select.column", &[("anchor", Place::Pos), ("focus", Place::Pos)]),
    ("select.dragUnit", &[("start", Place::Pos), ("end", Place::Pos), ("pos", Place::Pos)]),
    ("select.line", &[("pos", Place::Pos)]),
    ("select.addObject", &[("pos", Place::Pos), ("objects", Place::List)]),
    ("document.paragraph", &[("path", Place::Path)]),
    ("arrange.bounds", &[("to", Place::Pos)]),
    ("arrange.copyTo", &[("to", Place::Pos)]),
    ("shape.link", &[("target", Place::Pos)]),
    ("shape.connect", &[("start", Place::Pos), ("end", Place::Pos)]),
    ("layout.manualHyphenation", &[("from", Place::Pos)]),
    ("review.suggestions", &[("pos", Place::Pos)]),
    ("developer.control", &[("at", Place::Pos)]),
    ("developer.toggleCheckBox", &[("at", Place::Pos)]),
    ("developer.chooseItem", &[("at", Place::Pos)]),
    ("developer.setDate", &[("at", Place::Pos)]),
    ("developer.removeControl", &[("at", Place::Pos)]),
];

/// The params of command `id` that name places in the document ([`POSITIONS`]).
pub fn positions(id: &str) -> &'static [(&'static str, Place)] {
    static MAP: OnceLock<HashMap<&'static str, &'static [(&'static str, Place)]>> = OnceLock::new();
    MAP.get_or_init(|| POSITIONS.iter().copied().collect()).get(id).copied().unwrap_or_default()
}

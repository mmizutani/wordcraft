use super::{CloudEntry, CloudFailure, CloudFailureKind, CloudReply, CloudRequest, CloudResponse, CloudService};
mod view;
use std::collections::HashMap;
use std::sync::mpsc::TryRecvError;
pub(crate) use view::{show_library, show_save, show_status, show_waiting_navigation};

use serde_json::{Value, json};
use wordcraft_engine::{Password, Session};

use crate::{SaveInFlight, WordApp, async_saves, dialogs::Dialog, panes};

const MAX_BYTES: usize = 16 * 1024 * 1024;
const CLOUD_CREATE_FORMAT: &str = "Save to Cloud supports new documents and local DOCX files.";

#[derive(Default)]
pub(crate) struct State {
    documents: HashMap<u64, CloudEntry>,
    saves: HashMap<u64, Save>,
    held: Option<Held>,
    opening: Option<Opening>,
    library: Library,
    pub(crate) draft_name: String,
}

struct DocumentSnapshot {
    revision: u64,
    // Setting or removing encryption changes dirty state without advancing the edit revision.
    password: Option<Password>,
}

impl DocumentSnapshot {
    fn capture(session: &Session) -> Self {
        Self { revision: session.rev(), password: session.password.clone() }
    }

    fn matches(&self, session: &Session) -> bool {
        self.revision == session.rev() && self.password == session.password
    }
}

struct Save {
    request: CloudRequest,
    snapshot: DocumentSnapshot,
    phase: SavePhase,
}

enum SavePhase {
    Sending(CloudReply),
    Failed(CloudFailure),
}

struct Held {
    document: u64,
    then: String,
    params: Value,
}

struct Opening {
    document: u64,
    snapshot: DocumentSnapshot,
    entry: CloudEntry,
    result: OpenResult,
    save_pending: bool,
    confirmed: bool,
    /// The window started it, so it may open dialogs. A script's open never does (`execute`).
    asks: bool,
}

enum OpenResult {
    Fetching(CloudReply),
    Ready(Vec<u8>),
}

#[derive(Default)]
struct Library {
    entries: Vec<CloudEntry>,
    next_cursor: Option<String>,
    busy: Option<(LibraryAction, CloudReply)>,
    loaded: bool,
    refresh_needed: bool,
    confirm_delete: Option<CloudEntry>,
    message: String,
}

enum LibraryAction {
    List { append: bool },
    Download(String),
    Delete(String),
}

fn poll(reply: &CloudReply) -> Option<Result<CloudResponse, CloudFailure>> {
    match reply.try_recv() {
        Ok(result) => Some(result),
        Err(TryRecvError::Empty) => None,
        Err(TryRecvError::Disconnected) => {
            Some(Err(CloudFailure { kind: CloudFailureKind::Retryable, message: tl!("The cloud request was interrupted.").into() }))
        }
    }
}

fn unexpected() -> CloudFailure {
    CloudFailure { kind: CloudFailureKind::Retryable, message: tl!("The cloud returned an unexpected response.").into() }
}

impl State {
    pub(crate) fn bound(&self, document: u64) -> bool {
        self.documents.contains_key(&document)
    }

    pub(crate) fn unsaved(&self, document: u64) -> bool {
        self.saves.contains_key(&document)
    }

    fn sending(&self, document: u64) -> bool {
        self.saves.get(&document).is_some_and(|s| matches!(s.phase, SavePhase::Sending(_)))
    }

    pub(crate) fn discard_unattended_open(&mut self) {
        if !self.held() && self.opening.as_ref().is_some_and(|opening| matches!(opening.result, OpenResult::Ready(_))) {
            self.opening = None;
        }
    }

    pub(crate) fn held(&self) -> bool {
        self.held.is_some()
    }

    fn fetching(&self) -> bool {
        self.opening.as_ref().is_some_and(|opening| matches!(opening.result, OpenResult::Fetching(_)))
    }
}

impl WordApp {
    pub(crate) fn cancel_cloud_navigation(&mut self) {
        self.cloud.opening = None;
        self.cloud.held = None;
        // The picker still reports its write; only its obsolete cloud navigation is canceled.
        let _ = self.awaiting_save.take_if(|wait| matches!(wait.then.as_str(), "ui.cloudOpen" | "ui.cloudApply"));
    }

    pub(crate) fn cloud_command(&mut self, id: &str, params: &Value) -> Option<Result<Value, String>> {
        let result = match id {
            "ui.cloudSave" => self.cloud_save(params.get("name").and_then(Value::as_str), params),
            "ui.cloudDownload" => self.cloud_download_edits().map(|()| json!({})),
            "ui.cloudRefresh" => self.cloud_list(false).map(|()| json!({})),
            "ui.cloudMore" => self.cloud_list(true).map(|()| json!({})),
            "ui.cloudOpen" => self.cloud_open(params),
            "ui.cloudApply" => {
                self.cloud_apply(params.get("password").and_then(Value::as_str), params.get("force").and_then(Value::as_bool) == Some(true))
            }
            "ui.cloudLibraryDownload" => self.cloud_library_download(params).map(|()| json!({})),
            "ui.cloudDelete" => self.cloud_delete(params).map(|()| json!({})),
            "ui.cloudCancelNavigation" => {
                self.cancel_cloud_navigation();
                Ok(json!({}))
            }
            _ => return None,
        };
        if let Err(error) = &result {
            self.status(error.clone());
            if self.ui.backstage && self.ui.backstage_page == "open" {
                self.cloud.library.message.clone_from(error);
            }
        }
        Some(result)
    }

    fn cloud_service(&self) -> Result<&CloudService, String> {
        self.services.cloud.as_ref().ok_or_else(|| tl!("Cloud documents are unavailable here.").into())
    }

    pub(crate) fn cloud_saves_current(&self) -> bool {
        let document = self.session.document_id();
        self.cloud.bound(document) || self.cloud.unsaved(document)
    }

    pub(crate) fn cloud_wait_for_save(&mut self, then: String, params: Value, document: u64) -> Result<Value, String> {
        match self.cloud_save(None, &json!({})) {
            Ok(_) => {
                self.cloud.held = Some(Held { document, then, params });
                Ok(json!({"pending": "cloudSave"}))
            }
            Err(error) => {
                self.cloud_save_prompt(then, params, document, error.clone());
                Err(error)
            }
        }
    }

    pub(crate) fn cloud_approve_saved_open(&mut self) {
        if !self.document_unsaved()
            && !panes::has_comment_drafts(self)
            && let Some(opening) = self.cloud.opening.as_mut()
            && opening.snapshot.matches(&self.session)
        {
            opening.confirmed = true;
        }
    }

    fn cloud_save_prompt(&mut self, then: String, params: Value, document: u64, message: String) {
        self.dialog = Some(Dialog::SaveChanges { name: self.prompt_name(), then, params, document, message });
    }

    pub(crate) fn cloud_save(&mut self, name: Option<&str>, params: &Value) -> Result<Value, String> {
        let document = self.session.document_id();
        if let Some(save) = self.cloud.saves.get(&document) {
            if params.get("password").is_some_and(|password| !password.is_null()) {
                return Err(tl!("A cloud save is pending. Finish that save before setting a password through Save.").into());
            }
            let mut request = save.request.clone();
            match &save.phase {
                SavePhase::Sending(_) => return Ok(json!({"pending": "cloudSave"})),
                SavePhase::Failed(failure) => match failure.kind {
                    CloudFailureKind::Missing | CloudFailureKind::Stale => return Err(failure.message.clone()),
                    CloudFailureKind::Abandoned => {
                        if let CloudRequest::Save { operation_id, .. } = &mut request {
                            *operation_id = (self.cloud_service()?.new_operation_id)().map_err(|e| e.message)?;
                        }
                    }
                    _ => {}
                },
            }
            let reply = (self.cloud_service()?.request)(request.clone());
            if let Some(save) = self.cloud.saves.get_mut(&document) {
                save.request = request;
                save.phase = SavePhase::Sending(reply);
            }
        } else {
            let entry = self.cloud.documents.get(&document);
            let (id, base_revision, name) = match entry {
                Some(entry) => (Some(entry.id.clone()), Some(entry.revision), entry.name.clone()),
                None => {
                    if !self.cloud_can_create() {
                        return Err(tl!(CLOUD_CREATE_FORMAT).into());
                    }
                    (None, None, cloud_name(name.ok_or_else(|| tl!("Enter a name for the cloud document.").to_owned())?)?)
                }
            };
            let operation_id = (self.cloud_service()?.new_operation_id)().map_err(|e| e.message)?;
            if let Some(password) = params.get("password").filter(|value| !value.is_null()) {
                if password.as_str().is_none_or(str::is_empty) {
                    return Err(tl!("The password cannot be empty.").into());
                }
                self.session.run("file.encrypt", &json!({"password": password})).map_err(|e| e.to_string())?;
            }
            self.session.stamp_save();
            let bytes = self.cloud_docx(&name)?;
            if bytes.len() > MAX_BYTES {
                return Err(tl!("Cloud documents can be at most 16 MiB.").into());
            }
            let request = CloudRequest::Save { operation_id, id, base_revision, name, bytes: bytes.into() };
            let reply = (self.cloud_service()?.request)(request.clone());
            self.cloud.saves.insert(document, Save { request, snapshot: DocumentSnapshot::capture(&self.session), phase: SavePhase::Sending(reply) });
        }
        self.report_dirty();
        Ok(json!({"pending": "cloudSave"}))
    }

    fn cloud_docx(&self, name: &str) -> Result<Vec<u8>, String> {
        let password = self.session.password.as_ref().map(wordcraft_engine::Password::as_str);
        wordcraft_engine::io::save_bytes_with(name, &self.session.doc, password, &Default::default())
    }

    fn cloud_download_edits(&mut self) -> Result<(), String> {
        self.cloud_service()?;
        let name = self.cloud.documents.get(&self.session.document_id()).map(|e| e.name.clone()).unwrap_or_else(|| self.default_cloud_name());
        let bytes = self.cloud_docx(&name)?;
        self.cloud_download(&name, &bytes)
    }

    fn cloud_download(&mut self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let download = self.services.download.as_ref().ok_or_else(|| tl!("Downloads are unavailable here.").to_owned())?;
        let saves = self.services.async_saves.as_ref();
        if let Some(saves) = saves {
            let _ = saves.take_started();
        }
        download(name, bytes)?;
        if let Some(ticket) = saves.and_then(async_saves::AsyncSaves::take_started) {
            self.saves_in_flight.push(SaveInFlight { ticket, name: name.into(), document: self.session.document_id(), marked_saved: false });
        }
        Ok(())
    }

    fn cloud_can_create(&self) -> bool {
        !wordcraft_engine::io::docx_has_vba(&self.session.doc)
            && self
                .session
                .path
                .as_ref()
                .is_none_or(|path| path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("docx")))
    }

    pub(crate) fn default_cloud_name(&self) -> String {
        let name = self.default_save_name();
        if name.to_ascii_lowercase().ends_with(".docx") { name } else { format!("{}.docx", self.title_stem()) }
    }

    fn cloud_list(&mut self, append: bool) -> Result<(), String> {
        let library = &self.cloud.library;
        if library.busy.is_some() || (append && library.next_cursor.is_none()) {
            return Ok(());
        }
        let cursor = if append { library.next_cursor.clone() } else { None };
        if !append {
            self.cloud.library.refresh_needed = false;
        }
        self.start_library_request(LibraryAction::List { append }, CloudRequest::List { cursor })
    }

    fn library_entry(&self, params: &Value) -> Result<CloudEntry, String> {
        let id = params.get("id").and_then(Value::as_str).ok_or_else(|| tl!("Choose a cloud document.").to_owned())?;
        self.cloud
            .library
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
            .ok_or_else(|| tl!("Refresh the cloud document list and try again.").into())
    }

    fn start_library_request(&mut self, action: LibraryAction, request: CloudRequest) -> Result<(), String> {
        if self.cloud.library.busy.is_none() {
            let reply = (self.cloud_service()?.request)(request);
            self.cloud.library.busy = Some((action, reply));
        }
        Ok(())
    }

    fn cloud_library_download(&mut self, params: &Value) -> Result<(), String> {
        let entry = self.library_entry(params)?;
        self.cloud.library.message.clear();
        self.start_library_request(LibraryAction::Download(entry.name), CloudRequest::Open { id: entry.id })
    }

    fn cloud_delete(&mut self, params: &Value) -> Result<(), String> {
        let entry = self.library_entry(params)?;
        if params.get("revision").and_then(Value::as_u64) != Some(entry.revision) {
            return Err(tl!("The cloud document changed. Confirm the current revision before deleting.").into());
        }
        self.cloud.library.message.clear();
        self.start_library_request(LibraryAction::Delete(entry.id.clone()), CloudRequest::Delete { id: entry.id, revision: entry.revision })
    }

    fn cloud_open(&mut self, params: &Value) -> Result<Value, String> {
        let document = self.session.document_id();
        if (self.document_unsaved() || panes::has_comment_drafts(self)) && params.get("force").and_then(Value::as_bool) != Some(true) {
            return Err(tl!("Save or explicitly discard your changes before opening a cloud document.").into());
        }
        let entry = if params.get("reload").and_then(Value::as_bool) == Some(true) {
            self.cloud.documents.get(&document).cloned().ok_or_else(|| tl!("This document has no cloud version.").to_owned())?
        } else {
            self.library_entry(params)?
        };
        let reply = (self.cloud_service()?.request)(CloudRequest::Open { id: entry.id.clone() });
        self.cloud.library.message.clear();
        self.cloud.opening = Some(Opening {
            document,
            snapshot: DocumentSnapshot::capture(&self.session),
            entry,
            result: OpenResult::Fetching(reply),
            save_pending: self.cloud.unsaved(document),
            confirmed: true,
            asks: params.get("asks").and_then(Value::as_bool) == Some(true),
        });
        Ok(json!({"pending": "cloudOpen"}))
    }

    fn cloud_apply(&mut self, password: Option<&str>, force: bool) -> Result<Value, String> {
        let Some(mut opening) = self.cloud.opening.take() else {
            return Err(tl!("The cloud open was canceled.").into());
        };
        if opening.document != self.session.document_id() {
            return Err(tl!("The cloud open was canceled because another document is showing.").into());
        }
        self.catch_up_share();
        let drafted = panes::has_comment_drafts(self);
        let draft_refused = drafted && panes::flush_comment_drafts(self).is_err();
        let changed = !opening.snapshot.matches(&self.session) || (!opening.save_pending && self.cloud.unsaved(opening.document));
        if changed || (!force && (draft_refused || !opening.confirmed)) {
            return self.confirm_cloud_replacement(opening);
        }
        opening.confirmed = true;
        let OpenResult::Ready(bytes) = &opening.result else {
            self.cloud.opening = Some(opening);
            return Err(tl!("The cloud document is still loading.").into());
        };
        if self
            .cloud
            .documents
            .get(&opening.document)
            .is_some_and(|current| current.id == opening.entry.id && current.revision > opening.entry.revision)
        {
            return Err(tl!("This tab already has a newer saved cloud revision.").into());
        }
        // The same bounded import and password handling used by the local file command.
        let opened = wordcraft_engine::io::open_docx_bytes(bytes, password);
        let (document, encrypted) = match opened {
            Ok(opened) => opened,
            Err(error) if wordcraft_engine::io::needs_password(&error) || wordcraft_engine::io::wrong_password(&error) => {
                let (name, asks) = (opening.entry.name.clone(), opening.asks);
                self.cloud.opening = Some(opening);
                // A script hears why, and can send the password with `ui.cloudApply`.
                if password.is_some() || !asks {
                    return Err(error);
                }
                self.dialog = Some(Dialog::Password {
                    name,
                    then: "ui.cloudApply".to_string(),
                    params: json!({"cloud": true}),
                    password: Default::default(),
                    message: String::new(),
                });
                return Ok(json!({"pending": "password"}));
            }
            Err(error) => return Err(error),
        };
        if self.shows_shared_document() {
            self.stop_sharing();
            if !opening.snapshot.matches(&self.session) {
                return self.confirm_cloud_replacement(opening);
            }
        }
        let title = cloud_title(&opening.entry.name);
        let zoom = document.settings.zoom;
        self.session.set_document(document);
        self.session.path = None;
        self.session.file_name = Some(title);
        if encrypted {
            self.session.password = password.map(wordcraft_engine::Password::new);
        }
        if let Some(zoom) = zoom {
            let _ = self.session.run("view.zoom", &json!({"value": zoom.percent.clamp(10, 500)}));
            if let Some(fit) = zoom.fit {
                let fit = match fit {
                    wordcraft_doc::ZoomFit::OnePage => "onePage",
                    wordcraft_doc::ZoomFit::PageWidth => "pageWidth",
                    wordcraft_doc::ZoomFit::TextWidth => "textWidth",
                };
                let _ = self.session.run("view.zoom", &json!({"value": fit}));
            }
        }
        let local = self.session.document_id();
        self.cloud.documents.insert(local, opening.entry);
        self.sync_document();
        self.recovery_seen = true;
        self.ui.backstage = false;
        self.canvas.want_focus = true;
        Ok(json!({"opened": true}))
    }

    fn confirm_cloud_replacement(&mut self, mut opening: Opening) -> Result<Value, String> {
        let message = tl!("The document changed while opening the cloud copy.");
        if !opening.asks {
            return Err(message.into());
        }
        opening.snapshot = DocumentSnapshot::capture(&self.session);
        opening.save_pending = self.cloud.unsaved(opening.document);
        opening.confirmed = false;
        let document = opening.document;
        self.cloud.opening = Some(opening);
        self.cloud_save_prompt("ui.cloudApply".into(), json!({}), document, message.into());
        Ok(json!({"pending": "saveChanges"}))
    }
}

fn cloud_title(name: &str) -> String {
    name.rsplit_once('.').map_or(name, |(stem, _)| stem).into()
}

fn cloud_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    let complete = if name.to_ascii_lowercase().ends_with(".docx") { name.to_owned() } else { format!("{name}.docx") };
    if complete.len() > 255
        || complete.chars().any(|c| c.is_control() || matches!(c, '/' | '\\'))
        || complete.rsplit_once('.').is_none_or(|(stem, _)| stem.trim().is_empty())
    {
        return Err(tl!("Enter a document name of up to 255 bytes without slashes or control characters.").into());
    }
    Ok(complete)
}

impl WordApp {
    pub(crate) fn settle_cloud(&mut self) {
        if self.services.cloud.is_none() {
            return;
        }
        let session = &self.session;
        self.cloud.documents.retain(|document, _| session.is_open(*document));
        self.cloud.saves.retain(|document, _| session.is_open(*document));
        let completed: Vec<_> = self
            .cloud
            .saves
            .iter()
            .filter_map(|(document, save)| match &save.phase {
                SavePhase::Sending(reply) => poll(reply).map(|result| (*document, result)),
                SavePhase::Failed(_) => None,
            })
            .collect();
        for (document, result) in completed {
            if let Some(save) = self.cloud.saves.remove(&document) {
                self.finish_cloud_save(document, save, result);
            }
        }
        if let Some(result) = self.cloud.library.busy.as_ref().and_then(|(_, reply)| poll(reply))
            && let Some((action, _)) = self.cloud.library.busy.take()
        {
            self.finish_library_request(action, result);
        }
        if let Some(result) = self.cloud.opening.as_ref().and_then(|opening| match &opening.result {
            OpenResult::Fetching(reply) => poll(reply),
            OpenResult::Ready(_) => None,
        }) && let Some(mut opening) = self.cloud.opening.take()
            && opening.document == self.session.document_id()
        {
            match result {
                Ok(CloudResponse::Open { revision, bytes }) if revision > 0 && !bytes.is_empty() && bytes.len() <= MAX_BYTES => {
                    opening.entry.revision = revision;
                    opening.entry.byte_size = bytes.len() as u64;
                    opening.result = OpenResult::Ready(bytes);
                    self.cloud.opening = Some(opening);
                    if let Err(error) = self.cloud_apply(None, false) {
                        self.cloud_open_error(error);
                    }
                }
                Err(error) => self.cloud_open_error(error.message),
                _ => self.cloud_open_error(unexpected().message),
            }
        }
    }

    fn cloud_open_error(&mut self, error: String) {
        self.cloud.library.message = error.clone();
        self.status(error);
    }

    fn finish_cloud_save(&mut self, document: u64, mut save: Save, result: Result<CloudResponse, CloudFailure>) {
        let CloudRequest::Save { id, base_revision, name, bytes, .. } = &save.request else { return };
        let expected_revision = base_revision.unwrap_or_default().saturating_add(1);
        let result = match result {
            Ok(CloudResponse::Save(entry))
                if id.as_ref().is_none_or(|id| *id == entry.id)
                    && !entry.id.is_empty()
                    && entry.revision == expected_revision
                    && entry.name == *name
                    && entry.byte_size == bytes.len() as u64 =>
            {
                Ok(entry)
            }
            Ok(_) => Err(unexpected()),
            Err(error) => Err(error),
        };
        let active = self.session.document_id() == document;
        let held = self.cloud.held.take_if(|held| held.document == document);
        match result {
            Ok(entry) => {
                let _ = self.session.with_document(document, |session| {
                    session.path = None;
                    session.file_name = Some(cloud_title(&entry.name));
                    // A receipt for this operation says nothing about edits made after it.
                    session.dirty = !save.snapshot.matches(session);
                });
                if active {
                    self.autosave_path = None;
                }
                self.cloud.library.refresh_needed = true;
                self.cloud.documents.insert(document, entry);
                if let Some(held) = held.filter(|_| active) {
                    if save.snapshot.matches(&self.session) && !self.document_unsaved() && !panes::has_comment_drafts(self) {
                        if held.then == "ui.cloudApply"
                            && let Some(opening) = &mut self.cloud.opening
                        {
                            opening.snapshot = DocumentSnapshot::capture(&self.session);
                            opening.save_pending = false;
                            opening.confirmed = true;
                        }
                        if let Err(error) = self.execute_held_back(&held.then, held.params) {
                            self.status(error);
                        }
                    } else {
                        self.cloud_save_prompt(held.then, held.params, document, tl!("The document has changes made after the save began.").into());
                    }
                }
            }
            Err(error) => {
                let message = error.message.clone();
                let _ = self.session.with_document(document, |session| session.dirty = true);
                // A definitive validation error can be corrected and saved as a new operation.
                if error.kind != CloudFailureKind::Invalid {
                    save.phase = SavePhase::Failed(error);
                    self.cloud.saves.insert(document, save);
                }
                if active {
                    self.status(message.clone());
                    if let Some(held) = held {
                        self.cloud_save_prompt(held.then, held.params, document, message);
                    }
                }
            }
        }
    }

    fn finish_library_request(&mut self, action: LibraryAction, result: Result<CloudResponse, CloudFailure>) {
        let library = &mut self.cloud.library;
        let error = match (action, result) {
            (LibraryAction::List { append }, Ok(CloudResponse::List { documents, next_cursor })) => {
                if !append {
                    library.entries.clear();
                }
                for entry in documents {
                    if !library.entries.iter().any(|old| old.id == entry.id) {
                        library.entries.push(entry);
                    }
                }
                library.next_cursor = next_cursor;
                library.loaded = true;
                return;
            }
            (LibraryAction::Download(name), Ok(CloudResponse::Open { bytes, .. })) => {
                if let Err(error) = self.cloud_download(&name, &bytes) {
                    self.cloud_open_error(error);
                }
                return;
            }
            (LibraryAction::Delete(id), Ok(CloudResponse::Delete { cleanup_pending })) => {
                library.entries.retain(|entry| entry.id != id);
                library.message =
                    if cleanup_pending { tl!("Document removed. Storage cleanup is pending.") } else { tl!("Cloud document deleted.") }.into();
                return;
            }
            (LibraryAction::Delete(_), Err(error)) if error.kind == CloudFailureKind::Stale => {
                library.message = tl!("The cloud document changed. Review the refreshed list and confirm again.").into();
                let _ = self.cloud_list(false);
                return;
            }
            (_, Err(error)) => error,
            _ => unexpected(),
        };
        self.cloud_open_error(error.message);
    }
}

impl State {
    pub(crate) fn request_pending(&self) -> bool {
        self.library.busy.is_some() || self.fetching() || self.saves.values().any(|save| matches!(save.phase, SavePhase::Sending(_)))
    }
}

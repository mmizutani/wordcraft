//! Sharing the document the window shows ([`wordcraft_engine::room`]): the window's session is
//! its owner's copy, lent to it for each step, so what the person types goes through the room's
//! hub like anyone's edit and comes back made again, not lost, when it clashes with an agent's.
//! Participants with a key join through the control channel ([`Lobby`]), each with a copy of
//! their own on their connection's thread; their changes arrive here every frame.
//!
//! The control channel's `room.*` methods are the owner's ([`room_method`]), and commands of the
//! window's too, which File › Share runs ([`crate::share_page`]). The others see where the
//! person's caret and selection are while the window shows the shared document
//! ([`Replica::present`]); the others' carets and names in the window come next (milestone
//! A1.5).

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use wordcraft_engine::agent_files::AgentFiles;
use wordcraft_engine::room::{Lobby, ParticipantId, Replica, Role, Room, Terms};
use wordcraft_engine::scope::Scope;
use wordcraft_engine::{CmdError, Session};

use crate::WordApp;

/// The document the window shares: its room, and the owner's copy, which works on the window's
/// session ([`Replica::lend`]).
pub(crate) struct Share {
    room: Arc<Room>,
    copy: Replica,
}

/// The name the owner joins under when the window has no author name.
const OWNER: &str = "Owner";

impl Share {
    /// Run command `id` through the copy, on `session`, when its active document is the shared
    /// one and the command is one a copy runs. Others (files and windows) get what others
    /// changed first, so a save has it, and run on the session: `None` then, and when the
    /// session's active document is another one.
    pub(crate) fn run(&mut self, session: &mut Session, id: &str, params: &Value, ime: bool) -> Option<Result<Value, CmdError>> {
        let copy_s = Scope::of(id).is_some_and(|s| Role::Owner.allows(s)) && session.registry.get(id).is_some();
        self.copy
            .lend(session, |c| {
                if !copy_s {
                    c.sync();
                    return None;
                }
                let r = if ime { c.run_ime(id, params) } else { c.run(id, params) };
                c.send();
                Some(r)
            })
            .flatten()
    }
}

impl WordApp {
    /// Let participants with a key find the document this window shares in `lobby` (the control
    /// server's).
    pub fn with_lobby(mut self, lobby: Lobby) -> Self {
        self.lobby = lobby;
        self
    }

    /// Run command `id` through the shared document's copy, when the window shows that document
    /// and the command is one a copy runs (not files and windows, which get what others
    /// changed first: a save has it). `None` when it isn't the copy's to run.
    pub(crate) fn run_shared(&mut self, id: &str, params: &Value, ime: bool) -> Option<Result<Value, CmdError>> {
        self.share.as_mut()?.run(&mut self.session, id, params, ime)
    }

    /// The name of the built-in assistant's conversation, still going, when it joined the shared
    /// document as `name` (compared without case).
    pub(crate) fn assistant_named(&self, name: &str) -> Option<String> {
        let a = self.conversation().filter(|a| !a.has_ended())?;
        (a.name().to_lowercase() == name.trim().to_lowercase()).then(|| a.name().to_string())
    }

    /// The document the window shares, if it shares one.
    pub(crate) fn shared_document(&self) -> Option<u64> {
        self.share.as_ref().map(|s| s.copy.document())
    }

    /// Every frame: the shared document's copy gets what others changed and sends what the
    /// window changed, also while the window shows another document, so that one is up to date
    /// whenever it is saved, closed or shown. Sharing stops when the window no longer has the
    /// document open.
    pub(crate) fn sync_share(&mut self, ctx: &egui::Context) {
        // File › Share shows an invite's key only while the page stays open.
        if !(self.ui.backstage && self.ui.backstage_page == "share") {
            self.share_form.shown = None;
        }
        let Some(share) = self.share.as_mut() else { return };
        if share.room.is_closed() || !self.session.is_open(share.copy.document()) {
            self.stop_sharing();
            return;
        }
        // The others see where the person is while the window shows the document.
        let shows = share.copy.document() == self.session.document_id();
        let _ = share.copy.lend_document(&mut self.session, |c| {
            c.sync();
            if shows { c.present() } else { c.absent() }
        });
        // Its own come back with the next change the room takes (which asks for a frame); a
        // change sent back is sent again a little later.
        if share.copy.has_unconfirmed() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    /// Share the active document under `name` (the author name when `None`): the window joins
    /// its room as the owner, and participants with a key it hands out ([`room_method`],
    /// `room.invite`) join through the lobby.
    pub(crate) fn share_document(&mut self, name: Option<&str>) -> Result<Value, String> {
        if !self.has_document() {
            return Err("no document is open to share".into());
        }
        if let Some(share) = &self.share {
            if share.copy.document() == self.session.document_id() {
                return Ok(self.share_state());
            }
            return Err("another open document is shared: stop sharing it first (room.stop)".into());
        }
        let author = self.session.author.trim().to_string();
        let name = name.map(str::to_string).unwrap_or(if author.is_empty() { OWNER.to_string() } else { author });
        let (room, copy) = Room::host(&mut self.session, &name).map_err(|e| e.to_string())?;
        if let Some(ctx) = self.ctx.clone() {
            room.on_change(move || ctx.request_repaint());
        }
        self.lobby.open(room.clone());
        self.share = Some(Share { room, copy });
        Ok(self.share_state())
    }

    /// Stop sharing: the room closes, and the participants' copies stop answering. The window
    /// keeps the document as it is, with every change the room took in it, whether it shows the
    /// document or another one. Whether it shared one.
    pub(crate) fn stop_sharing(&mut self) -> bool {
        let Some(mut share) = self.share.take() else { return false };
        // The room takes nothing more, and what it took last is the window's too.
        share.room.close();
        let _ = share.copy.lend_document(&mut self.session, Replica::receive);
        self.lobby.close();
        true
    }

    /// Get what others changed in the shared document since the last frame, whichever document
    /// the window shows: before a close asks whether the document has unsaved changes, and
    /// before a control request reads, saves or closes it.
    pub(crate) fn catch_up_share(&mut self) {
        if let Some(share) = self.share.as_mut() {
            let _ = share.copy.lend_document(&mut self.session, Replica::receive);
        }
    }

    /// Before open document `doc` closes on a request with `params`: when it is the shared one
    /// and the close goes ahead (nothing unsaved, or `force`), sharing stops, so the room takes
    /// nothing more, and what it took last is in the document when the close checks it for
    /// unsaved changes again. A change an agent was told went through is never dropped unasked.
    pub(crate) fn stop_sharing_to_close(&mut self, doc: u64, params: &Value) {
        if self.shared_document() != Some(doc) {
            return;
        }
        self.catch_up_share();
        // The same unsaved work the close itself refuses on (#3274).
        if crate::control::close_refused_for(self, doc, params).is_none() {
            self.stop_sharing();
        }
    }

    /// [`WordApp::stop_sharing_to_close`] before `file.closeDocument` with `params` runs while
    /// document `active` is the active one: for the document its `id` names, else `active`. The
    /// engine then checks what the room took last for unsaved changes.
    pub(crate) fn stop_sharing_to_close_document(&mut self, active: u64, params: &Value) {
        let doc = match params.get("id").filter(|v| !v.is_null()) {
            None => Some(active),
            Some(v) => v.as_u64(),
        };
        if let Some(doc) = doc {
            self.stop_sharing_to_close(doc, params);
        }
    }

    /// Before the app quits with nothing unsaved: sharing stops, so the room takes nothing more.
    /// Whether what it took last left a document with unsaved changes, to ask about first.
    pub(crate) fn stop_sharing_to_quit(&mut self) -> bool {
        self.stop_sharing() && self.has_unsaved_documents()
    }

    /// Whether the document the window shows is the one it shares.
    pub(crate) fn shows_shared_document(&self) -> bool {
        self.shared_document() == Some(self.session.document_id())
    }

    /// The room of the document the window shares, if it shares one: the assistant joins it
    /// (#15).
    pub(crate) fn shared_room(&self) -> Option<Arc<Room>> {
        self.share.as_ref().map(|s| s.room.clone())
    }

    /// Whether the window has every change the room took and the room every change the window
    /// sent (tests wait for it).
    #[cfg(test)]
    pub(crate) fn share_settled(&self) -> bool {
        self.share.as_ref().is_none_or(|s| !s.copy.has_unconfirmed() && s.copy.received() == s.room.version())
    }

    /// Whether the window shares a document, which one, its version, who the window is in it and
    /// who else is, each with their colour and where their selection is in that version
    /// ([`wordcraft_engine::room::participant_json`]): for each participant who joined with a key,
    /// which files their commands can use now (`files`), as File › Options › Agents
    /// (`agentFiles`) and their invite say; and the invites not taken back, each with how many
    /// copies joined with it (`invited`).
    pub(crate) fn share_state(&self) -> Value {
        match &self.share {
            None => json!({"shared": false}),
            Some(s) => {
                let (version, mut participants) = s.room.participants_now();
                for v in &mut participants {
                    let id = v.get("id").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).map(ParticipantId);
                    if let Some(files) = id.and_then(|id| s.room.held_to(id)) {
                        v["files"] = json!(files.name());
                    }
                }
                let invited: Vec<Value> = s
                    .room
                    .invites()
                    .into_iter()
                    .map(|i| {
                        json!({
                            "name": i.name,
                            "role": i.role,
                            "files": i.files.map(AgentFiles::name),
                            "joined": i.joined,
                            "remote": i.remote,
                            "expires": i.expires,
                            "usesLeft": i.uses_left,
                        })
                    })
                    .collect();
                json!({
                    "shared": true,
                    "document": s.copy.document(),
                    "version": version,
                    "you": s.copy.participant(),
                    "participants": participants,
                    "invited": invited,
                    "agentFiles": s.room.agent_files().0.name(),
                })
            }
        }
    }
}

/// How long a remote invite's key works when `room.invite` doesn't say: a day.
const DEFAULT_REMOTE_EXPIRY: u64 = 24 * 60 * 60;
/// The shortest and longest `expiresIn`: a minute, and 30 days.
const MIN_EXPIRY: u64 = 60;
const MAX_EXPIRY: u64 = 30 * 24 * 60 * 60;

/// The control channel's methods for the document the window shares, which only its owner (the
/// window's key) calls; `None` for other methods.
///
/// - `room.share {name?}`: share the active document; the window joins as its owner, under
///   `name` or the author name.
/// - `room.invite {name, role?, files?, remote?, expiresIn?, uses?}`: a key for a participant
///   called `name` with `role` (viewer, commenter, suggester (the default), editor or owner), who
///   joins with it (`docs/control-protocol.md`, Shared documents). Their commands use the files
///   File › Options › Agents allows, or fewer when `files` says so: `none`, `folders` (that pane's
///   folders) or `all`, never more than the setting. `remote` (an agent on another machine): never
///   the owner, no files unless `files` names some, and the key stops working after a day unless
///   `expiresIn` says otherwise. The reply carries no connection lines: people don't run tunnels,
///   the relay (#76) will give a remote invite its URL.
///   `expiresIn`: seconds until the key stops working (60 to 30 days). `uses`: how many times it
///   joins.
/// - `room.revoke {key | name}`: take an invite back, by its key or by the name it was for (the key
///   shows once); the copies that joined with it stop answering.
/// - `room.role {name, role}`: give an invited participant another role from now on.
/// - `room.participants`: whether a document is shared, its version, who is in it, and the
///   invites not taken back (`invited`, without their keys).
/// - `room.stop`: stop sharing.
pub(crate) fn room_method(app: &mut WordApp, method: &str, p: &Value) -> Option<Result<Value, String>> {
    let s = |k: &str| p.get(k).and_then(Value::as_str);
    let room = || app.share.as_ref().map(|s| s.room.clone()).ok_or_else(|| "no document is shared: share one first (room.share)".to_string());
    Some(match method {
        "room.share" => app.share_document(s("name")),
        "room.invite" => (|| {
            let room = room()?;
            let name = s("name").ok_or("missing `name`")?;
            let role = match s("role") {
                None => Role::Suggester,
                Some(r) => Role::named(r).ok_or_else(|| format!("`role` {r:?}: viewer, commenter, suggester, editor or owner"))?,
            };
            let files = match p.get("files").filter(|v| !v.is_null()) {
                None => None,
                Some(v) => Some(v.as_str().and_then(AgentFiles::named).ok_or("`files`: none, folders or all")?),
            };
            let remote = match p.get("remote").filter(|v| !v.is_null()) {
                None => false,
                Some(v) => v.as_bool().ok_or("`remote`: true or false")?,
            };
            let expires_in = match p.get("expiresIn").filter(|v| !v.is_null()) {
                // A key that leaves this computer stops working within a day unless the owner says otherwise.
                None => remote.then_some(DEFAULT_REMOTE_EXPIRY),
                Some(v) => Some(
                    v.as_u64()
                        .filter(|n| (MIN_EXPIRY..=MAX_EXPIRY).contains(n))
                        .ok_or(format!("`expiresIn`: whole seconds from {MIN_EXPIRY} to {MAX_EXPIRY} (30 days)"))?,
                ),
            };
            let uses = match p.get("uses").filter(|v| !v.is_null()) {
                None => None,
                Some(v) => Some(v.as_u64().and_then(|n| u32::try_from(n).ok()).filter(|n| *n >= 1).ok_or("`uses`: a whole number from 1")?),
            };
            let expires = expires_in.map(|secs| wordcraft_engine::cmd::now_unix().saturating_add(secs));
            let terms = Terms { files, expires, uses, remote };
            let key = room.invite_on(name, role, terms).map_err(|e| e.to_string())?;
            // A remote invite's files are none unless it named some (`Hub::invite_on`).
            let files = if remote { Some(files.unwrap_or(AgentFiles::None)) } else { files };
            Ok(json!({
                "key": key,
                "name": name.trim(),
                "role": role,
                "files": files.map(AgentFiles::name),
                "remote": remote,
                "expires": expires,
                "uses": uses,
            }))
        })(),
        "room.revoke" => (|| {
            let room = room()?;
            match (s("key"), s("name")) {
                (Some(key), _) => Ok(json!({"revoked": room.revoke(key)})),
                (None, Some(name)) => Ok(json!({"revoked": room.revoke_named(name)})),
                (None, None) => Err("missing `key` or `name`".to_string()),
            }
        })(),
        "room.role" => (|| {
            let room = room()?;
            let name = s("name").ok_or("missing `name`")?;
            let role = s("role").ok_or("missing `role`")?;
            let role = Role::named(role).ok_or_else(|| format!("`role` {role:?}: viewer, commenter, suggester, editor or owner"))?;
            // The assistant's conversation was started with its role, told it, and keeps
            // comparing it with the role asked for: that role is the Assistant pane's to choose.
            if let Some(assistant) = app.assistant_named(name) {
                return Err(format!("{assistant} is the assistant: choose its role in the Assistant pane, which starts a new conversation with it"));
            }
            let changed = room.set_role(name, role).map_err(|e| e.to_string())?;
            Ok(json!({"changed": changed, "name": name.trim(), "role": role}))
        })(),
        "room.participants" => Ok(app.share_state()),
        "room.stop" => Ok(json!({"stopped": app.stop_sharing()})),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use wordcraft_doc::{Document, Pos, StoryRef};
    use wordcraft_engine::Session;
    use wordcraft_engine::agent_files::AgentFiles;
    use wordcraft_engine::room::{Lobby, Replica, hosted};

    use crate::control::{self, ControlRequest, Outcome};
    use crate::{Services, WordApp};

    /// A window showing "one two three", whose lobby is `lobby`.
    fn window(lobby: &Lobby) -> WordApp {
        let mut a = WordApp::new(Session::new(Document::from_text("one two three")), Services::default()).with_lobby(lobby.clone());
        a.session.author = "Minoru".into();
        a
    }

    /// A control-channel request with the window's key, and its reply.
    fn call(a: &mut WordApp, method: &str, params: Value) -> Value {
        let (req, _reply) = ControlRequest::new(method, params);
        match control::handle(a, &egui::Context::default(), &req) {
            Outcome::Done(v) => v,
            Outcome::Screenshot { .. } => panic!("{method} took a screenshot"),
        }
    }

    fn result(a: &mut WordApp, method: &str, params: Value) -> Value {
        let v = call(a, method, params);
        assert_eq!(v["ok"], true, "{method}: {v}");
        v["result"].clone()
    }

    /// Run `command` in a participant's hosted copy, as their connection does.
    fn agent_runs(copy: &mut Replica, command: &str, params: Value) -> Value {
        match hosted::call(copy, "engine.execute", &json!({"command": command, "params": params})) {
            Ok(v) => v,
            Err(e) => panic!("{command}: {e}"),
        }
    }

    fn text(a: &WordApp) -> String {
        a.session.doc.plain_text(StoryRef::Body)
    }

    /// The window holds the room's document, under the same block ids.
    fn window_is_hub(a: &WordApp) {
        let share = a.share.as_ref().expect("a shared document");
        let (_, hub) = share.room.document();
        assert!(a.session.doc == hub, "the window holds other content");
        assert_eq!(a.session.doc.block_ids(), hub.block_ids());
    }

    /// The bold characters of the first paragraph.
    fn bold(a: &WordApp) -> String {
        let Some(p) = a.session.doc.body.first().and_then(|b| b.as_para()) else { return String::new() };
        let mut out = String::new();
        let mut at = 0;
        for run in &p.runs {
            let end = at + run.len;
            if run.props.bold == Some(true) {
                out.push_str(&p.text[at..end]);
            }
            at = end;
        }
        out
    }

    /// Which files agents can use (#31): the window's File › Options › Agents reaches the agents
    /// already in the room, an invite can give its agent fewer files, and the owner sees what
    /// each agent can use.
    #[test]
    fn the_window_says_which_files_agents_can_use() {
        let lobby = Lobby::default();
        let mut a = window(&lobby);
        let dir = std::env::temp_dir().join(format!("wordcraft-window-agent-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.txt"), "from the folder").unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        result(&mut a, "room.share", json!({}));
        let follows = result(&mut a, "room.invite", json!({"name": "Researcher", "role": "editor"}));
        assert_eq!(follows["files"], Value::Null, "it follows the setting");
        let fewer = result(&mut a, "room.invite", json!({"name": "Proofreader", "role": "editor", "files": "none"}));
        assert_eq!(fewer["files"], "none");
        for junk in [json!("everything"), json!(3), json!(["all"])] {
            let e = call(&mut a, "room.invite", json!({"name": "Eve", "files": junk}));
            assert!(e["error"].as_str().unwrap().contains("none, folders or all"), "{e}");
        }
        let mut researcher = lobby.join_with(follows["key"].as_str().unwrap()).unwrap();
        let mut proofreader = lobby.join_with(fewer["key"].as_str().unwrap()).unwrap();
        let notes = dir.join("notes.txt").to_string_lossy().into_owned();
        let reads =
            |copy: &mut Replica| hosted::call(copy, "engine.execute", &json!({"command": "insert.textFromFile", "params": {"path": notes}})).is_ok();
        assert!(!reads(&mut researcher), "no file until the person says");
        // The window's own Options reach the agents already in the room.
        let folder = dir.to_string_lossy().into_owned();
        result(&mut a, "engine.execute", json!({"command": "file.options", "params": {"agentFiles": "folders", "agentFolders": [folder]}}));
        assert!(reads(&mut researcher));
        assert!(!reads(&mut proofreader), "its invite gave it none");
        let state = result(&mut a, "room.participants", json!({}));
        let files: Vec<(&str, &Value)> =
            state["participants"].as_array().unwrap().iter().map(|p| (p["name"].as_str().unwrap(), &p["files"])).collect();
        assert_eq!(files, [("Minoru", &Value::Null), ("Researcher", &json!("folders")), ("Proofreader", &json!("none"))]);
        assert_eq!(state["agentFiles"], "folders");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An invite for an agent on another machine (#74): never the owner, no files unless it
    /// names some, a day unless it says, and no tunnel line for a person to run; junk terms are
    /// refused.
    #[test]
    fn a_remote_invite_is_held_back() {
        let lobby = Lobby::default();
        lobby.set_addr(Some("127.0.0.1:43210".parse().unwrap()));
        let mut a = window(&lobby);
        result(&mut a, "room.share", json!({}));
        result(&mut a, "engine.execute", json!({"command": "file.options", "params": {"agentFiles": "all"}}));
        let now = wordcraft_engine::cmd::now_unix();
        let r = result(&mut a, "room.invite", json!({"name": "Sandbox", "remote": true}));
        assert_eq!((r["remote"].clone(), r["files"].clone(), r["role"].clone()), (json!(true), json!("none"), json!("suggester")));
        let expires = r["expires"].as_u64().unwrap();
        assert!((now + 86_400..=now + 86_402).contains(&expires), "a day: {expires}");
        let key = r["key"].as_str().unwrap();
        assert_eq!((r.get("connect"), r.get("tunnel")), (None, None), "no command for a person to run");
        let copy = lobby.join_with(key).unwrap();
        assert_eq!(a.shared_room().unwrap().files_for(copy.id()).0, AgentFiles::None, "the setting says all, a remote invite none");

        let r = result(&mut a, "room.invite", json!({"name": "Hour", "expiresIn": 3600, "uses": 1}));
        assert_eq!((r["remote"].clone(), r["uses"].clone()), (json!(false), json!(1)));
        assert!(r["expires"].as_u64().unwrap() >= now + 3600);
        let invited = result(&mut a, "room.participants", json!({}))["invited"].clone();
        let hour = invited.as_array().unwrap().iter().find(|i| i["name"] == "Hour").unwrap().clone();
        assert_eq!((hour["usesLeft"].clone(), hour["remote"].clone()), (json!(1), json!(false)));
        assert_eq!(hour.get("key"), None);

        let e = call(&mut a, "room.invite", json!({"name": "Boss", "role": "owner", "remote": true}));
        assert!(e["error"].as_str().unwrap().contains("can't be the owner"), "{e}");
        for (k, junk, says) in [
            ("expiresIn", json!(59), "expiresIn"),
            ("expiresIn", json!(2_592_001), "expiresIn"),
            ("expiresIn", json!(-1), "expiresIn"),
            ("expiresIn", json!("1h"), "expiresIn"),
            ("uses", json!(0), "uses"),
            ("uses", json!(1.5), "uses"),
            ("uses", json!(u64::MAX), "uses"),
            ("remote", json!("yes"), "remote"),
        ] {
            let e = call(&mut a, "room.invite", json!({"name": "Eve", k: junk}));
            assert!(e["error"].as_str().is_some_and(|m| m.contains(says)), "{k} {junk}: {e}");
        }
        assert!(result(&mut a, "room.participants", json!({}))["invited"].as_array().unwrap().iter().all(|i| i["name"] != "Eve"));
    }

    #[test]
    fn the_window_shares_its_document_and_shows_what_agents_change() {
        let lobby = Lobby::default();
        let mut a = window(&lobby);
        let ctx = egui::Context::default();
        assert_eq!(result(&mut a, "room.participants", json!({})), json!({"shared": false}));
        assert!(call(&mut a, "room.invite", json!({"name": "Agent"}))["error"].as_str().unwrap().contains("room.share"));
        let shared = result(&mut a, "room.share", json!({}));
        assert_eq!((shared["you"]["name"].clone(), shared["you"]["role"].clone()), (json!("Minoru"), json!("owner")));
        // Sharing it again changes nothing.
        assert_eq!(result(&mut a, "room.share", json!({}))["document"], shared["document"]);
        // A copy editor joins with a key (as a suggester, unless invited as more).
        let invite = result(&mut a, "room.invite", json!({"name": "Copy editor"}));
        assert_eq!(invite["role"], "suggester");
        let key = invite["key"].as_str().unwrap().to_string();
        assert!(call(&mut a, "room.invite", json!({"name": "Eve", "role": "admin"}))["error"].as_str().unwrap().contains("viewer, commenter"));
        let mut agent = lobby.join_with(&key).unwrap();
        // The person types while the agent suggests "2" for "two".
        a.execute("caret.docEnd", json!({})).unwrap();
        a.execute("text.insert", json!({"text": " four"})).unwrap();
        agent_runs(&mut agent, "select.text", json!({"text": "two"}));
        agent_runs(&mut agent, "text.insert", json!({"text": "2"}));
        for _ in 0..3 {
            a.sync_share(&ctx);
        }
        window_is_hub(&a);
        assert_eq!(text(&a), "one 2two three four");
        assert!(a.session.doc.revisions.iter().all(|r| r.author == "Copy editor"));
        let state = result(&mut a, "room.participants", json!({}));
        let names: Vec<&str> = state["participants"].as_array().unwrap().iter().filter_map(|p| p["name"].as_str()).collect();
        assert_eq!(names, ["Minoru", "Copy editor"]);
        // Accepting the suggestion in the window goes to the agent too.
        a.execute("review.acceptAll", json!({})).unwrap();
        let seen = hosted::call(&mut agent, "document.inspect", &json!({"text": true})).unwrap();
        assert!(seen.to_string().contains("one 2 three four"), "{seen}");
        // Revoked, the agent's copy stops answering; stopped, so does the room.
        assert_eq!(result(&mut a, "room.revoke", json!({"key": key})), json!({"revoked": true}));
        assert!(hosted::call(&mut agent, "document.inspect", &json!({})).is_err());
        assert_eq!(result(&mut a, "room.stop", json!({})), json!({"stopped": true}));
        assert!(lobby.room().is_none());
        assert_eq!(result(&mut a, "room.stop", json!({})), json!({"stopped": false}));
        assert_eq!(text(&a), "one 2 three four", "the window keeps the document");
        a.execute("text.insert", json!({"text": "!"})).unwrap();
    }

    #[test]
    fn the_person_s_edit_that_clashes_with_an_agent_s_is_made_again() {
        let lobby = Lobby::default();
        let mut a = window(&lobby);
        let ctx = egui::Context::default();
        result(&mut a, "room.share", json!({}));
        let key = result(&mut a, "room.invite", json!({"name": "Writer", "role": "editor"}))["key"].as_str().unwrap().to_string();
        let mut agent = lobby.join_with(&key).unwrap();
        // The agent types "2" over "two", and that reaches the room before the window's next
        // frame; the person bolds "two" meanwhile.
        agent_runs(&mut agent, "select.text", json!({"text": "two"}));
        agent_runs(&mut agent, "text.insert", json!({"text": "2"}));
        a.execute("select.text", json!({"text": "two"})).unwrap();
        a.execute("format.bold", json!({})).unwrap();
        // The room sent the person's bold back; their copy made it again on "2".
        assert_eq!(text(&a), "one 2 three");
        assert_eq!(bold(&a), "2");
        for _ in 0..3 {
            a.sync_share(&ctx);
        }
        window_is_hub(&a);
        let seen = hosted::call(&mut agent, "document.inspect", &json!({})).unwrap();
        assert!(seen.to_string().contains("\"bold\":true"), "{seen}");
    }

    #[test]
    fn the_person_s_undo_takes_their_own_edit_back_and_leaves_the_agent_s() {
        let lobby = Lobby::default();
        let mut a = window(&lobby);
        let ctx = egui::Context::default();
        result(&mut a, "room.share", json!({}));
        let key = result(&mut a, "room.invite", json!({"name": "Writer", "role": "editor"}))["key"].as_str().unwrap().to_string();
        let mut agent = lobby.join_with(&key).unwrap();
        // The person types at the end while the agent types "2" over "two".
        a.execute("caret.docEnd", json!({})).unwrap();
        a.execute("text.insert", json!({"text": " four"})).unwrap();
        agent_runs(&mut agent, "select.text", json!({"text": "two"}));
        agent_runs(&mut agent, "text.insert", json!({"text": "2"}));
        for _ in 0..3 {
            a.sync_share(&ctx);
        }
        assert_eq!(text(&a), "one 2 three four");
        // The person's Undo takes " four" back; the agent's "2" stays, in the window and in the
        // agent's copy.
        a.execute("edit.undo", json!({})).unwrap();
        assert_eq!(text(&a), "one 2 three");
        for _ in 0..3 {
            a.sync_share(&ctx);
        }
        window_is_hub(&a);
        let seen = hosted::call(&mut agent, "document.inspect", &json!({"text": true})).unwrap();
        assert!(seen.to_string().contains("one 2 three\""), "{seen}");
        // The agent's Undo takes its "2" back, and the person's Redo puts " four" back past it.
        agent_runs(&mut agent, "edit.undo", json!({}));
        for _ in 0..3 {
            a.sync_share(&ctx);
        }
        assert_eq!(text(&a), "one two three");
        a.execute("edit.redo", json!({})).unwrap();
        assert_eq!(text(&a), "one two three four");
        for _ in 0..3 {
            a.sync_share(&ctx);
        }
        window_is_hub(&a);
    }

    /// Presence (#32): agents see where the person's caret is while the window shows the shared
    /// document, and the window's key sees where every agent's is, whichever document it shows.
    #[test]
    fn the_window_and_agents_see_where_each_other_are() {
        let lobby = Lobby::default();
        let mut a = window(&lobby);
        let ctx = egui::Context::default();
        let shared = result(&mut a, "room.share", json!({}))["document"].as_u64().unwrap();
        let key = result(&mut a, "room.invite", json!({"name": "Ada", "role": "editor"}))["key"].as_str().unwrap().to_string();
        let mut agent = lobby.join_with(&key).unwrap();
        let where_is = |all: &Value, name: &str| -> Value {
            all["participants"].as_array().unwrap().iter().find(|p| p["name"] == name).map(|p| p["selection"].clone()).unwrap()
        };
        // The person's caret reaches the agent with the next frame.
        a.execute("caret.set", json!({"pos": Pos::body(0, 4)})).unwrap();
        a.sync_share(&ctx);
        let seen = hosted::call(&mut agent, "room.participants", &json!({})).unwrap();
        assert_eq!((where_is(&seen, "Minoru")["focus"]["off"].as_u64(), where_is(&seen, "Minoru")["story"].as_str()), (Some(4), Some("body")));
        // The agent's selection reaches the window's key, with its colour, and no text.
        agent_runs(&mut agent, "select.text", json!({"text": "three"}));
        let state = result(&mut a, "room.participants", json!({}));
        let ada = where_is(&state, "Ada");
        assert_eq!((ada["anchor"]["off"].as_u64(), ada["focus"]["off"].as_u64(), ada["index"].as_u64()), (Some(8), Some(13), Some(0)), "{state}");
        assert!(!state.to_string().contains("three"), "{state}");
        let colours: Vec<&Value> = state["participants"].as_array().unwrap().iter().map(|p| &p["color"]).collect();
        assert!(colours.iter().all(|c| c.is_u64()) && colours.first() != colours.get(1), "{state}");
        // While the window shows another document, the agents don't see the person's caret,
        // and the window's key still sees the agent's.
        a.execute("file.new", json!({})).unwrap();
        a.sync_share(&ctx);
        let seen = hosted::call(&mut agent, "room.participants", &json!({})).unwrap();
        assert_eq!(where_is(&seen, "Minoru"), Value::Null, "{seen}");
        agent_runs(&mut agent, "select.text", json!({"text": "two"}));
        let state = result(&mut a, "room.participants", json!({}));
        assert_eq!(where_is(&state, "Ada")["anchor"]["off"].as_u64(), Some(4), "{state}");
        assert_eq!(state["version"], json!(0));
        // Back on it, they do again.
        a.execute("view.switchWindows", json!({"id": shared})).unwrap();
        a.sync_share(&ctx);
        let seen = hosted::call(&mut agent, "room.participants", &json!({})).unwrap();
        assert_eq!(where_is(&seen, "Minoru")["focus"]["off"].as_u64(), Some(4), "{seen}");
        // An idle window tells the room nothing more.
        let stamp = a.share.as_ref().map(|s| s.room.presence_stamp());
        for _ in 0..10 {
            a.sync_share(&ctx);
        }
        assert_eq!(a.share.as_ref().map(|s| s.room.presence_stamp()), stamp);
    }

    #[test]
    fn the_window_catches_up_on_the_shared_document_and_stops_sharing_when_it_closes() {
        let lobby = Lobby::default();
        let mut a = window(&lobby);
        let ctx = egui::Context::default();
        let shared = result(&mut a, "room.share", json!({}))["document"].as_u64().unwrap();
        let key = result(&mut a, "room.invite", json!({"name": "Writer", "role": "editor"}))["key"].as_str().unwrap().to_string();
        let mut agent = lobby.join_with(&key).unwrap();
        // The person opens another document; only one is shared at a time.
        a.execute("file.new", json!({})).unwrap();
        assert!(call(&mut a, "room.share", json!({}))["error"].as_str().unwrap().contains("room.stop"));
        a.execute("text.insert", json!({"text": "notes"})).unwrap();
        agent_runs(&mut agent, "caret.docStart", json!({}));
        agent_runs(&mut agent, "text.insert", json!({"text": "zero "}));
        a.sync_share(&ctx);
        assert_eq!(text(&a), "notes");
        // Back on the shared document, the window has the agent's edit, and the notes stayed out.
        a.execute("view.switchWindows", json!({"id": shared})).unwrap();
        a.sync_share(&ctx);
        assert_eq!(text(&a), "zero one two three");
        window_is_hub(&a);
        // Closing it stops sharing: the agent's copy stops answering.
        a.execute("file.close", json!({"force": true})).unwrap();
        a.sync_share(&ctx);
        assert!(a.share.is_none());
        assert!(hosted::call(&mut agent, "document.inspect", &json!({})).unwrap_err().contains("isn't shared"));
    }

    #[test]
    fn stopping_or_closing_keeps_what_agents_changed() {
        let lobby = Lobby::default();
        let mut a = window(&lobby);
        let shared = result(&mut a, "room.share", json!({}))["document"].as_u64().unwrap();
        let key = result(&mut a, "room.invite", json!({"name": "Writer", "role": "editor"}))["key"].as_str().unwrap().to_string();
        let mut agent = lobby.join_with(&key).unwrap();
        // Stopped while the window shows another document, the shared one keeps the agent's edit.
        a.execute("file.new", json!({})).unwrap();
        agent_runs(&mut agent, "caret.docStart", json!({}));
        agent_runs(&mut agent, "text.insert", json!({"text": "zero "}));
        assert_eq!(result(&mut a, "room.stop", json!({})), json!({"stopped": true}));
        assert_eq!(text(&a), "");
        a.execute("view.switchWindows", json!({"id": shared})).unwrap();
        assert_eq!(text(&a), "zero one two three");
        // Shared again and saved: an agent's edit the room took since the last frame is unsaved
        // work, so a close without `force` is refused, and the person's Close asks to save.
        result(&mut a, "room.share", json!({}));
        let key = result(&mut a, "room.invite", json!({"name": "Writer", "role": "editor"}))["key"].as_str().unwrap().to_string();
        let mut agent = lobby.join_with(&key).unwrap();
        a.session.dirty = false;
        agent_runs(&mut agent, "caret.docEnd", json!({}));
        agent_runs(&mut agent, "text.insert", json!({"text": " four"}));
        let refused = call(&mut a, "file.close", json!({}));
        assert!(refused["error"].as_str().is_some_and(|e| e.contains("unsaved changes")), "{refused}");
        assert_eq!(text(&a), "zero one two three four");
        assert!(a.share.is_some(), "still shared");
        assert_eq!(a.run("file.close", json!({})).unwrap()["pending"], "saveChanges");
        a.dialog = None;
        // Closed with `force`, sharing stops; the agent's copy stops answering.
        a.execute("file.close", json!({"force": true})).unwrap();
        assert!(a.share.is_none());
        assert!(hosted::call(&mut agent, "document.inspect", &json!({})).unwrap_err().contains("isn't shared"));
    }

    /// The shared document's text, whichever document the window shows.
    fn shared_text(a: &mut WordApp, shared: u64) -> String {
        a.session.with_document(shared, |s| s.doc.plain_text(StoryRef::Body)).unwrap()
    }

    fn is_dirty(a: &WordApp, doc: u64) -> bool {
        a.session.documents().iter().any(|d| d.id == doc && d.dirty)
    }

    #[test]
    fn the_shared_document_keeps_up_while_the_window_shows_another() {
        let lobby = Lobby::default();
        let mut a = window(&lobby);
        let ctx = egui::Context::default();
        let shared = result(&mut a, "room.share", json!({}))["document"].as_u64().unwrap();
        let key = result(&mut a, "room.invite", json!({"name": "Writer", "role": "editor"}))["key"].as_str().unwrap().to_string();
        let mut agent = lobby.join_with(&key).unwrap();
        let dir = std::env::temp_dir().join(format!("wordcraft-share-keeps-up-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // The person works on another document; each frame brings the agent's edit into the
        // shared one all the same, as unsaved work.
        a.execute("file.new", json!({})).unwrap();
        a.session.with_document(shared, |s| s.dirty = false).unwrap();
        agent_runs(&mut agent, "caret.docStart", json!({}));
        agent_runs(&mut agent, "text.insert", json!({"text": "zero "}));
        a.sync_share(&ctx);
        assert_eq!(shared_text(&mut a, shared), "zero one two three");
        assert!(is_dirty(&a, shared), "the agent's edit is unsaved work");
        assert_eq!(text(&a), "", "the shown document stays as it was");
        // Requests addressed to it see what the agent changed since the last frame too: an
        // inspection, and a save.
        agent_runs(&mut agent, "caret.docEnd", json!({}));
        agent_runs(&mut agent, "text.insert", json!({"text": " four"}));
        let seen = result(&mut a, "document.inspect", json!({"doc": shared, "text": true}));
        assert!(seen.to_string().contains("zero one two three four"), "{seen}");
        agent_runs(&mut agent, "text.insert", json!({"text": " five"}));
        let path = dir.join("shared.txt");
        result(&mut a, "engine.execute", json!({"command": "file.saveAs", "params": {"path": path}, "doc": shared}));
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim_end(), "zero one two three four five");
        // Edits addressed to it go through the window's copy: the room has them before the reply.
        result(&mut a, "engine.execute", json!({"command": "caret.docEnd", "params": {}, "doc": shared}));
        result(&mut a, "engine.execute", json!({"command": "text.insert", "params": {"text": "!"}, "doc": shared}));
        let (_, hub) = a.share.as_ref().unwrap().room.document();
        assert_eq!(hub.plain_text(StoryRef::Body), "zero one two three four five!");
        assert_eq!(text(&a), "");
        // Saved, then edited by the agent: closing it without `force` is refused, and sharing
        // goes on. With `force` it closes, and sharing stops before it does.
        a.session.with_document(shared, |s| s.dirty = false).unwrap();
        agent_runs(&mut agent, "text.insert", json!({"text": "?"}));
        let refused = call(&mut a, "engine.execute", json!({"command": "file.close", "params": {}, "doc": shared}));
        assert!(refused["error"].as_str().is_some_and(|e| e.contains("unsaved changes")), "{refused}");
        assert!(a.share.is_some() && a.session.is_open(shared));
        result(&mut a, "engine.execute", json!({"command": "file.close", "params": {"force": true}, "doc": shared}));
        assert!(a.share.is_none(), "sharing stopped with the close");
        assert!(!a.session.is_open(shared));
        assert!(hosted::call(&mut agent, "document.inspect", &json!({})).unwrap_err().contains("isn't shared"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn closing_the_shared_document_by_id_sees_and_stops_what_agents_change() {
        let lobby = Lobby::default();
        let mut a = window(&lobby);
        let shared = result(&mut a, "room.share", json!({}))["document"].as_u64().unwrap();
        let key = result(&mut a, "room.invite", json!({"name": "Writer", "role": "editor"}))["key"].as_str().unwrap().to_string();
        let mut agent = lobby.join_with(&key).unwrap();
        a.execute("file.new", json!({})).unwrap();
        a.session.with_document(shared, |s| s.dirty = false).unwrap();
        // The agent's edit since the last frame is unsaved work: Close Document refuses it.
        agent_runs(&mut agent, "text.insert", json!({"text": "zero "}));
        let refused = call(&mut a, "file.closeDocument", json!({"id": shared}));
        assert!(refused["error"].as_str().is_some_and(|e| e.contains("unsaved changes")), "{refused}");
        assert!(a.share.is_some() && a.session.is_open(shared));
        // With `force` it closes, and sharing stops before it does.
        result(&mut a, "file.closeDocument", json!({"id": shared, "force": true}));
        assert!(a.share.is_none() && !a.session.is_open(shared));
        assert!(hosted::call(&mut agent, "document.inspect", &json!({})).unwrap_err().contains("isn't shared"));
        // So does Close Document addressed to the shared document with `doc`.
        let shared = result(&mut a, "room.share", json!({}))["document"].as_u64().unwrap();
        let key = result(&mut a, "room.invite", json!({"name": "Writer", "role": "editor"}))["key"].as_str().unwrap().to_string();
        let mut agent = lobby.join_with(&key).unwrap();
        a.execute("file.new", json!({})).unwrap();
        a.session.with_document(shared, |s| s.dirty = false).unwrap();
        agent_runs(&mut agent, "text.insert", json!({"text": "one "}));
        let close = |force: bool| json!({"command": "file.closeDocument", "params": {"force": force}, "doc": shared});
        let refused = call(&mut a, "engine.execute", close(false));
        assert!(refused["error"].as_str().is_some_and(|e| e.contains("unsaved changes")), "{refused}");
        assert!(a.share.is_some() && a.session.is_open(shared));
        result(&mut a, "engine.execute", close(true));
        assert!(a.share.is_none() && !a.session.is_open(shared));
        assert!(hosted::call(&mut agent, "document.inspect", &json!({})).unwrap_err().contains("isn't shared"));
    }

    #[test]
    fn quitting_asks_about_what_agents_change_and_stops_sharing() {
        let lobby = Lobby::default();
        let mut a = window(&lobby);
        result(&mut a, "room.share", json!({}));
        let key = result(&mut a, "room.invite", json!({"name": "Writer", "role": "editor"}))["key"].as_str().unwrap().to_string();
        let mut agent = lobby.join_with(&key).unwrap();
        // Saved, then edited by the agent since the last frame: quitting asks to save it, and
        // sharing goes on while it asks.
        a.session.dirty = false;
        agent_runs(&mut agent, "caret.docEnd", json!({}));
        agent_runs(&mut agent, "text.insert", json!({"text": " four"}));
        assert!(!a.close_requested());
        assert_eq!(a.dialog.as_ref().map(|d| d.name()), Some("saveChanges"));
        assert_eq!(text(&a), "one two three four");
        a.run("ui.saveChanges", json!({"answer": "cancel"})).unwrap();
        assert!(a.share.is_some() && !a.quit_requested);
        // With nothing unsaved, sharing stops before the app quits: the agent's copy stops
        // answering.
        a.session.dirty = false;
        assert!(a.close_requested());
        assert!(a.share.is_none());
        assert!(hosted::call(&mut agent, "document.inspect", &json!({})).unwrap_err().contains("isn't shared"));
    }

    #[test]
    fn quitting_after_asking_about_another_document_stops_sharing() {
        let lobby = Lobby::default();
        let mut a = window(&lobby);
        let shared = result(&mut a, "room.share", json!({}))["document"].as_u64().unwrap();
        let key = result(&mut a, "room.invite", json!({"name": "Writer", "role": "editor"}))["key"].as_str().unwrap().to_string();
        let mut agent = lobby.join_with(&key).unwrap();
        a.session.dirty = false;
        // Another document has unsaved work; the shared one has none.
        a.execute("file.new", json!({})).unwrap();
        a.execute("text.insert", json!({"text": "notes"})).unwrap();
        assert!(!a.close_requested());
        assert!(a.share.is_some(), "sharing goes on while the app asks");
        // Don't Save: nothing is left to ask about, so sharing stops and the app quits.
        a.run("ui.saveChanges", json!({"answer": "dontSave"})).unwrap();
        assert!(a.quit_requested);
        assert!(a.share.is_none());
        assert!(a.session.is_open(shared));
        assert!(hosted::call(&mut agent, "document.inspect", &json!({})).unwrap_err().contains("isn't shared"));
    }

    #[test]
    fn keeping_the_shared_document_up_to_date_in_the_background_leaves_the_input_method_alone() {
        let lobby = Lobby::default();
        let mut a = window(&lobby);
        let ctx = egui::Context::default();
        let shared = result(&mut a, "room.share", json!({}))["document"].as_u64().unwrap();
        let key = result(&mut a, "room.invite", json!({"name": "Writer", "role": "editor"}))["key"].as_str().unwrap().to_string();
        let mut agent = lobby.join_with(&key).unwrap();
        // The person types in another document, and the input method has looked since.
        a.execute("file.new", json!({})).unwrap();
        a.execute("text.insert", json!({"text": "notes"})).unwrap();
        let _ = a.session.take_pending_ime_application_change();
        let revision = a.session.ime_state_revision();
        // The agent's edits come into the shared document with a frame and before a control
        // request: nothing the input method is told of.
        agent_runs(&mut agent, "caret.docEnd", json!({}));
        agent_runs(&mut agent, "text.insert", json!({"text": " four"}));
        a.sync_share(&ctx);
        agent_runs(&mut agent, "text.insert", json!({"text": " five"}));
        result(&mut a, "room.participants", json!({}));
        assert_eq!(shared_text(&mut a, shared), "one two three four five");
        assert!(!a.session.take_pending_ime_application_change(), "the agent's edits aren't in the document typed in");
        assert_eq!(a.session.ime_state_revision(), revision);
        // While the window shows the shared document, the input method hears of them.
        a.show_document(shared).unwrap();
        let _ = a.session.take_pending_ime_application_change();
        agent_runs(&mut agent, "text.insert", json!({"text": " six"}));
        a.sync_share(&ctx);
        assert!(a.session.take_pending_ime_application_change());
        assert_eq!(text(&a), "one two three four five six");
    }

    #[test]
    fn a_file_opened_over_a_blank_shared_document_opens_beside_it() {
        let lobby = Lobby::default();
        let mut a = WordApp::new(Session::new(Document::new()), Services::default()).with_lobby(lobby.clone());
        let ctx = egui::Context::default();
        let shared = result(&mut a, "room.share", json!({}))["document"].as_u64().unwrap();
        let key = result(&mut a, "room.invite", json!({"name": "Writer", "role": "editor"}))["key"].as_str().unwrap().to_string();
        let mut agent = lobby.join_with(&key).unwrap();
        let dir = std::env::temp_dir().join(format!("wordcraft-share-open-over-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("other.txt");
        std::fs::write(&file, "Other").unwrap();
        // The shared document stays open, and shared, beside the file.
        result(&mut a, "engine.execute", json!({"command": "file.open", "params": {"path": file}}));
        assert_eq!(text(&a), "Other");
        assert!(a.session.is_open(shared));
        assert!(a.share.is_some(), "sharing goes on");
        agent_runs(&mut agent, "text.insert", json!({"text": "kept"}));
        a.sync_share(&ctx);
        assert_eq!(shared_text(&mut a, shared), "kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quitting_over_the_control_channel_stops_sharing_first() {
        let lobby = Lobby::default();
        let mut a = window(&lobby);
        result(&mut a, "room.share", json!({}));
        let key = result(&mut a, "room.invite", json!({"name": "Writer", "role": "editor"}))["key"].as_str().unwrap().to_string();
        let mut agent = lobby.join_with(&key).unwrap();
        agent_runs(&mut agent, "caret.docEnd", json!({}));
        agent_runs(&mut agent, "text.insert", json!({"text": " four"}));
        // The agent's edit is unsaved work in the window (every control request first gets what
        // the room took), so `app.quit` without `force` refuses, as with any unsaved change
        // (#3274), and the room stays open.
        let refused = call(&mut a, "app.quit", json!({}));
        assert_eq!(refused["ok"], false, "{refused}");
        assert!(refused["error"].as_str().unwrap().contains("unsaved"), "{refused}");
        assert!(!a.quit_requested);
        assert!(a.share.is_some());
        assert_eq!(agent_runs(&mut agent, "document.inspect", json!({}))["blocks"][0]["text"], "one two three four");
        // With `force` it quits, and sharing stops first: the agent's copy stops answering.
        result(&mut a, "app.quit", json!({"force": true}));
        assert!(a.quit_requested);
        assert!(a.share.is_none());
        assert_eq!(text(&a), "one two three four");
        assert!(hosted::call(&mut agent, "document.inspect", &json!({})).unwrap_err().contains("isn't shared"));
    }

    #[test]
    fn a_macro_s_close_closes_the_shown_document_and_never_quits() {
        let lobby = Lobby::default();
        let mut a = window(&lobby);
        let shared = result(&mut a, "room.share", json!({}))["document"].as_u64().unwrap();
        let key = result(&mut a, "room.invite", json!({"name": "Writer", "role": "editor"}))["key"].as_str().unwrap().to_string();
        let mut agent = lobby.join_with(&key).unwrap();
        a.execute("tools.macros", json!({"define": {"name": "close", "steps": [{"command": "file.close"}]}})).unwrap();
        // The window shows another document, with unsaved work: the macro's Close keeps it open
        // and says why.
        a.execute("file.new", json!({})).unwrap();
        a.execute("text.insert", json!({"text": "draft"})).unwrap();
        let draft = a.session.document_id();
        a.run("tools.macros", json!({"run": "close"})).unwrap();
        assert!(!a.quit_requested && a.session.is_open(draft));
        assert!(a.status_msg.as_ref().is_some_and(|(m, _)| m.contains("unsaved changes")), "{:?}", a.status_msg);
        // Without it, the document closes; the app and the shared document stay.
        a.session.dirty = false;
        a.run("tools.macros", json!({"run": "close"})).unwrap();
        assert!(!a.quit_requested && !a.session.is_open(draft));
        assert!(a.share.is_some() && a.session.is_open(shared));
        agent_runs(&mut agent, "text.insert", json!({"text": "zero "}));
        // In the shared document, the macro runs in the window's copy, which closes nothing.
        assert_eq!(a.session.document_id(), shared);
        assert!(a.run("tools.macros", json!({"run": "close"})).is_err());
        assert!(!a.quit_requested && a.share.is_some() && a.session.is_open(shared));
        a.catch_up_share();
        assert_eq!(text(&a), "zero one two three");
    }

    #[test]
    fn a_macro_can_t_close_the_shared_document_while_it_is_shared() {
        let lobby = Lobby::default();
        let mut a = window(&lobby);
        let shared = result(&mut a, "room.share", json!({}))["document"].as_u64().unwrap();
        let key = result(&mut a, "room.invite", json!({"name": "Writer", "role": "editor"}))["key"].as_str().unwrap().to_string();
        let mut agent = lobby.join_with(&key).unwrap();
        a.execute("file.new", json!({})).unwrap();
        let step = json!({"command": "file.closeDocument", "params": {"id": shared, "force": true}});
        a.execute("tools.macros", json!({"define": {"name": "close", "steps": [step]}})).unwrap();
        // An agent's edit since the window's last frame: closing would drop it, and what the
        // room took after.
        agent_runs(&mut agent, "text.insert", json!({"text": "zero "}));
        let why = a.run("tools.macros", json!({"run": "close"})).unwrap_err();
        assert!(why.contains("stop sharing it first"), "{why}");
        assert!(a.share.is_some() && a.session.is_open(shared));
        a.catch_up_share();
        assert_eq!(shared_text(&mut a, shared), "zero one two three");
        agent_runs(&mut agent, "text.insert", json!({"text": "and "}));
        // Once sharing stops, the macro closes it.
        result(&mut a, "room.stop", json!({}));
        a.run("tools.macros", json!({"run": "close"})).unwrap();
        assert!(!a.session.is_open(shared));
    }

    #[test]
    fn a_close_made_for_the_shared_document_in_the_background_stops_sharing_first() {
        let lobby = Lobby::default();
        let mut a = window(&lobby);
        let shared = result(&mut a, "room.share", json!({}))["document"].as_u64().unwrap();
        let key = result(&mut a, "room.invite", json!({"name": "Writer", "role": "editor"}))["key"].as_str().unwrap().to_string();
        let mut agent = lobby.join_with(&key).unwrap();
        a.execute("file.new", json!({})).unwrap();
        a.session.with_document(shared, |s| s.dirty = false).unwrap();
        // Close, as a command run on the shared document while another is shown asks for it.
        // An agent's edit since the last frame is unsaved work: the document stays, shared.
        agent_runs(&mut agent, "text.insert", json!({"text": "zero "}));
        a.handle_request(&json!({"close": true, "document": shared}));
        assert!(a.share.is_some() && a.session.is_open(shared));
        assert_eq!(shared_text(&mut a, shared), "zero one two three");
        // With nothing unsaved, sharing stops before it closes.
        a.session.with_document(shared, |s| s.dirty = false).unwrap();
        a.handle_request(&json!({"close": true, "document": shared}));
        assert!(a.share.is_none() && !a.session.is_open(shared));
        assert!(hosted::call(&mut agent, "document.inspect", &json!({})).unwrap_err().contains("isn't shared"));
    }

    #[test]
    fn frames_bring_in_what_agents_change() {
        let lobby = Lobby::default();
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1200.0, 800.0)).build_ui_state(
            |ui, app: &mut WordApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            window(&lobby),
        );
        h.run_steps(2);
        result(h.state_mut(), "room.share", json!({}));
        let key = result(h.state_mut(), "room.invite", json!({"name": "Writer", "role": "editor"}))["key"].as_str().unwrap().to_string();
        // The agent edits on a thread of its own, as on its connection.
        let lobby_on = lobby.clone();
        std::thread::spawn(move || {
            let mut agent = lobby_on.join_with(&key).unwrap();
            agent_runs(&mut agent, "caret.docEnd", json!({}));
            agent_runs(&mut agent, "text.insert", json!({"text": " four"}));
        })
        .join()
        .unwrap();
        h.run_steps(2);
        assert_eq!(text(h.state()), "one two three four");
        window_is_hub(h.state());
    }
}

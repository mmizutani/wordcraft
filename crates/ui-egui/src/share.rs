//! Sharing the document the window shows ([`wordcraft_engine::room`]): the window's session is
//! its owner's copy, lent to it for each step, so what the person types goes through the room's
//! hub like anyone's edit and comes back made again, not lost, when it clashes with an agent's.
//! Participants with a key join through the control channel ([`Lobby`]), each with a copy of
//! their own on their connection's thread; their changes arrive here every frame.
//!
//! The control channel's `room.*` methods are the owner's ([`room_method`]); File › Share and
//! the participants' carets and names come with presence (milestone A1.5).

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use wordcraft_engine::CmdError;
use wordcraft_engine::room::{Lobby, Replica, Role, Room};
use wordcraft_engine::scope::Scope;

use crate::WordApp;

/// The document the window shares: its room, and the owner's copy, which works on the window's
/// session ([`Replica::lend`]).
pub(crate) struct Share {
    room: Arc<Room>,
    copy: Replica,
}

/// The name the owner joins under when the window has no author name.
const OWNER: &str = "Owner";

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
        let share = self.share.as_mut()?;
        let copy_s = Scope::of(id).is_some_and(|s| Role::Owner.allows(s)) && self.session.registry.get(id).is_some();
        share
            .copy
            .lend(&mut self.session, |c| {
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

    /// Every frame: the shared document's copy gets what others changed and sends what the
    /// window changed. Sharing stops when the window no longer has the document open.
    pub(crate) fn sync_share(&mut self, ctx: &egui::Context) {
        let Some(share) = self.share.as_mut() else { return };
        if share.room.is_closed() || !self.session.is_open(share.copy.document()) {
            self.stop_sharing();
            return;
        }
        let _ = share.copy.lend(&mut self.session, Replica::sync);
        // Its own come back with the next change the room takes (which asks for a frame); a
        // change sent back is sent again a little later.
        if share.copy.has_unconfirmed() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    /// Share the active document under `name` (the author name when `None`): the window joins
    /// its room as the owner, and participants with a key it hands out ([`room_method`],
    /// `room.invite`) join through the lobby.
    fn share_document(&mut self, name: Option<&str>) -> Result<Value, String> {
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
    /// keeps the document as it is. Whether it shared one.
    pub(crate) fn stop_sharing(&mut self) -> bool {
        let Some(mut share) = self.share.take() else { return false };
        // What the room took last is the window's too.
        let _ = share.copy.lend(&mut self.session, Replica::receive);
        share.room.close();
        self.lobby.close();
        true
    }

    /// Whether the window shares a document, which one, its version, who the window is in it and
    /// who else is.
    fn share_state(&self) -> Value {
        match &self.share {
            None => json!({"shared": false}),
            Some(s) => json!({
                "shared": true,
                "document": s.copy.document(),
                "version": s.room.version(),
                "you": s.copy.participant(),
                "participants": s.room.participants(),
            }),
        }
    }
}

/// The control channel's methods for the document the window shares, which only its owner (the
/// window's key) calls; `None` for other methods.
///
/// - `room.share {name?}`: share the active document; the window joins as its owner, under
///   `name` or the author name.
/// - `room.invite {name, role?}`: a key for a participant called `name` with `role` (viewer,
///   commenter, suggester (the default), editor or owner), who joins with it
///   (`docs/control-protocol.md`, Shared documents).
/// - `room.revoke {key}`: take an invite back; the copies that joined with it stop answering.
/// - `room.participants`: whether a document is shared, its version, and who is in it.
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
            let key = room.invite(name, role).map_err(|e| e.to_string())?;
            Ok(json!({"key": key, "name": name.trim(), "role": role}))
        })(),
        "room.revoke" => (|| {
            let room = room()?;
            let key = s("key").ok_or("missing `key`")?;
            Ok(json!({"revoked": room.revoke(key)}))
        })(),
        "room.participants" => Ok(app.share_state()),
        "room.stop" => Ok(json!({"stopped": app.stop_sharing()})),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use wordcraft_doc::{Document, StoryRef};
    use wordcraft_engine::Session;
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

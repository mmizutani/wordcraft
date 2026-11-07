//! Which files the agents in a shared document can use (#31): the window's setting (File ›
//! Options › Agents, `file.options {agentFiles, agentFolders}`), an invite's own choice, and what
//! a participant's requests reach with them ([`Room::reach`], [`hosted::call`]).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use wordcraft_doc::Document;

use crate::Session;
use crate::agent_files::{AgentFiles, AgentFolder};
use crate::room::{Lobby, Replica, Role, Room, hosted};

/// A fresh folder holding `read` (agents read it), `write` (they read and write it) and
/// `outside`, each with a recipients list naming its folder.
struct Folders(PathBuf);

impl Folders {
    fn new(name: &str) -> Self {
        let base = std::env::temp_dir().join(format!("wordcraft-agent-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for d in ["read", "write", "outside"] {
            std::fs::create_dir_all(base.join(d)).unwrap();
            std::fs::write(base.join(d).join("list.csv"), format!("Name,Where\nAda,{d}-row\n")).unwrap();
        }
        Folders(std::fs::canonicalize(base).unwrap())
    }

    fn path(&self, rel: &str) -> String {
        self.0.join(rel).to_string_lossy().into_owned()
    }

    /// `file.options` setting the folders: `read` read-only, `write` read and write.
    fn setting(&self, files: &str) -> Value {
        json!({"agentFiles": files, "agentFolders": [{"path": self.path("read")}, {"path": self.path("write"), "write": true}]})
    }
}

impl Drop for Folders {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn shared(setting: Option<Value>) -> (Session, Replica, std::sync::Arc<Room>, Lobby) {
    let mut win = Session::new(Document::from_text("one two three"));
    win.author = "Minoru".into();
    if let Some(v) = setting {
        win.run("file.options", &v).unwrap();
    }
    let (room, own) = Room::host(&mut win, "Minoru").unwrap();
    let lobby = Lobby::default();
    lobby.open(room.clone());
    (win, own, room, lobby)
}

fn agent(room: &Room, lobby: &Lobby, name: &str, role: Role, files: Option<AgentFiles>) -> Replica {
    let key = room.invite_with(name, role, files).unwrap();
    lobby.join_with(&key).unwrap()
}

fn execute(copy: &mut Replica, command: &str, params: Value) -> Result<Value, String> {
    hosted::call(copy, "engine.execute", &json!({"command": command, "params": params}))
}

/// Whether `copy` read the recipients list in `folder`: its row reaches them.
fn reads(copy: &mut Replica, f: &Folders, folder: &str) -> Result<(), String> {
    execute(copy, "mailings.recipients", json!({"path": f.path(&format!("{folder}/list.csv"))}))?;
    let rows = execute(copy, "mailings.editRecipients", json!({}))?;
    if rows.to_string().contains(&format!("{folder}-row")) { Ok(()) } else { Err(format!("read nothing of {folder}: {rows}")) }
}

/// Whether `copy` exported a PDF to `rel`; the file is there when it did.
fn exports(copy: &mut Replica, f: &Folders, rel: &str) -> Result<(), String> {
    let path = f.path(rel);
    execute(copy, "file.exportPdf", json!({"path": path}))?;
    if Path::new(&path).is_file() { Ok(()) } else { Err(format!("no file at {path}")) }
}

/// Run `id` in the window as the app does: through its copy, lent the window's session.
fn in_window(own: &mut Replica, win: &mut Session, id: &str, v: Value) -> Value {
    match own.lend(win, |c| c.run(id, &v)) {
        Some(Ok(out)) => out,
        other => panic!("{id} {v}: {other:?}"),
    }
}

#[test]
fn an_agent_reaches_only_the_files_the_setting_allows() {
    let f = Folders::new("setting");
    let (mut win, mut own, room, lobby) = shared(Some(f.setting("folders")));
    let mut ed = agent(&room, &lobby, "Editor agent", Role::Editor, None);
    reads(&mut ed, &f, "read").unwrap();
    reads(&mut ed, &f, "write").unwrap();
    let e = reads(&mut ed, &f, "outside").unwrap_err();
    assert!(e.contains("outside the read roots"), "{e}");
    exports(&mut ed, &f, "write/out.pdf").unwrap();
    let e = exports(&mut ed, &f, "read/out.pdf").unwrap_err();
    assert!(e.contains("outside the write root"), "a read-only folder isn't written: {e}");
    assert!(exports(&mut ed, &f, "outside/out.pdf").is_err());
    let sep = std::path::MAIN_SEPARATOR;
    assert!(exports(&mut ed, &f, &format!("write{sep}..{sep}outside{sep}up.pdf")).is_err(), "`..` out of a folder");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(f.0.join("outside"), f.0.join("write/away")).unwrap();
        assert!(reads(&mut ed, &f, "write/away").is_err(), "a link out of a folder");
        assert!(exports(&mut ed, &f, "write/away/out.pdf").is_err());
    }
    // A changed setting holds from the agent's next request on.
    in_window(&mut own, &mut win, "file.options", json!({"agentFiles": "none"}));
    let e = reads(&mut ed, &f, "read").unwrap_err();
    assert!(e.contains("read authority is absent"), "{e}");
    assert!(exports(&mut ed, &f, "write/none.pdf").is_err());
    in_window(&mut own, &mut win, "file.options", json!({"agentFiles": "all"}));
    reads(&mut ed, &f, "outside").unwrap();
    exports(&mut ed, &f, "outside/all.pdf").unwrap();
    // The folders stay while another choice is made, and come back with it.
    in_window(&mut own, &mut win, "file.options", json!({"agentFiles": "folders"}));
    assert!(reads(&mut ed, &f, "outside").is_err());
    reads(&mut ed, &f, "read").unwrap();
    // A changed list too.
    in_window(&mut own, &mut win, "file.options", json!({"agentFolders": [f.path("outside")]}));
    reads(&mut ed, &f, "outside").unwrap();
    assert!(reads(&mut ed, &f, "read").is_err());
    assert!(exports(&mut ed, &f, "outside/ro.pdf").is_err(), "given as a path alone, a folder is read-only");
    // The person at the window reaches everything as before.
    win.run("mailings.recipients", &json!({"path": f.path("outside/list.csv")})).unwrap();
}

#[test]
fn the_files_agents_can_use_are_none_until_the_window_says() {
    let f = Folders::new("default");
    let (_win, _own, room, lobby) = shared(None);
    assert_eq!(room.agent_files(), (AgentFiles::None, vec![]));
    for role in [Role::Viewer, Role::Editor, Role::Owner] {
        let mut a = agent(&room, &lobby, &format!("Agent {role}"), role, None);
        let e = reads(&mut a, &f, "read").unwrap_err();
        assert!(e.contains("not granted"), "{role}: {e}");
        assert!(exports(&mut a, &f, "write/out.pdf").unwrap_err().contains("not granted"), "{role}");
    }
    // Sharing takes the setting as it is then.
    let (_win, _own, room, _) = shared(Some(f.setting("folders")));
    let (files, folders) = room.agent_files();
    assert_eq!(files, AgentFiles::Folders);
    assert_eq!(folders, [AgentFolder { path: f.path("read"), write: false }, AgentFolder { path: f.path("write"), write: true }]);
}

#[test]
fn an_invite_can_give_its_agent_fewer_files_never_more() {
    let f = Folders::new("invite");
    let (mut win, mut own, room, lobby) = shared(Some(f.setting("folders")));
    let mut follows = agent(&room, &lobby, "Follower", Role::Editor, None);
    let mut nothing = agent(&room, &lobby, "Shut out", Role::Editor, Some(AgentFiles::None));
    let mut folders = agent(&room, &lobby, "Kept in", Role::Editor, Some(AgentFiles::Folders));
    let mut anything = agent(&room, &lobby, "Free", Role::Viewer, Some(AgentFiles::All));
    reads(&mut follows, &f, "read").unwrap();
    assert!(reads(&mut nothing, &f, "read").is_err(), "less than the setting");
    reads(&mut folders, &f, "read").unwrap();
    reads(&mut anything, &f, "read").unwrap();
    assert!(reads(&mut anything, &f, "outside").is_err(), "never more than the setting");
    // Widened, the setting gives each what its invite chose, and those who follow it all of it.
    in_window(&mut own, &mut win, "file.options", json!({"agentFiles": "all"}));
    reads(&mut follows, &f, "outside").unwrap();
    exports(&mut anything, &f, "outside/free.pdf").unwrap();
    assert!(reads(&mut nothing, &f, "outside").is_err());
    assert!(reads(&mut folders, &f, "outside").is_err());
    reads(&mut folders, &f, "read").unwrap();
    // None stops every agent, whatever its invite chose.
    in_window(&mut own, &mut win, "file.options", json!({"agentFiles": "none"}));
    for copy in [&mut follows, &mut nothing, &mut folders, &mut anything] {
        assert!(reads(copy, &f, "read").is_err());
    }
    // Each sees its own reach; nothing of anyone's shows to the others.
    in_window(&mut own, &mut win, "file.options", json!({"agentFiles": "folders"}));
    let mine = hosted::call(&mut folders, "room.participants", &json!({})).unwrap();
    assert_eq!(mine["files"], "folders");
    assert_eq!(mine["folders"], json!([{"path": f.path("read"), "write": false}, {"path": f.path("write"), "write": true}]));
    let theirs = hosted::call(&mut nothing, "room.participants", &json!({})).unwrap();
    assert_eq!((theirs["files"].as_str(), theirs.get("folders")), (Some("none"), None));
    let others = mine["participants"].to_string();
    assert!(!others.contains(&*f.0.to_string_lossy()) && !others.contains("files"), "{others}");
}

#[test]
fn a_request_running_when_the_reach_narrows_reaches_no_file_from_then_on() {
    let f = Folders::new("withdrawn");
    let (mut win, mut own, room, lobby) = shared(Some(f.setting("folders")));
    let ed = agent(&room, &lobby, "Editor agent", Role::Editor, None);
    let list = f.path("read/list.csv");
    let reads_now = |reach: &crate::agent_files::Reach| reach.confine(|| crate::file_access::check_read(&list));
    // The reach a request took as it started, as a long macro holds it.
    let running = room.reach(ed.id());
    reads_now(&running).unwrap();
    in_window(&mut own, &mut win, "file.options", json!({"agentFiles": "all"}));
    reads_now(&running).unwrap();
    // Narrowing withdraws it: the choice, ...
    let running = room.reach(ed.id());
    in_window(&mut own, &mut win, "file.options", json!({"agentFiles": "folders"}));
    let e = reads_now(&running).unwrap_err();
    assert!(e.contains("changed while this request ran"), "{e}");
    reads_now(&room.reach(ed.id())).unwrap();
    // ... a folder no longer written, ...
    let running = room.reach(ed.id());
    in_window(&mut own, &mut win, "file.options", json!({"agentFolders": [f.path("read"), f.path("write")]}));
    assert!(reads_now(&running).is_err());
    // ... an invite taken back, and the room closing.
    let key = room.invite("Second", Role::Editor).unwrap();
    let second = lobby.join_with(&key).unwrap();
    let running = room.reach(second.id());
    reads_now(&running).unwrap();
    assert!(room.revoke(&key));
    assert!(reads_now(&running).is_err());
    let running = room.reach(ed.id());
    room.close();
    assert!(reads_now(&running).is_err());
}

#[test]
fn no_agent_changes_the_options() {
    let f = Folders::new("options");
    let (_win, _own, room, lobby) = shared(Some(f.setting("folders")));
    let mut owner = agent(&room, &lobby, "Owner agent", Role::Owner, None);
    let before = room.agent_files();
    let e = execute(&mut owner, "file.options", json!({"agentFiles": "all"})).unwrap_err();
    assert!(e.contains("set at the window"), "{e}");
    assert!(hosted::call(&mut owner, "file.options", &json!({"agentFolders": [f.path("outside")]})).is_err(), "as a method too");
    // Nor as a macro's step.
    let steps = json!([{"command": "file.options", "params": {"agentFiles": "all"}}]);
    execute(&mut owner, "tools.macros", json!({"define": {"name": "widen", "steps": steps}})).unwrap();
    let e = execute(&mut owner, "tools.macros", json!({"run": "widen"})).unwrap_err();
    assert!(e.contains("set at the window"), "{e}");
    assert_eq!(room.agent_files(), before);
    assert!(reads(&mut owner, &f, "outside").is_err());
    // The application's settings an owner changes reach no file, even with any file allowed:
    // fonts and language packs are the person's to install.
    room.set_agent_files(AgentFiles::All, vec![]);
    reads(&mut owner, &f, "outside").unwrap();
    let e = execute(&mut owner, "font.add", json!({"path": f.path("read/list.csv")})).unwrap_err();
    assert!(e.contains("automation"), "refused before the file is read: {e}");
    // The commands an agent lists say so; exporting is every role's.
    let mut viewer = agent(&room, &lobby, "Viewer agent", Role::Viewer, None);
    for copy in [&mut owner, &mut viewer] {
        let commands = hosted::call(copy, "engine.commands", &json!({})).unwrap();
        let enabled = |id: &str| commands.as_array().and_then(|a| a.iter().find(|c| c["id"] == id)).map(|c| c["enabled"].clone());
        assert_eq!(enabled("file.options"), Some(json!(false)));
        assert_eq!(enabled("file.exportPdf"), Some(json!(true)));
        assert_eq!(enabled("file.save"), Some(json!(false)), "saving stamps the document: no copy's");
    }
}

#[test]
fn every_role_exports_where_it_may_write_and_saves_nowhere() {
    let f = Folders::new("exports");
    let (_win, _own, room, lobby) = shared(Some(f.setting("folders")));
    let mut viewer = agent(&room, &lobby, "Viewer agent", Role::Viewer, None);
    exports(&mut viewer, &f, "write/viewer.pdf").unwrap();
    // A new file only: never one that is there, which a viewer's role says they don't change.
    let made = std::fs::read(f.path("write/viewer.pdf")).unwrap();
    let e = exports(&mut viewer, &f, "write/viewer.pdf").unwrap_err();
    assert!(e.contains("is there already"), "{e}");
    assert_eq!(std::fs::read(f.path("write/viewer.pdf")).unwrap(), made);
    for (id, name) in [("file.exportPdf", "theirs.pdf"), ("file.exportPng", "theirs.png"), ("file.printPdf", "printed.pdf")] {
        let theirs = f.path(&format!("write/{name}"));
        std::fs::write(&theirs, "the person's").unwrap();
        let e = execute(&mut viewer, id, json!({"path": theirs})).unwrap_err();
        assert!(e.contains("is there already"), "{id}: {e}");
        assert_eq!(std::fs::read_to_string(&theirs).unwrap(), "the person's", "{id}");
    }
    // An export's name ends in its format's: `notes.txt` becomes the new `notes.txt.pdf`.
    std::fs::write(f.path("write/notes.txt"), "the person's").unwrap();
    assert_eq!(execute(&mut viewer, "file.exportPdf", json!({"path": f.path("write/notes.txt")})).unwrap()["path"], f.path("write/notes.txt.pdf"));
    assert_eq!(std::fs::read_to_string(f.path("write/notes.txt")).unwrap(), "the person's");
    let png = f.path("write/page.png");
    execute(&mut viewer, "file.exportPng", json!({"path": png, "page": 1, "scale": 0.5})).unwrap();
    assert!(Path::new(&png).is_file());
    let print = f.path("write/print.pdf");
    execute(&mut viewer, "file.printPdf", json!({"path": print})).unwrap();
    assert!(Path::new(&print).is_file());
    // Without a path the PDF comes back in the reply (#74); printing still needs one.
    assert!(execute(&mut viewer, "file.exportPdf", json!({})).unwrap()["pdf"].is_string());
    let e = execute(&mut viewer, "file.printPdf", json!({})).unwrap_err();
    assert!(e.contains("needs a `path`"), "{e}");
    for (id, params) in [
        ("file.saveAs", json!({"path": f.path("write/copy.docx")})),
        ("file.save", json!({})),
        ("file.open", json!({"path": f.path("read/list.csv")})),
    ] {
        let e = execute(&mut viewer, id, params).unwrap_err();
        assert!(e.contains("aren't part of a shared document"), "{id}: {e}");
    }
    assert!(!Path::new(&f.path("write/copy.docx")).exists());
    // Exporting changes nothing in the document.
    assert_eq!(room.version(), 0);
}

#[test]
fn file_options_sets_the_files_agents_can_use() {
    let f = Folders::new("options-setting");
    let mut s = Session::new(Document::new());
    let sep = std::path::MAIN_SEPARATOR;
    // Folders are kept with their links followed, so `..` can't stand for another one.
    let r = s
        .run("file.options", &json!({"agentFiles": "folders", "agentFolders": [format!("{}{sep}..{sep}read", f.path("write")), {"path": f.path("write"), "write": true}]}))
        .unwrap();
    assert_eq!(r["agentFiles"], "folders");
    assert_eq!(r["agentFolders"], json!([{"path": f.path("read"), "write": false}, {"path": f.path("write"), "write": true}]));
    assert_eq!(r["opened"], false);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(f.0.join("outside"), f.0.join("read/link")).unwrap();
        let r = s.run("file.options", &json!({"agentFolders": [f.path("read/link")]})).unwrap();
        assert_eq!(r["agentFolders"], json!([{"path": f.path("outside"), "write": false}]), "a link is the folder it leads to");
    }
    let kept = s.prefs.clone();
    let long = format!("{}{}", f.path("read"), "x".repeat(5000));
    let many: Vec<String> = (0..33).map(|_| f.path("read")).collect();
    let bad = [
        json!({"agentFiles": "ask"}),
        json!({"agentFiles": 1}),
        json!({"agentFolders": "/"}),
        json!({"agentFolders": [7]}),
        json!({"agentFolders": [{"write": true}]}),
        json!({"agentFolders": [{"path": f.path("read"), "write": "yes"}]}),
        json!({"agentFolders": ["relative/folder"]}),
        json!({"agentFolders": [""]}),
        json!({"agentFolders": [long]}),
        json!({"agentFolders": [format!("{}\u{7}", f.path("read"))]}),
        json!({"agentFolders": [r"\\host\share"]}),
        json!({"agentFolders": ["//host/share"]}),
        json!({"agentFolders": [f.path("missing")]}),
        json!({"agentFolders": [f.path("read/list.csv")]}),
        json!({"agentFolders": [f.path("read"), f.path("read")]}),
        json!({"agentFolders": many}),
        // A bad one changes nothing, the good ones with it included.
        json!({"agentFiles": "all", "agentFolders": ["relative"]}),
    ];
    for v in bad {
        assert!(s.run("file.options", &v).is_err(), "{v}");
        assert_eq!(s.prefs, kept, "{v}");
    }
    // Junk never panics.
    for junk in [json!(null), json!(true), json!(-1), json!(1e300), json!("\u{0}"), json!([[[]]]), json!({"path": null}), json!([{"path": ["x"]}])] {
        let _ = s.run("file.options", &json!({"agentFiles": junk.clone(), "agentFolders": junk}));
    }
}

/// A folder agents use may be given from the home folder (`~/`), as File › Options › Agents
/// takes it; without a home folder, `~/` is no full path.
#[test]
fn a_folder_agents_use_may_start_in_the_home_folder() {
    let f = Folders::new("home");
    let sep = std::path::MAIN_SEPARATOR;
    let found = crate::cmd::options::agent_folder("agentFolders[0]", &format!("~/read{sep}"), Some(f.0.clone())).unwrap();
    assert_eq!(found, f.path("read"));
    let e = crate::cmd::options::agent_folder("agentFolders[0]", "~/read", None).unwrap_err();
    assert!(e.to_string().contains("isn't a full path"), "{e}");
    assert!(crate::cmd::options::agent_folder("agentFolders[0]", "~/missing", Some(f.0.clone())).is_err());
}

/// An agent's edit made again after a clash runs its command again: within the files the
/// setting allows now, also outside any request, as when the assistant's copy brings in the
/// person's edits before its turn (#31).
#[test]
fn an_edit_made_again_reads_a_file_only_while_the_setting_allows_it() {
    let f = Folders::new("again");
    std::fs::write(f.0.join("read/note.txt"), "from the file").unwrap();
    let note = f.path("read/note.txt");
    for files in ["folders", "none"] {
        let (mut win, mut own, room, lobby) = shared(Some(f.setting("folders")));
        let mut ed = agent(&room, &lobby, "Editor agent", Role::Editor, None);
        // The agent puts the file's text in place of "two", while the person types over it and
        // their change reaches the room first.
        room.reach(ed.id()).confine(|| {
            ed.run("select.text", &json!({"text": "two"})).unwrap();
            ed.run("insert.textFromFile", &json!({"path": note})).unwrap();
        });
        in_window(&mut own, &mut win, "select.text", json!({"text": "two"}));
        in_window(&mut own, &mut win, "text.insert", json!({"text": "2"}));
        own.lend(&mut win, Replica::send).unwrap();
        in_window(&mut own, &mut win, "file.options", json!({"agentFiles": files}));
        ed.receive();
        let text = ed.session().doc.final_text(wordcraft_doc::StoryRef::Body);
        if files == "none" {
            assert!(!text.contains("from the file"), "{text}");
            assert_eq!(ed.stats().lost, 1);
        } else {
            assert!(text.contains("from the file"), "made again while the folder is in reach: {text}");
            assert_eq!((ed.stats().made_again, ed.stats().lost), (1, 0));
        }
    }
}

/// An export is written once it is made, as a new file, and only if the copy still may write
/// there: the files it can use narrowing while it is made, or a file of that name put there
/// meanwhile (another participant's export of the same name), stop it.
#[test]
fn an_export_is_written_only_while_allowed_and_only_as_a_new_file() {
    let f = Folders::new("export-late");
    let (mut win, mut own, room, lobby) = shared(Some(f.setting("folders")));
    let ed = agent(&room, &lobby, "Editor agent", Role::Editor, None);
    // The reach the export took as it started; the person narrows it while the export is made.
    let late = f.path("write/late.pdf");
    let running = room.reach(ed.id());
    in_window(&mut own, &mut win, "file.options", json!({"agentFiles": "none"}));
    let e = running.confine(|| ed.session().write_export(&late, b"%PDF-")).unwrap_err();
    assert!(e.to_string().contains("changed while this request ran"), "{e}");
    assert!(!Path::new(&late).exists());
    // Another export of the same name got there first.
    in_window(&mut own, &mut win, "file.options", json!({"agentFiles": "folders"}));
    let first = f.path("write/first.pdf");
    room.reach(ed.id()).confine(|| ed.session().write_export(&first, b"one")).unwrap();
    let e = room.reach(ed.id()).confine(|| ed.session().write_export(&first, b"two")).unwrap_err();
    assert!(e.to_string().contains("is there already"), "{e}");
    assert_eq!(std::fs::read(&first).unwrap(), b"one");
    // Nor through a link put there, even one that leads nowhere yet.
    #[cfg(unix)]
    {
        let link = f.path("write/link.pdf");
        std::os::unix::fs::symlink(f.0.join("write/elsewhere.pdf"), &link).unwrap();
        assert!(room.reach(ed.id()).confine(|| ed.session().write_export(&link, b"x")).is_err());
        assert!(!f.0.join("write/elsewhere.pdf").exists());
        // Unconfined, a new file is still never written through one.
        let e = crate::io::write_new_file(&link, b"x").unwrap_err();
        assert!(e.contains("is there already"), "{e}");
        assert!(!f.0.join("write/elsewhere.pdf").exists());
    }
    // The window's own exports replace a file as before.
    win.run("file.exportPdf", &json!({"path": first})).unwrap();
    assert_ne!(std::fs::read(&first).unwrap(), b"one");
}

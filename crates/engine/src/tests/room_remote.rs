//! Invites for participants on other machines (#74): keys that stop working, keys that join a
//! number of times, remote invites (never the owner, no files unless named), exports handed
//! back as bytes, what `auth` says a key reaches, and wrong keys slowed down.

use std::time::Duration;

use serde_json::{Value, json};
use wordcraft_doc::Document;

use crate::Session;
use crate::agent_files::AgentFiles;
use crate::room::{Hub, Lobby, Replica, Role, Room, RoomError, Terms, hosted};

fn shared() -> (Session, Replica, std::sync::Arc<Room>, Lobby) {
    let mut win = Session::new(Document::from_text("one two three"));
    win.author = "Minoru".into();
    let (room, own) = Room::host(&mut win, "Minoru").unwrap();
    let lobby = Lobby::default();
    lobby.open(room.clone());
    (win, own, room, lobby)
}

fn execute(copy: &mut Replica, command: &str, params: Value) -> Result<Value, String> {
    hosted::call(copy, "engine.execute", &json!({"command": command, "params": params}))
}

fn base64_decode(s: &str) -> Vec<u8> {
    crate::cmd::insert::base64_decode(s).expect("base64")
}

#[test]
fn an_invite_s_key_stops_working_when_its_time_is_up() {
    let mut hub = Hub::new(Document::from_text("one"));
    let key = hub.invite_on("Ana", Role::Editor, Terms { expires: Some(1_000), ..Terms::default() }).unwrap();
    let forever = hub.invite("Ben", Role::Viewer).unwrap();
    let ana = hub.admit_with(&key).unwrap();
    assert!(!hub.expire(999), "not yet");
    assert!(hub.participant(ana.id).is_some());
    assert!(hub.expire(1_000), "due at its second");
    assert!(hub.participant(ana.id).is_none(), "the copy that joined is out, as when the key is taken back");
    assert!(matches!(hub.admit_with(&key), Err(RoomError::UnknownKey)));
    assert!(!hub.expire(5_000), "nothing else is due");
    // An invite without a time keeps working.
    assert!(hub.admit_with(&forever).is_ok());
    assert_eq!(hub.invites().iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["Ben"]);
}

#[test]
fn the_soonest_expiry_is_kept_across_invites() {
    let mut hub = Hub::new(Document::from_text("one"));
    let late = hub.invite_on("Late", Role::Viewer, Terms { expires: Some(2_000), ..Terms::default() }).unwrap();
    let soon = hub.invite_on("Soon", Role::Viewer, Terms { expires: Some(1_000), ..Terms::default() }).unwrap();
    assert!(hub.expire(1_500));
    assert!(matches!(hub.admit_with(&soon), Err(RoomError::UnknownKey)));
    assert!(hub.admit_with(&late).is_ok(), "the later one is still good");
    assert!(hub.expire(2_000));
    assert!(matches!(hub.admit_with(&late), Err(RoomError::UnknownKey)));
}

#[test]
fn a_room_takes_back_a_key_whose_time_is_up_and_its_copy_stops_answering() {
    let (_win, _own, room, lobby) = shared();
    // Already due: the next look at the room takes it back, before anyone joins with it.
    let past = room.invite_on("Past", Role::Editor, Terms { expires: Some(1), ..Terms::default() }).unwrap();
    assert!(matches!(lobby.join_with(&past), Err(RoomError::UnknownKey)));
    assert!(room.invites().iter().all(|i| i.name != "Past"));

    // Two seconds from now: the copy works, then stops once the time is up.
    let soon = crate::cmd::now_unix() + 2;
    let key = room.invite_on("Soon", Role::Editor, Terms { expires: Some(soon), ..Terms::default() }).unwrap();
    let mut copy = lobby.join_with(&key).unwrap();
    assert!(hosted::call(&mut copy, "room.participants", &json!({})).is_ok());
    while crate::cmd::now_unix() < soon {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(hosted::ended(&copy).is_some_and(|why| why.contains("Soon")), "out of the room");
    assert!(execute(&mut copy, "text.insert", json!({"text": "late "})).is_err());
    assert!(matches!(lobby.join_with(&key), Err(RoomError::UnknownKey)));
}

#[test]
fn a_key_joins_as_many_times_as_its_invite_allows() {
    let (_win, _own, room, lobby) = shared();
    let key = room.invite_on("Once", Role::Suggester, Terms { uses: Some(1), ..Terms::default() }).unwrap();
    assert_eq!(room.invites()[0].uses_left, Some(1));
    let mut first = lobby.join_with(&key).unwrap();
    assert_eq!(room.invites()[0].uses_left, Some(0));
    assert!(matches!(lobby.join_with(&key), Err(RoomError::NotAllowed(_))), "no second copy");
    // The copy that joined keeps working.
    execute(&mut first, "text.insert", json!({"text": "Hi "})).unwrap();
    assert!(hosted::ended(&first).is_none());
    // Zero joins is no invite at all.
    assert!(matches!(room.invite_on("Never", Role::Viewer, Terms { uses: Some(0), ..Terms::default() }), Err(RoomError::NotAllowed(_))));
}

#[test]
fn a_remote_participant_is_never_the_owner() {
    let (_win, _own, room, _lobby) = shared();
    let remote = Terms { remote: true, ..Terms::default() };
    assert!(matches!(room.invite_on("Sandbox", Role::Owner, remote), Err(RoomError::NotAllowed(_))));
    room.invite_on("Sandbox", Role::Editor, remote).unwrap();
    assert!(matches!(room.set_role("Sandbox", Role::Owner), Err(RoomError::NotAllowed(_))), "nor becomes it");
    assert_eq!(room.set_role("Sandbox", Role::Commenter).ok(), Some(true), "other roles change as for anyone");
    // A local invite still may be an owner.
    assert!(room.invite_on("Laptop", Role::Owner, Terms::default()).is_ok());
}

#[test]
fn a_remote_participant_reaches_no_file_unless_its_invite_names_some() {
    let (_win, _own, room, lobby) = shared();
    room.set_agent_files(AgentFiles::All, vec![]);
    let key = room.invite_on("Sandbox", Role::Editor, Terms { remote: true, ..Terms::default() }).unwrap();
    let copy = lobby.join_with(&key).unwrap();
    assert_eq!(room.files_for(copy.id()).0, AgentFiles::None, "the setting allows all, a remote invite none");
    let key = room.invite_on("Named", Role::Editor, Terms { remote: true, files: Some(AgentFiles::Folders), ..Terms::default() }).unwrap();
    let named = lobby.join_with(&key).unwrap();
    assert_eq!(room.files_for(named.id()).0, AgentFiles::Folders, "what the invite names, within the setting");
    // A local invite follows the setting.
    let key = room.invite_on("Local", Role::Editor, Terms::default()).unwrap();
    let local = lobby.join_with(&key).unwrap();
    assert_eq!(room.files_for(local.id()).0, AgentFiles::All);
    let listed = room.invites();
    let sandbox = listed.iter().find(|i| i.name == "Sandbox").unwrap();
    assert!(sandbox.remote);
    assert_eq!(sandbox.files, Some(AgentFiles::None));
    assert!(!listed.iter().find(|i| i.name == "Local").unwrap().remote);
}

#[test]
fn a_copy_gets_its_pdf_and_pages_back_as_bytes_without_a_path() {
    let (_win, _own, room, lobby) = shared();
    // No file at all: the bytes come back in the reply, for every role.
    let key = room.invite_on("Reader", Role::Viewer, Terms { remote: true, ..Terms::default() }).unwrap();
    let mut copy = lobby.join_with(&key).unwrap();
    let pdf = execute(&mut copy, "file.exportPdf", json!({})).unwrap();
    let bytes = base64_decode(pdf["pdf"].as_str().unwrap());
    assert!(bytes.starts_with(b"%PDF"), "a PDF");
    assert_eq!(pdf["bytes"], json!(bytes.len()));
    let png = execute(&mut copy, "file.exportPng", json!({"page": 1, "scale": 0.5})).unwrap();
    let bytes = base64_decode(png["png"].as_str().unwrap());
    assert!(bytes.starts_with(b"\x89PNG"), "a PNG");
    assert_eq!(png["pages"], json!(1));
    assert!(execute(&mut copy, "file.exportPng", json!({"page": 9})).is_err_and(|e| e.contains("no page 9")));
    // Printing still needs somewhere to print to.
    assert!(execute(&mut copy, "file.printPdf", json!({})).is_err_and(|e| e.contains("path")));
    // A file path is still held to the files the participant may use: none.
    let dir = std::env::temp_dir().join(format!("wordcraft-remote-{}", std::process::id()));
    let path = dir.join("out.pdf");
    assert!(execute(&mut copy, "file.exportPdf", json!({"path": path.to_string_lossy()})).is_err());
    assert!(!path.exists());
}

#[test]
fn auth_says_a_participant_s_copy_and_whose() {
    let (_win, _own, room, lobby) = shared();
    let key = room.invite_on("Copy editor", Role::Suggester, Terms { remote: true, ..Terms::default() }).unwrap();
    let copy = lobby.join_with(&key).unwrap();
    assert_eq!(hosted::auth_reply(&copy), json!({"as": "participant", "name": "Copy editor", "role": "suggester"}));
    // The role as it is now, though the copy hasn't received anything since it changed.
    room.set_role("Copy editor", Role::Viewer).unwrap();
    assert_eq!(hosted::auth_reply(&copy)["role"], "viewer");
}

#[test]
fn wrong_keys_slow_down_after_the_first_few_and_at_most_two_seconds() {
    let refusals = hosted::Refusals::default();
    let waits: Vec<Duration> = (0..30).map(|_| refusals.refused()).collect();
    assert_eq!(&waits[..3], &[Duration::ZERO; 3], "a stale key's retry doesn't wait");
    assert_eq!(waits[3], Duration::from_millis(100));
    assert_eq!(waits[4], Duration::from_millis(200));
    assert!(waits.windows(2).all(|w| w[0] <= w[1]));
    assert_eq!(waits[29], Duration::from_secs(2), "capped");
}

#[test]
fn hostile_invite_terms_never_panic() {
    let mut hub = Hub::new(Document::from_text("one"));
    for (n, terms) in [
        Terms { expires: Some(0), ..Terms::default() },
        Terms { expires: Some(u64::MAX), ..Terms::default() },
        Terms { uses: Some(u32::MAX), ..Terms::default() },
        Terms { remote: true, files: Some(AgentFiles::All), expires: Some(u64::MAX), uses: Some(1) },
    ]
    .into_iter()
    .enumerate()
    {
        let key = hub.invite_on(&format!("P{n}"), Role::Viewer, terms).unwrap();
        let _ = hub.admit_with(&key);
    }
    assert!(hub.expire(0), "the one due at 0");
    assert!(!hub.expire(u64::MAX - 1));
    assert!(hub.expire(u64::MAX));
}
